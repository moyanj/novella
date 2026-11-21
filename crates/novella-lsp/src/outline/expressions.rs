//! 表达式遍历：嵌套结构（lambda、match、new 等）里的符号与折叠区间。

use novella_ast::expr::{
    Expression, LambdaBody, LambdaParams, MatchCaseBody, MatchPattern, PropertyAssignment,
};

use super::{FoldKind, Walker};

impl Walker<'_> {
    // ---------- 表达式 ----------

    pub(crate) fn expression(&mut self, expression: &Expression, parent: usize) {
        match expression {
            Expression::Literal { .. }
            | Expression::This { .. }
            | Expression::Identifier { .. } => {}
            Expression::Paren { expr, .. } | Expression::Await { expr, .. } => {
                self.expression(expr, parent)
            }
            Expression::Unary { operand, .. } => self.expression(operand, parent),
            Expression::Tuple { elements, .. } | Expression::ArrayLiteral { elements, .. } => {
                for element in elements {
                    self.expression(element, parent);
                }
            }
            Expression::ObjectLiteral { properties, .. }
            | Expression::StructInit { properties, .. } => {
                for property in properties {
                    self.property(property, parent);
                }
            }
            Expression::Binary { left, right, .. } => {
                self.expression(left, parent);
                self.expression(right, parent);
            }
            Expression::Is { operand, ty, .. }
            | Expression::Cast {
                expr: operand, ty, ..
            } => {
                self.expression(operand, parent);
                self.type_expr(ty, parent);
            }
            Expression::Ternary {
                condition,
                then_expr,
                else_expr,
                ..
            } => {
                self.expression(condition, parent);
                self.expression(then_expr, parent);
                self.expression(else_expr, parent);
            }
            Expression::Assignment { target, value, .. } => {
                self.expression(target, parent);
                self.expression(value, parent);
            }
            Expression::MemberAccess { target, .. } => self.expression(target, parent),
            Expression::IndexAccess { target, index, .. } => {
                self.expression(target, parent);
                self.expression(index, parent);
            }
            Expression::Call {
                callee,
                type_args,
                args,
                ..
            }
            | Expression::New {
                target: callee,
                type_args,
                args,
                ..
            } => {
                self.expression(callee, parent);
                for argument in args {
                    self.expression(argument, parent);
                }
                for type_arg in type_args {
                    self.type_expr(type_arg, parent);
                }
            }
            Expression::Match(match_expression) => {
                self.expression(&match_expression.subject, parent);
                for case in &match_expression.cases {
                    match &case.pattern {
                        MatchPattern::Expr { expr, .. } => self.expression(expr, parent),
                        MatchPattern::Type { ty, .. } => self.type_expr(ty, parent),
                    }
                    self.match_case_body(&case.body, parent);
                }
                if let Some(default) = &match_expression.default {
                    self.match_case_body(default, parent);
                }
                self.fold(match_expression.span, FoldKind::Match);
            }
            Expression::Lambda(lambda) => {
                if let LambdaParams::List { params, .. } = &lambda.params {
                    for parameter in params {
                        self.param(parameter, parent);
                    }
                }
                if let Some(return_type) = &lambda.return_type {
                    self.type_expr(return_type, parent);
                }
                match &lambda.body {
                    LambdaBody::Expr { expr, .. } => self.expression(expr, parent),
                    LambdaBody::Block { block, .. } => self.block(block, Some(parent)),
                }
            }
        }
    }

    pub(crate) fn property(&mut self, property: &PropertyAssignment, parent: usize) {
        if let Some(value) = &property.value {
            self.expression(value, parent);
        }
    }

    pub(crate) fn match_case_body(&mut self, body: &MatchCaseBody, parent: usize) {
        match body {
            MatchCaseBody::Expr { expr, .. } => self.expression(expr, parent),
            MatchCaseBody::Block { block, .. } => self.block(block, Some(parent)),
        }
    }
}
