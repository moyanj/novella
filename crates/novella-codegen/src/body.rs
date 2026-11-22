//! 将 HIR 控制流和顺序指令发射为 LLVM 基本块。

use std::collections::{HashMap, HashSet};

use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::values::{BasicMetadataValueEnum, BasicValueEnum, FunctionValue, PointerValue};
use novella_hir::{
    FunctionSignature, HirBlock, HirCond, HirFunction, HirFunctionId, HirInstr, HirOperand,
    HirStmt, HirType, HirUnaryOp, LocalId,
};

use crate::{CodegenError, Result, Types, value_type};

mod arrays;
mod calls;
mod control;
mod instructions;
mod ownership;

pub(crate) type Functions<'ctx, 'hir> =
    HashMap<HirFunctionId, (FunctionValue<'ctx>, &'hir FunctionSignature)>;

struct LoopTargets<'ctx> {
    exit: BasicBlock<'ctx>,
    step: BasicBlock<'ctx>,
    has_break: bool,
    has_continue: bool,
    owned_start: usize,
}

pub(crate) struct Body<'ctx, 'hir> {
    pub(crate) context: &'ctx Context,
    pub(crate) module: &'hir Module<'ctx>,
    pub(crate) builder: Builder<'ctx>,
    pub(crate) function: FunctionValue<'ctx>,
    pub(crate) types: &'hir Types<'ctx>,
    functions: &'hir Functions<'ctx, 'hir>,
    ret: &'hir HirType,
    entry: BasicBlock<'ctx>,
    slots: HashMap<LocalId, (PointerValue<'ctx>, HirType)>,
    loops: Vec<LoopTargets<'ctx>>,
    // 按定义顺序记录拥有槽位，逆序清理；析构接收者及其快照仅借用。
    owned: Vec<LocalId>,
    borrowed: HashSet<LocalId>,
}

pub(crate) fn emit<'ctx>(
    context: &'ctx Context,
    module: &Module<'ctx>,
    types: &'ctx Types<'ctx>,
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
        types,
        functions,
        ret: &hir.signature.ret,
        entry,
        slots: HashMap::new(),
        loops: Vec::new(),
        owned: Vec::new(),
        borrowed: HashSet::new(),
    };
    body.builder.position_at_end(entry);
    if function.count_params() as usize != hir.params.len() {
        return Err(CodegenError("HIR 形参与函数签名不一致".into()));
    }
    for (index, (param, value)) in hir.params.iter().zip(function.get_param_iter()).enumerate() {
        if index == 0 && hir.signature.receiver {
            // 接收者在整个调用期间由调用方保活，析构期间也只借用 self。
            body.borrowed.insert(param.id);
            body.save(param.id, &param.ty, value)?;
        } else {
            body.save_parameter(param.id, &param.ty, value)?;
        }
    }
    if body.block(&hir.body)? {
        if *body.ret != HirType::Void {
            return Err(CodegenError("有返回值的函数存在未返回的路径".into()));
        }
        body.release_locals(None)?;
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
        let pointer = allocator.build_alloca(value_type(self.context, self.types, ty)?, "slot")?;
        if Self::is_managed(ty) {
            allocator.build_store(
                pointer,
                value_type(self.context, self.types, ty)?.const_zero(),
            )?;
        }
        self.slots.insert(id, (pointer, ty.clone()));
        if Self::is_managed(ty) && !self.borrowed.contains(&id) {
            self.owned.push(id);
        }
        Ok(pointer)
    }

    fn save(&mut self, id: LocalId, ty: &HirType, value: BasicValueEnum<'ctx>) -> Result<()> {
        if value.get_type() != value_type(self.context, self.types, ty)? {
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
                Ok(self.builder.build_load(
                    value_type(self.context, self.types, ty)?,
                    *pointer,
                    "load",
                )?)
            }
            (HirOperand::GlobalAddress { .. } | HirOperand::AggregateConst(_), _) => {
                crate::globals::constant(
                    self.context,
                    self.module,
                    self.types,
                    self.functions,
                    operand,
                    ty,
                )
            }
            (HirOperand::IntConst(value), HirType::Int(_)) => {
                Ok(value_type(self.context, self.types, ty)?
                    .into_int_type()
                    .const_int(*value as u64, false)
                    .into())
            }
            (HirOperand::FloatConst(value), HirType::Float(_)) => {
                Ok(value_type(self.context, self.types, ty)?
                    .into_float_type()
                    .const_float(*value)
                    .into())
            }
            (HirOperand::BoolConst(value), HirType::Boolean) => Ok(self
                .context
                .bool_type()
                .const_int(u64::from(*value), false)
                .into()),
            (HirOperand::Null, HirType::Pointer(_) | HirType::Class(..) | HirType::Array(_)) => {
                Ok(value_type(self.context, self.types, ty)?
                    .into_pointer_type()
                    .const_null()
                    .into())
            }
            (
                HirOperand::Function(id) | HirOperand::FunctionPtr(id),
                HirType::Function(expected),
            ) => {
                let (function, signature) = self
                    .functions
                    .get(id)
                    .ok_or_else(|| CodegenError(format!("未定义 HIR 函数 `{id}`")))?;
                if crate::function_type(self.context, self.types, signature)?
                    != crate::function_type(self.context, self.types, expected)?
                {
                    return Err(CodegenError(format!("函数 `{id}` 与回调签名不一致")));
                }
                Ok(function.as_global_value().as_pointer_value().into())
            }
            _ => Err(CodegenError(format!(
                "暂不支持该 HIR 操作数或类型不匹配：{operand:?} / {ty:?}"
            ))),
        }
    }
}
