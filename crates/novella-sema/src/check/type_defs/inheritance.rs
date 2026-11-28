//! 继承关系、trait 实现、覆写与抽象成员。
use super::*;

/// 注册全部 class 的父类和 trait 关系，并检测继承环。
pub(crate) fn resolve_class_parents(sema: &mut Sema) {
    let class_keys: Vec<(ModuleId, String)> = sema
        .type_defs
        .iter()
        .filter(|(_, def)| matches!(def, TypeDef::Class(_) | TypeDef::Struct(_)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in class_keys {
        let (is_class, heritage) = match sema.type_defs.get(&key) {
            Some(TypeDef::Class(def)) => (true, def.trait_impls.clone()),
            Some(TypeDef::Struct(def)) => (false, def.heritage.clone()),
            _ => continue,
        };
        let env = type_env_for(sema, &key);
        let scope = sema.modules[key.0].global_scope;
        let mut parent = None;
        let mut traits = Vec::new();
        for expression in &heritage {
            let ty = sema.resolve_type(key.0, scope, expression, &env);
            let base = match &ty {
                Ty::Applied(base, _) => &**base,
                ty => ty,
            };
            match base {
                Ty::Class(..) if is_class => {
                    if parent.is_some() {
                        sema.decl_error(key.0, expression.span(), "class 只能有一个父类");
                    } else {
                        parent = Some(ty);
                    }
                }
                Ty::Trait(..) => {
                    if traits.iter().any(|prior| crate::ty::ty_eq(prior, &ty)) {
                        sema.decl_error(key.0, expression.span(), "重复实现同一 trait");
                    } else {
                        traits.push(ty);
                    }
                }
                other if other.is_opaque() => {}
                _ => sema.decl_error(
                    key.0,
                    expression.span(),
                    if is_class {
                        "继承列表只允许 class 或 trait"
                    } else {
                        "struct 不支持继承，父列表只允许 trait"
                    },
                ),
            }
        }
        if let Some(TypeDef::Class(current)) = sema.type_defs.get_mut(&key) {
            current.parent_resolved = parent.as_ref().map(crate::typecheck::this_ty_key);
            current.parent_type = parent;
            current.trait_impls_resolved =
                traits.iter().map(crate::typecheck::this_ty_key).collect();
            current.trait_types = traits;
        } else if let Some(TypeDef::Struct(current)) = sema.type_defs.get_mut(&key) {
            current.trait_types = traits;
        }
    }
    // 继承环检测：沿 parent_resolved 链走访，回到起点即成环
    let class_keys: Vec<(ModuleId, String)> = sema
        .type_defs
        .iter()
        .filter(|(_, def)| matches!(def, TypeDef::Class(_)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in class_keys {
        let mut visited = HashSet::new();
        let mut current = key.clone();
        loop {
            if !visited.insert(current.clone()) {
                // 类定义节点 span 未入注册表，环诊断退化到文件头
                let span = Span { start: 0, end: 0 };
                sema.decl_error(key.0, span, format!("class `{}` 的继承链存在循环", key.1));
                break;
            }
            match sema.type_defs.get(&current) {
                Some(TypeDef::Class(def)) => match &def.parent_resolved {
                    Some(parent) => current = parent.clone(),
                    None => break,
                },
                _ => break,
            }
        }
    }
}

/// 方法返回类型定型后，校验覆写和 trait 实现的签名兼容性。
pub(crate) fn check_inherited_signatures(sema: &mut Sema) {
    check_overrides(sema);
    super::conformance::check_trait_impls(sema);
}

/// 类型定义的名字环境与声明身份对应；方法形参可在其上遮蔽。
pub(super) fn type_env_for(sema: &Sema, key: &(ModuleId, String)) -> HashMap<String, Ty> {
    sema.type_defs
        .get(key)
        .map(|def| {
            def.type_params()
                .iter()
                .map(|id| {
                    (
                        id.split('@').next().unwrap_or(id).to_owned(),
                        Ty::TypeParam(id.clone()),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// trait 声明与实现的签名兼容：显式实参逐位相同，返回值保持同一表示。
pub(super) fn trait_signature_compatible(
    sema: &Sema,
    required: &FunctionSig,
    actual: &FunctionSig,
) -> bool {
    if required.type_params.len() != actual.type_params.len()
        || required.is_async != actual.is_async
    {
        return false;
    }
    let renaming = actual
        .type_params
        .iter()
        .cloned()
        .zip(required.type_params.iter().cloned().map(Ty::TypeParam))
        .collect();
    let Ty::Function(actual) = crate::ty::substitute_type_params(
        &Ty::Function(std::rc::Rc::new(actual.clone())),
        &renaming,
    ) else {
        unreachable!()
    };
    // `self` 或隐式 `this` 均为接收者，不参与对外显式参数列表。
    let params = |sig: &FunctionSig| {
        usize::from(sig.is_method && sig.params.first().is_some_and(|p| p.name == "self"))
    };
    let required_params = &required.params[params(required)..];
    let actual_params = &actual.params[params(&actual)..];
    required_params.len() == actual_params.len()
        && required_params
            .iter()
            .zip(actual_params)
            .all(|(want, have)| crate::ty::ty_eq(&want.ty, &have.ty))
        && covariant_return(sema, &required.ret, &actual.ret)
}

/// 派发不会执行任意隐式转换：仅允许同类型或同表示的 class 引用协变。
fn covariant_return(sema: &Sema, required: &Ty, actual: &Ty) -> bool {
    crate::ty::ty_eq(required, actual)
        || matches!(
            (
                crate::traits::nominal_base(required),
                crate::traits::nominal_base(actual)
            ),
            (Ty::Class(..), Ty::Class(..))
        ) && sema.is_subclass(required, actual)
}

/// D02 覆写校验：子类同名方法必须与父类签名一致（返回类型允许协变）。
///
/// 父类信息在 [`resolve_class_parents`] 中才补齐，故本检查必须排在其后。
/// 规则：
/// - 参数个数与类型必须**完全相同**（方法名相同即视为覆写，不允许「形似重载」）；
/// - 返回类型必须是父类返回类型的子类型（协变）或相同；
/// - 静态 / 实例属性必须一致（静态方法不能覆写实例方法）。
fn check_overrides(sema: &mut Sema) {
    let class_keys: Vec<(ModuleId, String)> = sema
        .type_defs
        .iter()
        .filter(|(_, def)| matches!(def, TypeDef::Class(_)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in class_keys {
        let Some(TypeDef::Class(def)) = sema.type_defs.get(&key) else {
            continue;
        };
        let Some(parent) = def.parent_resolved.clone() else {
            continue;
        };
        let methods: Vec<MethodSig> = def.methods.values().cloned().collect();
        let fields = def.fields.clone();
        for field in fields {
            // 遮蔽判定按「同类静态性」：实例字段只与实例成员冲突，静态字段只与静态成员冲突。
            if lookup_member_chain(sema, &parent, &field.name, field.is_static).is_some() {
                sema.decl_error(
                    key.0,
                    sema.type_defs
                        .get(&key)
                        .and_then(|d| match d {
                            TypeDef::Class(c) => Some(c.span),
                            _ => None,
                        })
                        .unwrap_or(Span { start: 0, end: 0 }),
                    format!("字段 `{}` 不能遮蔽继承成员", field.name),
                );
            }
        }
        for method in methods {
            if method.name.as_str() == "__init" {
                continue;
            }
            if matches!(
                lookup_member_chain(sema, &parent, &method.name, false).map(|h| h.info),
                Some(MemberInfo::Field(_))
            ) {
                sema.decl_error(
                    key.0,
                    method.span,
                    format!("方法 `{}` 不能遮蔽继承字段", method.name),
                );
                continue;
            }
            // 先按「同类静态性」查找方法；未命中说明父类同名成员是另一侧的静态性。
            let Some(hit) = lookup_member_chain(sema, &parent, &method.name, method.is_static)
            else {
                // 静态 / 实例不对应：仅当父类存在同静态性的方法时才报不一致。
                // （同静态性的字段已在上一段由遮蔽检查覆盖）
                let mismatched =
                    lookup_member_chain(sema, &parent, &method.name, !method.is_static)
                        .is_some_and(|other| matches!(other.info, MemberInfo::Method(_)));
                if mismatched {
                    sema.decl_error(
                        module_of(&key),
                        method.span,
                        format!(
                            "方法 `{}` 与父类 `{}::{}` 的静态 / 实例属性不一致，不能覆写",
                            method.name, parent.1, method.name
                        ),
                    );
                }
                continue;
            };
            let MemberInfo::Method(mut base) = hit.info else {
                continue;
            };
            let receiver = Ty::Class(key.0, key.1.clone());
            let mut subst = crate::ty::member_substitution(sema, &receiver, &hit.owner);
            subst.extend(
                base.sig
                    .type_params
                    .iter()
                    .cloned()
                    .zip(method.sig.type_params.iter().cloned().map(Ty::TypeParam)),
            );
            if let Ty::Function(sig) = crate::ty::substitute_type_params(
                &Ty::Function(std::rc::Rc::new(base.sig.clone())),
                &subst,
            ) {
                base.sig = (*sig).clone();
            }
            check_override_sig(sema, key.0, &method, &base, &parent);
        }
    }
}

/// 单个覆写方法的签名比对。
fn check_override_sig(
    sema: &mut Sema,
    module: ModuleId,
    method: &MethodSig,
    base: &MethodSig,
    parent_key: &(ModuleId, String),
) {
    if method.is_static != base.is_static
        || method.sig.is_async != base.sig.is_async
        || method.sig.type_params.len() != base.sig.type_params.len()
    {
        sema.decl_error(
            module,
            method.name_span,
            "覆写方法的 static、async 与泛型参数个数必须与父类一致",
        );
    }
    // 只看显式形参：实例方法的 index 0 是同名 `self`，两侧都已定型为各自属主类型
    let ours = &method.sig.params;
    let theirs = &base.sig.params;
    let name = &method.name;
    if ours.len() != theirs.len() {
        sema.decl_error(
            module,
            signature_span(method),
            format!(
                "方法 `{name}` 覆写父类同名方法时形参个数必须一致（子类 {} 个，父类 `{}::{name}` {} 个）",
                ours.len(),
                parent_key.1,
                theirs.len()
            ),
        );
        return;
    }
    for (index, (a, b)) in ours.iter().zip(theirs.iter()).enumerate() {
        // 首个 self 的类型天然不同（各自属主），跳过
        if index == 0 && a.name == "self" {
            continue;
        }
        if !crate::ty::ty_eq(&a.ty, &b.ty) {
            sema.decl_error(
                module,
                signature_span(method),
                format!(
                    "方法 `{name}` 覆写父类同名方法时第 {index} 个形参类型须为 `{}`，实为 `{}`",
                    b.ty, a.ty
                ),
            );
        }
    }
    if !covariant_return(sema, &base.sig.ret, &method.sig.ret) {
        sema.decl_error(
            module,
            signature_span(method),
            format!(
                "方法 `{name}` 覆写父类 `{}::{name}` 时返回类型 `{}` 不兼容父类的 `{}`",
                parent_key.1, method.sig.ret, base.sig.ret
            ),
        );
    }
    // 抽象性不在此判定：子类给出函数体即视为有效实现，不改变父类的抽象标记。
}

/// 方法的声明位置（覆写诊断定位）。
fn signature_span(method: &MethodSig) -> Span {
    method.span
}

/// D02 抽象类：含抽象方法（无函数体的 `func f(self) -> T;`）的类不可实例化。
pub(crate) fn is_abstract_class(sema: &Sema, key: &(ModuleId, String)) -> bool {
    let mut current = key.clone();
    let mut visited = HashSet::new();
    let mut methods = HashSet::new();
    loop {
        if !visited.insert(current.clone()) {
            return false;
        }
        match sema.type_defs.get(&current) {
            Some(TypeDef::Class(def)) => {
                if def.methods.iter().any(|(name, method)| {
                    // 每个名称以继承链上最近的声明为准，表键已经过特殊方法名规范化。
                    methods.insert(name) && method.is_abstract
                }) {
                    return true;
                }
                match &def.parent_resolved {
                    Some(parent) => current = parent.clone(),
                    None => return false,
                }
            }
            _ => return false,
        }
    }
}

/// 类型键的所属模块。
fn module_of(key: &(ModuleId, String)) -> ModuleId {
    key.0
}
