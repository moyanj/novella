//! 类型相等与可赋值关系。
use super::*;

/// 结构化判等（`Function` 变体比较签名内容而非 `Rc` 指针）。
pub(crate) fn ty_eq(a: &Ty, b: &Ty) -> bool {
    match (a, b) {
        (Ty::Unknown, Ty::Unknown)
        | (Ty::Void, Ty::Void)
        | (Ty::Null, Ty::Null)
        | (Ty::Boolean, Ty::Boolean)
        // 同为未定型字面量即同类（数值差异由赋值点的范围判定处理）
        | (Ty::IntLiteral(..), Ty::IntLiteral(..))
        | (Ty::FloatLiteral(_), Ty::FloatLiteral(_)) => true,
        (Ty::Int(x), Ty::Int(y)) => x == y,
        (Ty::Float(x), Ty::Float(y)) => x == y,
        (Ty::Applied(x, a), Ty::Applied(y, b)) => ty_eq(x, y) && a.len() == b.len() && a.iter().zip(b).all(|(a,b)| ty_eq(a,b)),
        (Ty::Future(x), Ty::Future(y)) => ty_eq(x,y),
        (Ty::Object(x), Ty::Object(y)) => x.len() == y.len() && x.iter().all(|(n,t)| y.get(n).is_some_and(|s| ty_eq(t,s))),
        (Ty::Pointer(x), Ty::Pointer(y)) => ty_eq(x, y),
        (Ty::Array(x), Ty::Array(y)) => ty_eq(x, y),
        (Ty::Tuple(x), Ty::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| ty_eq(a, b))
        }
        (Ty::Function(x), Ty::Function(y)) => sig_eq(x, y),
        (Ty::Union(x), Ty::Union(y)) => {
            x.len() == y.len() && x.iter().all(|m| y.iter().any(|n| ty_eq(m, n)))
        }
        (Ty::Struct(m1, n1), Ty::Struct(m2, n2))
        | (Ty::Class(m1, n1), Ty::Class(m2, n2))
        | (Ty::Trait(m1, n1), Ty::Trait(m2, n2)) => m1 == m2 && n1 == n2,
        (Ty::TypeParam(x), Ty::TypeParam(y)) => x == y,
        (Ty::Error, Ty::Error) => true,
        _ => false,
    }
}

/// 函数签名判等（忽略 async / extern 标记）。
pub(crate) fn sig_eq(a: &FunctionSig, b: &FunctionSig) -> bool {
    a.params.len() == b.params.len()
        && a.params
            .iter()
            .zip(&b.params)
            .all(|(x, y)| ty_eq(&x.ty, &y.ty))
        && ty_eq(&a.ret, &b.ret)
}

/// `target ← source` 可赋值性（数值只放行同符号无损扩宽；`sema` 需继承链信息，
/// class 父子判定由调用方通过 `is_subclass` 闭包注入）。
pub(crate) fn assignable(
    target: &Ty,
    source: &Ty,
    is_subclass: &mut dyn FnMut(&Ty, &Ty) -> bool,
    width: crate::PointerWidth,
) -> bool {
    // 任一侧不透明：放行（诊断已在产生 Unknown 时报出）
    if target.is_opaque() || source.is_opaque() {
        return true;
    }
    if ty_eq(target, source) {
        return true;
    }
    match (target, source) {
        // 未定型字面量归化到具体数值类型；整数还须落在目标位宽范围内（L02–L04）。
        // 这里只按正数上限判定：负数下限由一元 `-` 在取符号处判定（L04）。
        // 这里只按正数上限判定；负数下限由取负态 + [`int_negated_literal_fits`] 处理
        (Ty::Int(kind), Ty::IntLiteral(false, value)) => {
            int_literal_fits_non_negative(target_int_kind(*kind, width), *value)
        }
        (Ty::Int(kind), Ty::IntLiteral(true, value)) => {
            int_negated_literal_fits(target_int_kind(*kind, width), *value)
        }
        // 同符号整数的无损扩宽（design.md §5.2.1）：`i32 → i64`、`u8 → u16`。
        // 目标为平台宽度整数时先按平台位宽折算，再判同符号 + 位宽更大；
        // 源为 `usize` / `isize` 时其位宽随目标平台，折算按 32 位最窄口径（保守）。
        // 窄化与跨符号仍是错误，必须显式 `as`。
        (Ty::Int(target), Ty::Int(source)) => {
            let target_width = target_int_kind(*target, width);
            let source_width = target_int_kind(*source, width);
            source_width.widens_to(target_width)
        }
        // 浮点字面量：目标类型能有限表示即可（L05–L06）
        (Ty::Float(kind), Ty::FloatLiteral(value)) => float_literal_fits(*kind, *value),
        // null → 任意指针 / class 引用（design.md §6.5 S5）
        (Ty::Pointer(_), Ty::Null) | (Ty::Class(..), Ty::Null) => true,
        (Ty::Applied(base, _), Ty::Null) if matches!(**base, Ty::Class(..)) => true,
        // void* ↔ 任意指针（design.md §6.5 S6）
        (Ty::Pointer(t), Ty::Pointer(s)) if matches!(**t, Ty::Void) || matches!(**s, Ty::Void) => {
            true
        }
        // 子类引用 → 父类引用
        (Ty::Class(..), Ty::Class(..)) => is_subclass(target, source),
        (Ty::Trait(..), _) => is_subclass(target, source),
        (Ty::Applied(..), Ty::Applied(..))
        | (Ty::Applied(..), Ty::Class(..))
        | (Ty::Class(..), Ty::Applied(..)) => is_subclass(target, source),
        // 联合目标：source 可赋给任一成员即可
        (Ty::Union(members), _) => members
            .iter()
            .any(|m| assignable(m, source, is_subclass, width)),
        // 联合源：全部成员都可赋给目标才可赋（窄化前的保守规则）
        (_, Ty::Union(members)) => members
            .iter()
            .all(|m| assignable(target, m, is_subclass, width)),
        // 元组 / 数组：同形后逐元素可赋（字面量归化等规则随递归生效）
        (Ty::Tuple(t), Ty::Tuple(s)) if t.len() == s.len() => t
            .iter()
            .zip(s)
            .all(|(t, s)| assignable(t, s, is_subclass, width)),
        (Ty::Array(t), Ty::Array(s)) => assignable(t, s, is_subclass, width),
        (Ty::Object(t), Ty::Object(s)) => t.iter().all(|(name, ty)| {
            s.get(name)
                .is_some_and(|source| assignable(ty, source, is_subclass, width))
        }),
        _ => false,
    }
}
