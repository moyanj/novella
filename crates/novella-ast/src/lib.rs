//! 重新导出 derive 宏，供各模块统一以 `use crate::Spanned;` 引入。
pub use novella_derive::Spanned;

pub mod expr;
pub mod program;
pub mod stmt;
pub mod types;

#[cfg(test)]
mod tests {
    use novella_core::Span;

    use crate::{
        expr::{Expression, Lambda, LambdaBody, LambdaParams},
        stmt::Block,
    };

    /// 验证 `Spanned` 派生：具名字段变体取 span 字段，newtype 变体委托内部节点。
    #[test]
    fn spanned_派生生成_span_方法() {
        let ident = Expression::Identifier {
            name: "x".into(),
            span: Span { start: 1, end: 2 },
        };
        assert_eq!(ident.span(), Span { start: 1, end: 2 });

        // newtype 变体（Lambda）：span 委托给内部节点
        let lambda = Expression::Lambda(Lambda {
            is_async: false,
            params: LambdaParams::Single {
                name: "a".into(),
                span: Span { start: 0, end: 1 },
            },
            return_type: None,
            body: LambdaBody::Expr {
                expr: Box::new(ident),
                span: Span { start: 5, end: 6 },
            },
            span: Span { start: 0, end: 6 },
        });
        assert_eq!(lambda.span(), Span { start: 0, end: 6 });

        // struct 节点：直接取字段
        let block = Block {
            stmts: vec![],
            span: Span { start: 2, end: 4 },
        };
        assert_eq!(block.span(), Span { start: 2, end: 4 });
    }
}
