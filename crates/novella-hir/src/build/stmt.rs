//! HIR 降级：stmt 职责实现。

use super::*;

impl<'a> BodyBuilder<'a> {
    // ---------- 语句 ----------

    /// 降级一条语句到新块。
    pub(super) fn build_statement(
        &mut self,
        statement: &Statement,
    ) -> Result<HirBlock, BuildError> {
        let mut block = HirBlock::new();
        self.emit_statement(statement, &mut block)?;
        Ok(block)
    }

    /// 把一条语句发射进既有块。
    pub(super) fn emit_statement(
        &mut self,
        statement: &Statement,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        match statement {
            Statement::Variable(variable) => self.emit_variable(variable, block)?,
            Statement::VariableList { declarations, .. } => {
                for declaration in declarations {
                    self.emit_variable(declaration, block)?;
                }
            }
            Statement::Empty { .. } => block.push(HirStmt::Nop),
            Statement::Return { value, .. } => {
                let operand = match value {
                    Some(expr) => {
                        let ty = self.return_type.clone().ok_or_else(|| {
                            BuildError::at(BuildErrorKind::MissingType {
                                what: "返回类型"
                            })
                            .with_site(self.module, expr.span())
                        })?;
                        Some(self.build_typed_expression(expr, &ty, block)?)
                    }
                    None => None,
                };
                block.push(HirStmt::Instr(HirInstr::Return(operand)));
            }
            Statement::Break { span } => {
                if self.loop_depth == 0 {
                    return Err(BuildError::at(BuildErrorKind::Unimplemented {
                        node: "break outside loop",
                    })
                    .with_site(self.module, *span));
                }
                block.push(HirStmt::Break);
            }
            Statement::Continue { span } => {
                if self.loop_depth == 0 {
                    return Err(BuildError::at(BuildErrorKind::Unimplemented {
                        node: "continue outside loop",
                    })
                    .with_site(self.module, *span));
                }
                block.push(HirStmt::Continue);
            }
            Statement::Block(inner) => {
                let nested = self.with_scope(|this| {
                    let mut ops = HirBlock::new();
                    this.emit_block(inner, &mut ops)?;
                    Ok(ops)
                })?;
                block.push(HirStmt::Block(nested));
            }
            Statement::If(if_stmt) => self.emit_if(if_stmt, block)?,
            Statement::While {
                condition, body, ..
            } => {
                // 条件每轮重新求值：求值所需的指令全部落进 `cond.prelude`，
                // 循环外的块只保留 `Loop` 本身（事先发射会让条件只算一次）
                let mut prelude = HirBlock::new();
                let value = self.build_expression(condition, &mut prelude)?;
                self.loop_depth += 1;
                let body_block = self.with_scope(|this| this.build_statement(body));
                self.loop_depth -= 1;
                block.push(HirStmt::Loop {
                    cond: Some(HirCond { prelude, value }),
                    body: body_block?,
                    step: HirBlock::new(),
                });
            }
            Statement::For(for_stmt) => self.emit_for(for_stmt, block)?,
            Statement::Unsafe { block: inner, .. } => {
                let nested = self.with_scope(|this| {
                    let mut ops = HirBlock::new();
                    ops.push(HirStmt::EnterScope);
                    this.emit_block(inner, &mut ops)?;
                    ops.push(HirStmt::LeaveScope);
                    Ok(ops)
                })?;
                block.push(HirStmt::Block(nested));
            }
            Statement::Expression(expr) => {
                self.build_expression(expr, block)?;
            }
            // 异常传播和挂起需要运行时协议，不能替换为普通 return / 求值。
            Statement::Throw { span, .. } => {
                return Err(
                    BuildError::at(BuildErrorKind::Unimplemented { node: "throw" })
                        .with_site(self.module, *span),
                );
            }
            Statement::Yield { span, .. } => {
                return Err(
                    BuildError::at(BuildErrorKind::Unimplemented { node: "yield" })
                        .with_site(self.module, *span),
                );
            }
            Statement::Try(try_stmt) => {
                return Err(
                    BuildError::at(BuildErrorKind::Unimplemented { node: "try" })
                        .with_site(self.module, try_stmt.span),
                );
            }
            // `with expr as b { ... }`：expr 求值一次，体内成员名解析到该接收者
            Statement::With(with_stmt) => self.emit_with(with_stmt, block)?,
        }
        Ok(())
    }

    /// `with` 语句：接收者求值一次，在块内递归绑定局部名字。
    ///
    /// 解析期用 `with_stack` / `with_shield` 判定 `as` 的归属，那套上下文状态
    /// 在这里全部消除：HIR 只留「接收者值 + 接收者类型 + 块」。
    pub(super) fn emit_with(
        &mut self,
        with_stmt: &novella_ast::stmt::WithStmt,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        let receiver_ty = match with_stmt
            .binding
            .as_ref()
            .and_then(|binding| binding.ty.as_ref())
        {
            Some(annotation) => annotation_type(self.sema, self.module, annotation)?,
            None => self.expr_type(with_stmt.expr.span())?,
        };
        let receiver = self.build_typed_expression(&with_stmt.expr, &receiver_ty, block)?;
        let outer = self.with_receiver.clone();
        self.with_receiver = Some((receiver_ty.clone(), receiver.clone()));
        let result = self.with_scope(|this| {
            let mut body = HirBlock::new();
            if let Some(binding) = &with_stmt.binding {
                this.bind_pattern(&binding.pattern, &receiver_ty, Some(receiver), &mut body)?;
            }
            this.emit_block(&with_stmt.block, &mut body)?;
            Ok(body)
        });
        self.with_receiver = outer;
        block.push(HirStmt::Block(result?));
        Ok(())
    }

    /// 块体发射（不引入新块节点，语句直接落进 `block`）。
    pub(super) fn emit_block(
        &mut self,
        inner: &Block,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        for statement in &inner.stmts {
            self.emit_statement(statement, block)?;
        }
        Ok(())
    }

    /// 变量声明：定型 → 申请槽位 → `Alloc`。
    pub(super) fn emit_variable(
        &mut self,
        variable: &VariableStmt,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        let view = self
            .sema
            .declaration_types
            .get(self.module, variable.span)
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingType {
                    what: "变量声明类型",
                })
                .with_site(self.module, variable.span)
            })?;
        let ty = HirType::from_view(view, &mut function_id_of)
            .map_err(|error| error.with_site(self.module, variable.span))?;
        // 在引入新名字前求初始化值，遮蔽时仍引用外层绑定。
        let init = variable
            .value
            .as_ref()
            .map(|expr| self.build_typed_expression(expr, &ty, block))
            .transpose()?;
        self.bind_pattern(&variable.pattern, &ty, init, block)
    }

    /// 递归解构：初始化值只求一次，再按元素提取并声明局部。
    pub(super) fn bind_pattern(
        &mut self,
        pattern: &BindingPattern,
        ty: &HirType,
        init: Option<HirOperand>,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        match pattern {
            BindingPattern::Ident { name, .. } => {
                let id = self.declare_var(name.clone(), ty.clone());
                block.push(HirStmt::Instr(HirInstr::Alloc {
                    id,
                    ty: ty.clone(),
                    init,
                }));
            }
            BindingPattern::Tuple { elements, span } => {
                let HirType::Tuple(types) = ty else {
                    return Err(BuildError::at(BuildErrorKind::MissingType {
                        what: "解构元组类型",
                    })
                    .with_site(self.module, *span));
                };
                if types.len() != elements.len() {
                    return Err(BuildError::at(BuildErrorKind::Unimplemented {
                        node: "tuple pattern arity",
                    })
                    .with_site(self.module, *span));
                }
                for (index, (pattern, ty)) in elements.iter().zip(types).enumerate() {
                    let value = init.as_ref().map(|target| {
                        let dst = self.fresh_local(ty.clone());
                        block.push(HirStmt::Instr(HirInstr::TupleExtract {
                            target: target.clone(),
                            index,
                            ty: ty.clone(),
                            dst,
                        }));
                        HirOperand::Local(dst)
                    });
                    self.bind_pattern(pattern, ty, value, block)?;
                }
            }
        }
        Ok(())
    }

    /// 源类型与目标类型不同时补一条 `Cast` 指令，返回转换后的操作数。
    pub(super) fn coerce(
        &mut self,
        operand: HirOperand,
        from: &HirType,
        to: &HirType,
        span: Span,
        block: &mut HirBlock,
    ) -> Result<HirOperand, BuildError> {
        // 未定型字面量：上下文直接给出目标类型，常量原地落地，不发射指令
        match (&operand, to) {
            (HirOperand::UntypedInt { value, .. }, HirType::Int(_)) => {
                return Ok(HirOperand::IntConst(*value));
            }
            (HirOperand::UntypedInt { value, .. }, HirType::Float(_)) => {
                return Ok(HirOperand::FloatConst(*value as f64));
            }
            (HirOperand::UntypedFloat { value, .. }, HirType::Float(_)) => {
                return Ok(HirOperand::FloatConst(*value));
            }
            (HirOperand::UntypedFloat { value, .. }, HirType::Int(_)) => {
                return Ok(HirOperand::IntConst(*value as i64));
            }
            _ => {}
        }
        if same_value_type(from, to) {
            return Ok(operand);
        }
        if !castable(from, to) {
            return Err(BuildError::at(BuildErrorKind::ImplicitCast {
                from: type_label(from),
                to: type_label(to),
            })
            .with_site(self.module, span));
        }
        let dst = self.fresh_local(to.clone());
        block.push(HirStmt::Instr(HirInstr::Cast {
            operand,
            from: from.clone(),
            to: to.clone(),
            dst,
        }));
        Ok(HirOperand::Local(dst))
    }

    // ---------- 控制流 ----------

    /// if / elif / else：条件链顺序发射，`else` 折叠进紧邻的上一条 `If`。
    pub(super) fn emit_if(
        &mut self,
        if_stmt: &IfStmt,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        // 逆序构造嵌套：最内层是 else（或无），逐层向外包 `If`
        let mut tail: Option<HirBlock> = None;
        for branch in if_stmt.branches.iter().rev() {
            match branch {
                IfBranch::Else { block: body, .. } => {
                    tail = Some(self.with_scope(|this| this.build_statement(body))?);
                }
                IfBranch::ElseIf {
                    condition,
                    block: body,
                    ..
                } => {
                    let mut wrapper = HirBlock::new();
                    let cond = self.build_expression(condition, &mut wrapper)?;
                    let then_block = self.with_scope(|this| this.build_statement(body))?;
                    wrapper.push(HirStmt::If {
                        cond: HirCond::new(cond),
                        then_block,
                        else_block: tail.take(),
                    });
                    tail = Some(wrapper);
                }
            }
        }
        match tail {
            Some(wrapped) => {
                // 条件的前置指令必须留在当前块，因此把 wrapper 摊平
                for stmt in wrapped.stmts {
                    block.push(stmt);
                }
            }
            None => block.push(HirStmt::Nop),
        }
        Ok(())
    }

    /// for 循环：C 风格三段式与数组 for-in 分别展开为 Loop。
    pub(super) fn emit_for(
        &mut self,
        for_stmt: &novella_ast::stmt::ForStmt,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        match &for_stmt.condition {
            ForKind::ForLoop(cond) => self.emit_c_for(cond, &for_stmt.block, block),
            ForKind::ForIn(cond) => self.emit_for_in(cond, &for_stmt.block, block),
        }
    }

    /// C 风格 for：`init` → `loop { if !cond break; body; update }`。
    pub(super) fn emit_c_for(
        &mut self,
        cond: &novella_ast::stmt::ForLoopCond,
        body: &Statement,
        block: &mut HirBlock,
    ) -> Result<(), BuildError> {
        // for 自带一层作用域：init 声明的变量不泄漏到循环外
        self.with_scope(|this| {
            if let Some(init) = &cond.init {
                match init {
                    ForInit::Declaration { declarations, .. } => {
                        for declaration in declarations {
                            this.emit_variable(declaration, block)?;
                        }
                    }
                    ForInit::Expression { expr, .. } => {
                        this.build_expression(expr, block)?;
                    }
                }
            }
            // 省略条件视为恒真；条件求值语句每轮都要重跑，落进 `prelude`
            let (mut prelude, value) = match &cond.condition {
                Some(expr) => {
                    let mut prelude = HirBlock::new();
                    let value = this.build_expression(expr, &mut prelude)?;
                    (prelude, value)
                }
                None => (HirBlock::new(), HirOperand::BoolConst(true)),
            };
            this.loop_depth += 1;
            let body_block = this.with_scope(|inner| inner.build_statement(body));
            this.loop_depth -= 1;
            // `update` 放入 `Loop.step`：`continue` 跳过体但必须执行它
            let step = match &cond.update {
                Some(update) => {
                    let mut step = HirBlock::new();
                    this.build_expression(update, &mut step)?;
                    step
                }
                None => HirBlock::new(),
            };
            prelude.stmts.shrink_to_fit();
            block.push(HirStmt::Loop {
                cond: Some(HirCond { prelude, value }),
                body: body_block?,
                step,
            });
            Ok(())
        })
    }
}
