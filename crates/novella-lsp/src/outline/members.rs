//! 成员与类型遍历：字段、方法、类型形参、参数、类型表达式。

use novella_ast::{
    program::{ClassMember, FieldMember, MethodMember},
    types::{FunctionType, Param, TypeExpr, TypeParameter},
};

use super::name::signature_text;
use super::{FoldKind, OutlineKind, Walker, type_expr_text};

impl Walker<'_> {
    /// 类型成员的统一入口：字段与方法各走各自的收集函数。
    pub(crate) fn members(&mut self, members: &[ClassMember], parent: usize) {
        for member in members {
            match member {
                ClassMember::Field(field) => self.field(field, parent),
                ClassMember::Method(method) => self.method(method, parent),
            }
        }
    }

    pub(crate) fn field(&mut self, field: &FieldMember, parent: usize) {
        let detail = field.ty.as_ref().map(type_expr_text);
        let index = self.push(
            &field.name,
            OutlineKind::Field,
            field.span,
            Some(parent),
            detail,
        );
        if let Some(ty) = &field.ty {
            self.type_expr(ty, index);
        }
        if let Some(initializer) = &field.initializer {
            self.expression(initializer, index);
        }
    }

    pub(crate) fn method(&mut self, method: &MethodMember, parent: usize) {
        let detail = signature_text(Some(&method.params), method.return_type.as_ref());
        let index = self.push(
            &method.name,
            OutlineKind::Method,
            method.span,
            Some(parent),
            detail,
        );
        for parameter in &method.type_parameters {
            self.type_parameter(parameter, Some(index));
        }
        for parameter in &method.params {
            self.param(parameter, index);
        }
        if let Some(return_type) = &method.return_type {
            self.type_expr(return_type, index);
        }
        if let Some(block) = &method.block {
            self.block(block, Some(index));
        }
        self.fold(method.span, FoldKind::FunctionBody);
    }

    pub(crate) fn type_parameter(&mut self, parameter: &TypeParameter, parent: Option<usize>) {
        let index = self.push(
            &parameter.name,
            OutlineKind::TypeParameter,
            parameter.span,
            parent,
            None,
        );
        if let Some(bound) = &parameter.bound {
            self.type_expr(bound, index);
        }
    }

    pub(crate) fn param(&mut self, param: &Param, parent: usize) {
        let detail = param.ty.as_ref().map(type_expr_text);
        let index = self.push(
            &param.name,
            OutlineKind::Parameter,
            param.span,
            Some(parent),
            detail,
        );
        if let Some(ty) = &param.ty {
            self.type_expr(ty, index);
        }
        if let Some(default) = &param.default {
            self.expression(default, index);
        }
    }

    pub(crate) fn function_type(&mut self, function_type: &FunctionType, parent: Option<usize>) {
        for parameter in &function_type.type_parameters {
            self.type_parameter(parameter, parent);
        }
        if let Some(parent) = parent {
            for parameter in &function_type.params {
                self.param(parameter, parent);
            }
            if let Some(return_type) = &function_type.return_type {
                self.type_expr(return_type, parent);
            }
        }
    }
    // ---------- 类型表达式 ----------

    pub(crate) fn type_expr(&mut self, ty: &TypeExpr, parent: usize) {
        match ty {
            TypeExpr::Predefined { .. } => {}
            TypeExpr::Reference { generic_args, .. } => {
                for argument in generic_args {
                    self.type_expr(argument, parent);
                }
            }
            TypeExpr::Array { element, .. } | TypeExpr::Pointer { element, .. } => {
                self.type_expr(element, parent)
            }
            TypeExpr::Tuple { elements, .. }
            | TypeExpr::Union {
                types: elements, ..
            } => {
                for element in elements {
                    self.type_expr(element, parent);
                }
            }
            TypeExpr::Function(function_type) => self.function_type(function_type, Some(parent)),
        }
    }
}
