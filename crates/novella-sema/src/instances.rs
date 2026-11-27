//! 语义阶段已经定型的调用。HIR 使用这里的替换表，不再重新推导泛型。

use novella_ast::expr::Expression;
use novella_core::Span;
use std::collections::HashMap;

use crate::ty::{FunctionSig, Ty, member_substitution};
use crate::{FunctionView, ModuleId, Sema, TypeView};

/// 单个调用点的签名及实例化实参。泛型函数体中的值仍可引用外层形参。
#[derive(Debug, Clone)]
pub struct CallInstance {
    /// 调用点选中的声明与完成本次参数代入的调用签名。
    pub signature: FunctionView,
    /// 以声明身份为键的实例替换表（包括定义类型及方法自身的形参）。
    pub substitutions: Vec<(String, TypeView)>,
    /// 成员调用的静态接收者类型。受约束调用在实例化后据此寻找实现。
    pub receiver: Option<TypeView>,
    /// 若所选方法声明位于 trait，保留精确的应用 trait 以识别见证和 dyn 槽。
    pub trait_ref: Option<TypeView>,
}

/// 接收者可用的 trait 实现（包括继承实现和接收者覆写）；模板可引用宿主形式参数。
#[derive(Debug, Clone)]
pub struct TraitImplementation {
    pub receiver: TypeView,
    pub trait_ref: TypeView,
    pub bindings: Vec<(String, TypeView)>,
    pub methods: Vec<TraitMethodImplementation>,
}

/// 实现选择的精确方法声明，默认体的 `Self` 为实现接收者。
#[derive(Debug, Clone)]
pub struct TraitMethodImplementation {
    pub name: String,
    pub module: ModuleId,
    pub symbol: String,
    pub owner: TypeView,
    pub is_default: bool,
}

/// 语义阶段已验证的拥有型动态 trait 转换。
#[derive(Debug, Clone)]
pub struct DynConversion {
    pub source: TypeView,
    pub target: TypeView,
}

impl Sema {
    pub(crate) fn record_call_instance(
        &mut self,
        module: ModuleId,
        span: Span,
        callee: &Expression,
        signature: &FunctionSig,
        method_subst: &HashMap<String, Ty>,
    ) {
        let receiver = receiver_expression(callee)
            .and_then(|expr| self.expression_types.get(&(module, expr.span())));
        let (mut subst, trait_ref) = self.call_owner_substitution(module, callee, signature);
        subst.extend(
            method_subst
                .iter()
                .map(|(name, ty)| (name.clone(), ty.clone())),
        );
        let mut substitutions: Vec<_> = subst
            .into_iter()
            .map(|(name, ty)| (name, ty.view()))
            .collect();
        substitutions.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        let mut signature = signature.view();
        signature.type_param_count = 0;
        self.call_instances.insert(
            (module, span),
            CallInstance {
                signature,
                substitutions,
                receiver: receiver.map(Ty::view),
                trait_ref: trait_ref.as_ref().map(Ty::view),
            },
        );
    }

    /// 通过语义选定的方法符号找到具体的属主应用，而非仅按方法名挑选约束。
    pub(crate) fn call_owner_substitution(
        &self,
        module: ModuleId,
        callee: &Expression,
        sig: &FunctionSig,
    ) -> (HashMap<String, Ty>, Option<Ty>) {
        let receiver = receiver_expression(callee)
            .and_then(|expr| self.expression_types.get(&(module, expr.span())));
        let Some((receiver, (owner, _))) =
            receiver.zip(sig.name.as_ref().and_then(|name| name.split_once("::")))
        else {
            return (HashMap::new(), None);
        };
        let receiver = match receiver {
            Ty::Pointer(inner) => inner,
            other => other,
        };
        let key = (sig.module, owner.to_owned());
        let trait_ref = self
            .implemented_traits(receiver)
            .into_iter()
            .find(|t| crate::typecheck::this_ty_key(t) == key);
        (member_substitution(self, receiver, &key), trait_ref)
    }
}

pub(crate) fn receiver_expression(callee: &Expression) -> Option<&Expression> {
    let mut current = callee;
    loop {
        match current {
            Expression::Paren { expr, .. } => current = expr,
            Expression::MemberAccess { target, .. } => return Some(target),
            _ => return None,
        }
    }
}
