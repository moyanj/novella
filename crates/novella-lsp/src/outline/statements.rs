//! 语句遍历：块、控制流、变量声明、绑定模式、catch / with。

use novella_ast::stmt::{
    BindingPattern, Block, CatchClause, ForInit, ForKind, IfBranch, IfStmt, Statement,
    VariableStmt, WithBinding,
};

use super::{FoldKind, OutlineKind, Walker, type_expr_text};

impl Walker<'_> {
    // ---------- 语句 ----------

    pub(crate) fn block(&mut self, block: &Block, parent: Option<usize>) {
        for statement in &block.stmts {
            self.statement(statement, parent);
        }
        self.fold(block.span, FoldKind::Block);
    }

    pub(crate) fn statement(&mut self, statement: &Statement, parent: Option<usize>) {
        match statement {
            Statement::Variable(variable) => self.variable(variable, parent),
            Statement::VariableList { declarations, .. } => {
                for declaration in declarations {
                    self.variable(declaration, parent);
                }
            }
            Statement::Empty { .. } | Statement::Break { .. } | Statement::Continue { .. } => {}
            Statement::Return { value, .. } | Statement::Yield { value, .. } => {
                if let (Some(index), Some(value)) = (parent, value) {
                    self.expression(value, index);
                }
            }
            Statement::Throw { expr, .. } => {
                if let (Some(index), Some(expr)) = (parent, expr) {
                    self.expression(expr, index);
                }
            }
            Statement::Block(block) => self.block(block, parent),
            Statement::If(if_statement) => self.if_statement(if_statement, parent),
            Statement::While {
                condition,
                body,
                span,
            } => {
                if let Some(index) = parent {
                    self.expression(condition, index);
                }
                self.statement(body, parent);
                self.fold(*span, FoldKind::Block);
            }
            Statement::For(for_statement) => {
                if let Some(index) = parent {
                    self.for_kind(&for_statement.condition, index);
                }
                self.statement(&for_statement.block, parent);
                self.fold(for_statement.span, FoldKind::Conditional);
            }
            Statement::Try(try_statement) => {
                if let Some(index) = parent {
                    for catch in &try_statement.catches {
                        self.catch_clause(catch, index);
                    }
                }
                self.block(&try_statement.block, parent);
                if let Some(finally) = &try_statement.finally {
                    self.block(finally, parent);
                }
                self.fold(try_statement.span, FoldKind::TryBlock);
            }
            Statement::With(with_statement) => {
                if let Some(index) = parent {
                    self.expression(&with_statement.expr, index);
                    if let Some(binding) = &with_statement.binding {
                        self.with_binding(binding, index);
                    }
                }
                self.block(&with_statement.block, parent);
                self.fold(with_statement.span, FoldKind::Block);
            }
            Statement::Unsafe { block, span } => {
                self.block(block, parent);
                self.fold(*span, FoldKind::Block);
            }
            Statement::Expression(expression) => {
                if let Some(index) = parent {
                    self.expression(expression, index);
                }
            }
        }
    }

    pub(crate) fn for_kind(&mut self, kind: &ForKind, parent: usize) {
        match kind {
            ForKind::ForIn(for_in) => {
                self.binding_pattern(&for_in.left, parent, None);
                self.expression(&for_in.right, parent);
            }
            ForKind::ForLoop(for_loop) => {
                match &for_loop.init {
                    Some(ForInit::Declaration { declarations, .. }) => {
                        for declaration in declarations {
                            self.variable(declaration, Some(parent));
                        }
                    }
                    Some(ForInit::Expression { expr, .. }) => self.expression(expr, parent),
                    None => {}
                }
                if let Some(condition) = &for_loop.condition {
                    self.expression(condition, parent);
                }
                if let Some(update) = &for_loop.update {
                    self.expression(update, parent);
                }
            }
        }
    }

    pub(crate) fn variable(&mut self, variable: &VariableStmt, parent: Option<usize>) {
        let Some(index) = parent else {
            return;
        };
        let detail = variable.ty.as_ref().map(type_expr_text);
        self.binding_pattern(&variable.pattern, index, detail);
        if let Some(ty) = &variable.ty {
            self.type_expr(ty, index);
        }
        if let Some(value) = &variable.value {
            self.expression(value, index);
        }
    }

    pub(crate) fn binding_pattern(
        &mut self,
        pattern: &BindingPattern,
        parent: usize,
        detail: Option<String>,
    ) {
        match pattern {
            BindingPattern::Ident { name, span } => {
                self.push(name, OutlineKind::Variable, *span, Some(parent), detail);
            }
            BindingPattern::Tuple { elements, .. } => {
                for element in elements {
                    self.binding_pattern(element, parent, None);
                }
            }
        }
    }

    pub(crate) fn catch_clause(&mut self, catch: &CatchClause, parent: usize) {
        let detail = catch.ty.as_ref().map(type_expr_text);
        let index = self.push(
            &catch.name,
            OutlineKind::Variable,
            catch.span,
            Some(parent),
            detail,
        );
        if let Some(ty) = &catch.ty {
            self.type_expr(ty, index);
        }
        self.block(&catch.block, Some(index));
    }

    pub(crate) fn with_binding(&mut self, binding: &WithBinding, parent: usize) {
        let detail = binding.ty.as_ref().map(type_expr_text);
        self.binding_pattern(&binding.pattern, parent, detail);
        if let Some(ty) = &binding.ty {
            self.type_expr(ty, parent);
        }
    }

    pub(crate) fn if_statement(&mut self, if_statement: &IfStmt, parent: Option<usize>) {
        // if 链整体作为一条「多行条件块」折叠，不在每个分支重复折叠
        for branch in &if_statement.branches {
            match branch {
                IfBranch::ElseIf {
                    condition, block, ..
                } => {
                    if let Some(index) = parent {
                        self.expression(condition, index);
                    }
                    self.statement(block, parent);
                }
                IfBranch::Else { block, .. } => self.statement(block, parent),
            }
        }
        self.fold(if_statement.span, FoldKind::Conditional);
    }
}
