//! 将 HIR 控制流和顺序指令发射为 LLVM 基本块。

use std::collections::HashMap;

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicValueEnum, FunctionValue, PointerValue};
use novella_hir::{
    FunctionSignature, HirBlock, HirCond, HirFunction, HirFunctionId, HirInstr, HirOperand,
    HirStmt, HirType, HirUnaryOp, LocalId,
};

use crate::{CodegenError, Result, value_type};

pub(crate) type Functions<'ctx, 'hir> =
    HashMap<HirFunctionId, (FunctionValue<'ctx>, &'hir FunctionSignature)>;

struct LoopTargets<'ctx> {
    exit: BasicBlock<'ctx>,
    step: BasicBlock<'ctx>,
    has_break: bool,
    has_continue: bool,
}

pub(crate) struct Body<'ctx, 'hir> {
    pub(crate) context: &'ctx Context,
    pub(crate) module: &'hir Module<'ctx>,
    pub(crate) builder: Builder<'ctx>,
    pub(crate) function: FunctionValue<'ctx>,
    functions: &'hir Functions<'ctx, 'hir>,
    ret: &'hir HirType,
    entry: BasicBlock<'ctx>,
    slots: HashMap<LocalId, (PointerValue<'ctx>, HirType)>,
    loops: Vec<LoopTargets<'ctx>>,
}

pub(crate) fn emit<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    functions: &Functions<'ctx, '_>,
    hir: &HirFunction,
) -> Result<()> {
    let function = functions[&hir.id].0;
    let entry = context.append_basic_block(function, "entry");
    let mut body = Body {
        context,
        module,
        builder: context.create_builder(),
        function,
        functions,
        ret: &hir.signature.ret,
        entry,
        slots: HashMap::new(),
        loops: Vec::new(),
    };
    body.builder.position_at_end(entry);
    if function.count_params() as usize != hir.params.len() {
        return Err(CodegenError("HIR 形参与函数签名不一致".into()));
    }
    for (param, value) in hir.params.iter().zip(function.get_param_iter()) {
        body.save(param.id, &param.ty, value)?;
    }
    if body.block(&hir.body)? {
        if *body.ret != HirType::Void {
            return Err(CodegenError("有返回值的函数存在未返回的路径".into()));
        }
        body.builder.build_return(None)?;
    }
    Ok(())
}

impl<'ctx> Body<'ctx, '_> {
    fn slot(&self, id: LocalId) -> Result<&(PointerValue<'ctx>, HirType)> {
        self.slots
            .get(&id)
            .ok_or_else(|| CodegenError(format!("HIR 槽位 {} 尚未定义", id.0)))
    }

    fn allocate(&mut self, id: LocalId, ty: &HirType) -> Result<PointerValue<'ctx>> {
        if self.slots.contains_key(&id) {
            return Err(CodegenError(format!("重复定义 HIR 槽位 {}", id.0)));
        }
        // 槽位只在入口分配；循环里的初始化仍留在原指令位置。
        let allocator = self.context.create_builder();
        if let Some(first) = self.entry.get_first_instruction() {
            allocator.position_before(&first);
        } else {
            allocator.position_at_end(self.entry);
        }
        let pointer = allocator.build_alloca(value_type(self.context, ty)?, "slot")?;
        self.slots.insert(id, (pointer, ty.clone()));
        Ok(pointer)
    }

    fn save(&mut self, id: LocalId, ty: &HirType, value: BasicValueEnum<'ctx>) -> Result<()> {
        if value.get_type() != value_type(self.context, ty)? {
            return Err(CodegenError(format!("HIR 槽位 {} 的结果类型不匹配", id.0)));
        }
        let pointer = self.allocate(id, ty)?;
        self.builder.build_store(pointer, value)?;
        Ok(())
    }

    pub(crate) fn operand(
        &self,
        operand: &HirOperand,
        ty: &HirType,
    ) -> Result<BasicValueEnum<'ctx>> {
        match (operand, ty) {
            (HirOperand::Local(id), _) => {
                let (pointer, actual) = self.slot(*id)?;
                if actual != ty {
                    return Err(CodegenError(format!(
                        "HIR 槽位 {} 类型不匹配：{actual:?} / {ty:?}",
                        id.0
                    )));
                }
                Ok(self
                    .builder
                    .build_load(value_type(self.context, ty)?, *pointer, "load")?)
            }
            (HirOperand::IntConst(value), HirType::Int(_)) => Ok(value_type(self.context, ty)?
                .into_int_type()
                .const_int(*value as u64, false)
                .into()),
            (HirOperand::BoolConst(value), HirType::Boolean) => Ok(self
                .context
                .bool_type()
                .const_int(u64::from(*value), false)
                .into()),
            (HirOperand::Null, HirType::Pointer(_)) => Ok(value_type(self.context, ty)?
                .into_pointer_type()
                .const_null()
                .into()),
            _ => Err(CodegenError(format!(
                "暂不支持该 HIR 操作数或类型不匹配：{operand:?} / {ty:?}"
            ))),
        }
    }

    fn instruction(&mut self, instruction: &HirInstr) -> Result<bool> {
        match instruction {
            HirInstr::Alloc { id, ty, init } => {
                let value = init
                    .as_ref()
                    .map(|operand| self.operand(operand, ty))
                    .transpose()?;
                let pointer = self.allocate(*id, ty)?;
                if let Some(value) = value {
                    self.builder.build_store(pointer, value)?;
                }
            }
            HirInstr::Load { dst, src } => {
                let ty = self.slot(*src)?.1.clone();
                let value = self.operand(&HirOperand::Local(*src), &ty)?;
                self.save(*dst, &ty, value)?;
            }
            HirInstr::Store { id, value } => {
                let (pointer, ty) = self.slot(*id)?;
                self.builder
                    .build_store(*pointer, self.operand(value, ty)?)?;
            }
            HirInstr::LocalAddress { local, ty, dst } => {
                let (pointer, actual) = self.slot(*local)?;
                if actual != ty {
                    return Err(CodegenError("局部变量取址类型不匹配".into()));
                }
                self.save(
                    *dst,
                    &HirType::Pointer(Box::new(ty.clone())),
                    (*pointer).into(),
                )?;
            }
            HirInstr::StoreIndirect { address, value, ty } => {
                let address = self
                    .operand(address, &HirType::Pointer(Box::new(ty.clone())))?
                    .into_pointer_value();
                self.builder
                    .build_store(address, self.operand(value, ty)?)?;
            }
            HirInstr::Unary {
                op: HirUnaryOp::Deref,
                operand,
                dst,
                dst_ty,
            } => {
                let address = self
                    .operand(operand, &HirType::Pointer(Box::new(dst_ty.clone())))?
                    .into_pointer_value();
                let value =
                    self.builder
                        .build_load(value_type(self.context, dst_ty)?, address, "deref")?;
                self.save(*dst, dst_ty, value)?;
            }
            HirInstr::Unary {
                op,
                operand,
                dst,
                dst_ty,
            } => {
                let value = self.unary(*op, operand, dst_ty)?;
                self.save(*dst, dst_ty, value)?;
            }
            HirInstr::Binary {
                op,
                lhs,
                rhs,
                lhs_ty,
                rhs_ty,
                dst,
                dst_ty,
            } => {
                let value = self.binary(*op, (lhs, lhs_ty), (rhs, rhs_ty))?;
                self.save(*dst, dst_ty, value)?;
            }
            HirInstr::Cast {
                operand,
                from,
                to,
                dst,
            } => {
                let value = self.cast(operand, from, to)?;
                self.save(*dst, to, value)?;
            }
            HirInstr::Call {
                callee,
                args,
                dst,
                dst_ty,
            } => {
                let (HirOperand::Function(id) | HirOperand::FunctionPtr(id)) = callee else {
                    return Err(CodegenError("暂不支持间接调用".into()));
                };
                let &(callee, signature) = self
                    .functions
                    .get(id)
                    .ok_or_else(|| CodegenError(format!("未定义 HIR 函数 `{id}`")))?;
                if args.len() != signature.params.len() {
                    return Err(CodegenError(format!("`{id}` 实参个数与签名不一致")));
                }
                let args = args
                    .iter()
                    .zip(&signature.params)
                    .map(|(operand, ty)| self.operand(operand, ty).map(Into::into))
                    .collect::<Result<Vec<_>>>()?;
                let call = self.builder.build_call(callee, &args, "call")?;
                if let Some(dst) = dst {
                    let value = call
                        .try_as_basic_value()
                        .basic()
                        .filter(|_| dst_ty.as_ref() == Some(&signature.ret))
                        .ok_or_else(|| {
                            CodegenError(format!("`{id}` 调用的返回类型与 HIR 不一致"))
                        })?;
                    self.save(*dst, &signature.ret, value)?;
                }
            }
            HirInstr::Return(value) => {
                let value = value
                    .as_ref()
                    .map(|operand| self.operand(operand, self.ret))
                    .transpose()?;
                if value.is_none() != (*self.ret == HirType::Void) {
                    return Err(CodegenError("返回值与函数签名不一致".into()));
                }
                self.builder.build_return(
                    value
                        .as_ref()
                        .map(|value| value as &dyn inkwell::values::BasicValue),
                )?;
                return Ok(false);
            }
            HirInstr::FieldAddress { .. }
            | HirInstr::FieldLoad { .. }
            | HirInstr::FieldStore { .. }
            | HirInstr::Tuple { .. }
            | HirInstr::TupleExtract { .. }
            | HirInstr::Array { .. }
            | HirInstr::ArrayLength { .. }
            | HirInstr::IndexAddress { .. }
            | HirInstr::IndexLoad { .. }
            | HirInstr::IndexStore { .. }
            | HirInstr::TypeTest { .. } => {
                return Err(CodegenError(format!("暂不支持 HIR 指令 {instruction:?}")));
            }
        }
        Ok(true)
    }

    fn block(&mut self, block: &HirBlock) -> Result<bool> {
        for stmt in &block.stmts {
            let falls_through = match stmt {
                HirStmt::Instr(instruction) => self.instruction(instruction)?,
                HirStmt::Block(block) => self.block(block)?,
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
                    self.builder.build_unconditional_branch(target)?;
                    false
                }
                HirStmt::EnterScope | HirStmt::LeaveScope | HirStmt::Nop => true,
            };
            if !falls_through {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn branch(
        &mut self,
        cond: &HirCond,
        then_block: &HirBlock,
        else_block: Option<&HirBlock>,
    ) -> Result<bool> {
        if !self.block(&cond.prelude)? {
            return Ok(false);
        }
        let value = self
            .operand(&cond.value, &HirType::Boolean)?
            .into_int_value();
        if let Some(constant) = value.get_zero_extended_constant() {
            return if constant != 0 {
                self.block(then_block)
            } else if let Some(block) = else_block {
                self.block(block)
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
        let then_falls = self.block(then_block)?;
        if then_falls {
            self.builder.build_unconditional_branch(merge)?;
        }
        self.builder.position_at_end(else_label);
        let else_falls = if let Some(block) = else_block {
            self.block(block)?
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

    fn loop_block(
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
        });
        self.builder.build_unconditional_branch(head)?;
        self.builder.position_at_end(head);
        let (head_falls, head_can_exit) = if let Some(cond) = cond {
            if self.block(&cond.prelude)? {
                let value = self
                    .operand(&cond.value, &HirType::Boolean)?
                    .into_int_value();
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
                (false, false)
            }
        } else {
            self.builder.build_unconditional_branch(body_label)?;
            (true, false)
        };

        self.builder.position_at_end(body_label);
        let body_falls = if head_falls {
            self.block(body)?
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
            if self.block(step)? {
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
