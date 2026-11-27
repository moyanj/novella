//! 结构化控制流及作用域退出清理。

use super::*;

impl<'ctx> Body<'ctx, '_> {
    pub(super) fn block(&mut self, block: &HirBlock) -> Result<bool> {
        for stmt in &block.stmts {
            let falls_through = match stmt {
                HirStmt::Instr(instruction) => self.instruction(instruction)?,
                HirStmt::Block(block) => self.scoped_block(block)?,
                HirStmt::If {
                    cond,
                    then_block,
                    else_block,
                } => self.branch(cond, then_block, else_block.as_ref())?,
                HirStmt::Loop { cond, body, step } => self.loop_block(cond.as_ref(), body, step)?,
                HirStmt::Break | HirStmt::Continue => {
                    let targets = self
                        .loops
                        .last_mut()
                        .ok_or_else(|| CodegenError("循环外出现跳转".into()))?;
                    let target = if matches!(stmt, HirStmt::Break) {
                        targets.has_break = true;
                        targets.exit
                    } else {
                        targets.has_continue = true;
                        targets.step
                    };
                    let owned_start = targets.owned_start;
                    self.release_owned(owned_start, None)?;
                    self.builder.build_unconditional_branch(target)?;
                    false
                }
                HirStmt::ReleaseTemps(ids) => {
                    self.release_temps(ids)?;
                    true
                }
                HirStmt::EnterScope | HirStmt::LeaveScope | HirStmt::Nop => true,
            };
            if !falls_through {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn scoped_block(&mut self, block: &HirBlock) -> Result<bool> {
        let start = self.owned.len();
        let falls = self.block(block)?;
        if falls {
            self.release_owned(start, None)?;
        }
        self.owned.truncate(start);
        Ok(falls)
    }

    pub(super) fn branch(
        &mut self,
        cond: &HirCond,
        then_block: &HirBlock,
        else_block: Option<&HirBlock>,
    ) -> Result<bool> {
        let condition_start = self.owned.len();
        if !self.block(&cond.prelude)? {
            return Ok(false);
        }
        let value = self
            .operand(&cond.value, &HirType::Boolean)?
            .into_int_value();
        self.release_owned(condition_start, None)?;
        self.owned.truncate(condition_start);
        if let Some(constant) = value.get_zero_extended_constant() {
            return if constant != 0 {
                self.scoped_block(then_block)
            } else if let Some(block) = else_block {
                self.scoped_block(block)
            } else {
                Ok(true)
            };
        }
        let then_label = self.context.append_basic_block(self.function, "then");
        let else_label = self.context.append_basic_block(self.function, "else");
        let merge = self.context.append_basic_block(self.function, "merge");
        self.builder
            .build_conditional_branch(value, then_label, else_label)?;

        self.builder.position_at_end(then_label);
        let then_falls = self.scoped_block(then_block)?;
        if then_falls {
            self.builder.build_unconditional_branch(merge)?;
        }
        self.builder.position_at_end(else_label);
        let else_falls = if let Some(block) = else_block {
            self.scoped_block(block)?
        } else {
            true
        };
        if else_falls {
            self.builder.build_unconditional_branch(merge)?;
        }
        self.builder.position_at_end(merge);
        if !then_falls && !else_falls {
            self.builder.build_unreachable()?;
        }
        Ok(then_falls || else_falls)
    }

    pub(super) fn loop_block(
        &mut self,
        cond: Option<&HirCond>,
        body: &HirBlock,
        step: &HirBlock,
    ) -> Result<bool> {
        let head = self.context.append_basic_block(self.function, "loop.head");
        let body_label = self.context.append_basic_block(self.function, "loop.body");
        let step_label = self.context.append_basic_block(self.function, "loop.step");
        let exit = self.context.append_basic_block(self.function, "loop.exit");
        self.loops.push(LoopTargets {
            exit,
            step: step_label,
            has_break: false,
            has_continue: false,
            owned_start: self.owned.len(),
        });
        self.builder.build_unconditional_branch(head)?;
        self.builder.position_at_end(head);
        let condition_start = self.owned.len();
        let (head_falls, head_can_exit) = if let Some(cond) = cond {
            if self.block(&cond.prelude)? {
                let value = self
                    .operand(&cond.value, &HirType::Boolean)?
                    .into_int_value();
                self.release_owned(condition_start, None)?;
                self.owned.truncate(condition_start);
                match value.get_zero_extended_constant() {
                    Some(0) => {
                        self.builder.build_unconditional_branch(exit)?;
                        (false, true)
                    }
                    Some(_) => {
                        self.builder.build_unconditional_branch(body_label)?;
                        (true, false)
                    }
                    None => {
                        self.builder
                            .build_conditional_branch(value, body_label, exit)?;
                        (true, true)
                    }
                }
            } else {
                self.owned.truncate(condition_start);
                (false, false)
            }
        } else {
            self.builder.build_unconditional_branch(body_label)?;
            (true, false)
        };

        self.builder.position_at_end(body_label);
        let body_falls = if head_falls {
            self.scoped_block(body)?
        } else {
            self.builder.build_unreachable()?;
            false
        };
        if body_falls {
            self.builder.build_unconditional_branch(step_label)?;
        }
        self.builder.position_at_end(step_label);
        let targets = self.loops.last_mut().expect("已压入循环目标");
        let step_reachable = body_falls || targets.has_continue;
        targets.step = head;
        if step_reachable {
            if self.scoped_block(step)? {
                self.builder.build_unconditional_branch(head)?;
            }
        } else {
            self.builder.build_unreachable()?;
        }
        let targets = self.loops.pop().expect("已压入循环目标");
        self.builder.position_at_end(exit);
        let exit_reachable = head_can_exit || targets.has_break;
        if !exit_reachable {
            self.builder.build_unreachable()?;
        }
        Ok(exit_reachable)
    }
}
