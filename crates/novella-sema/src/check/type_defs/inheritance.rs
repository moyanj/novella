//! 继承关系、trait 实现、覆写与抽象成员。
use super::*;

/// 注册全部 class 的父类和 trait 关系，并检测继承环。
pub(crate) fn resolve_class_parents(sema: &mut Sema) {
    let class_keys: Vec<(ModuleId, String)> = sema
        .type_defs
        .iter()
        .filter(|(_, def)| matches!(def, TypeDef::Class(_)))
        .map(|(key, _)| key.clone())
        .collect();
    for key in class_keys {
        let Some(TypeDef::Class(def)) = sema.type_defs.get(&key).cloned() else {
            continue;
        };
        let env = type_env_for(sema, &key);
        let scope = sema.modules[key.0].global_scope;
        let mut parent = None;
        let mut traits = Vec::new();
        for expression in &def.trait_impls {
            let ty = sema.resolve_type(key.0, scope, expression, &env);
            let base = match &ty {
                Ty::Applied(base, _) => &**base,
                ty => ty,
            };
            match base {
                Ty::Class(..) => {
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
                _ => sema.decl_error(key.0, expression.span(), "继承列表只允许 class 或 trait"),
            }
        }
        if let Some(TypeDef::Class(current)) = sema.type_defs.get_mut(&key) {
            current.parent_resolved = parent.as_ref().map(crate::typecheck::this_ty_key);
            current.parent_type = parent;
            current.trait_impls_resolved =
                traits.iter().map(crate::typecheck::this_ty_key).collect();
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
    check_trait_impls(sema);
}

/// D04：校验 class 声明的 trait 实现。
///
/// 语法复用 `classHeritage`：`class C (Parent, Trait1, Trait2)` 中首项是父类，
/// 其余是 trait。要求：
/// - 列表里除首项外必须是 trait 类型（写成别的类型报错）；
/// - trait 的每个方法都必须在类（含继承链）中有**兼容实现**：形参类型完全一致、
///   返回类型可赋给 trait 的返回类型（协变）、且不是抽象方法。
fn check_trait_impls(sema: &mut Sema) {
    let mut tasks = Vec::new();
    for (key, def) in &sema.type_defs {
        if let TypeDef::Class(def) = def {
            for ty in &def.trait_types {
                tasks.push((key.clone(), ty.clone()));
            }
        }
    }
    for (class, ty) in tasks {
        check_trait_impl(sema, &class, ty);
    }
}

/// 类型定义的名字环境与声明身份对应；方法形参可在其上遮蔽。
fn type_env_for(sema: &Sema, key: &(ModuleId, String)) -> HashMap<String, Ty> {
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

/// 单个 (class, trait) 的一致性检查：trait 的每个方法都必须被实现。
fn check_trait_impl(sema: &mut Sema, class: &(ModuleId, String), trait_ty: Ty) {
    let trait_key = crate::typecheck::this_ty_key(&trait_ty);
    let subst = crate::ty::instance_substitution(sema, &trait_ty);
    let Some(TypeDef::Trait(def)) = sema.type_defs.get(&trait_key).cloned() else {
        return;
    };
    let class_span = match sema.type_defs.get(class) {
        Some(TypeDef::Class(def)) => def.span,
        _ => Span { start: 0, end: 0 },
    };
    for (name, required) in def.methods.iter() {
        let found = find_implementation(sema, class, name, required.is_static);
        match found {
            None => sema.decl_error(
                class.0,
                class_span,
                format!(
                    "class `{}` 声明实现 trait `{}`，但缺少方法 `{name}` 的实现",
                    class.1, def.name
                ),
            ),
            Some(implemented) => {
                let required_ty = crate::ty::substitute_type_params(
                    &Ty::Function(std::rc::Rc::new(required.sig.clone())),
                    &subst,
                );
                let Ty::Function(required_sig) = required_ty else {
                    unreachable!()
                };
                if !trait_signature_compatible(sema, &required_sig, &implemented.sig) {
                    sema.decl_error(
                        class.0,
                        implemented.span,
                        format!(
                            "方法 `{name}` 的实现与 trait `{}` 声明的签名不一致",
                            def.name
                        ),
                    );
                }
            }
        }
    }
}

/// 查找继承链中生效的方法，抽象声明会遮蔽父类的实现。
fn find_implementation(
    sema: &Sema,
    class: &(ModuleId, String),
    name: &str,
    want_static: bool,
) -> Option<MethodSig> {
    let mut current = Some(class.clone());
    let mut visited = HashSet::new();
    while let Some(k) = current {
        if !visited.insert(k.clone()) {
            return None;
        }
        let Some(TypeDef::Class(def)) = sema.type_defs.get(&k) else {
            return None;
        };
        if let Some(method) = def.methods.get(crate::check::canonical_method_name(name)) {
            if method.is_abstract || method.is_static != want_static {
                return None;
            }
            let mut resolved = method.clone();
            let receiver = Ty::Class(class.0, class.1.clone());
            let subst = crate::ty::member_substitution(sema, &receiver, &k);
            if let Ty::Function(sig) = crate::ty::substitute_type_params(
                &Ty::Function(std::rc::Rc::new(resolved.sig)),
                &subst,
            ) {
                resolved.sig = (*sig).clone();
            } else {
                unreachable!();
            }
            return Some(resolved);
        }
        current = def.parent_resolved.clone();
    }
    None
}

/// trait 声明与实现的签名兼容：实参逐位相同、返回类型可赋给 trait 的声明。
fn trait_signature_compatible(sema: &Sema, required: &FunctionSig, actual: &FunctionSig) -> bool {
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
    if required.params.len() != actual.params.len() {
        return false;
    }
    for (want, have) in required.params.iter().zip(actual.params.iter()) {
        // 首个 self 在两侧分别是 trait 与 class 的属主，不参与比较
        if want.name == "self" {
            continue;
        }
        if !crate::ty::ty_eq(&want.ty, &have.ty) {
            return false;
        }
    }
    // 返回类型协变
    sema.is_assignable(&required.ret, &actual.ret)
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
            if lookup_member_chain(sema, &parent, &field.name, false).is_some() {
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
            if crate::check::canonical_method_name(&method.name) == "__init" {
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
            // 先按「同类静态性」签名查找；静态方法还需确认父类同名成员不是实例方法
            let Some(hit) = lookup_member_chain(sema, &parent, &method.name, method.is_static)
            else {
                // 静态 / 实例不对应的情况：再用另一侧查一次
                if let Some(other) =
                    lookup_member_chain(sema, &parent, &method.name, !method.is_static)
                    && let MemberInfo::Method(base) = other.info
                {
                    sema.decl_error(
                        module_of(&key),
                        method.span,
                        format!(
                            "方法 `{}` 与父类 `{}::{}` 的静态 / 实例属性不一致，不能覆写",
                            method.name, parent.1, method.name
                        ),
                    );
                    let _ = base;
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
            check_override_sig(sema, key.0, &key.1, &method, &base, &parent);
        }
    }
}

/// 单个覆写方法的签名比对。
fn check_override_sig(
    sema: &mut Sema,
    module: ModuleId,
    class_name: &str,
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
    // 返回类型协变：子类返回可赋给父类返回类型即可
    if !sema.is_assignable(&base.sig.ret, &method.sig.ret) {
        sema.decl_error(
            module,
            signature_span(method),
            format!(
                "方法 `{name}` 覆写父类 `{}::{name}` 时返回类型 `{}` 不兼容父类的 `{}`",
                parent_key.1, method.sig.ret, base.sig.ret
            ),
        );
    }
    if base.is_abstract {
        // 已实现（有函数体）即为有效实现；此处仅在声明但父类也是抽象时保持抽象
        let _ = class_name;
    }
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
