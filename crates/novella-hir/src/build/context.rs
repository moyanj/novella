//! HIR 降级：context 职责实现。

use super::*;

impl<'a> BodyBuilder<'a> {
    /// 默认值使用定义处词法环境；保留调用方临时值编号与已发射指令。
    pub(super) fn in_definition_scope<T>(
        &mut self,
        module: ModuleId,
        locals: Vec<(String, LocalId)>,
        f: impl FnOnce(&mut Self) -> Result<T, BuildError>,
    ) -> Result<T, BuildError> {
        let previous_module = std::mem::replace(&mut self.module, module);
        let previous_locals = std::mem::replace(&mut self.locals, locals);
        let previous_functions = std::mem::take(&mut self.functions);
        let previous_captures = std::mem::take(&mut self.capture_names);
        let previous_receiver = self.with_receiver.take();
        let result = f(self);
        self.module = previous_module;
        self.locals = previous_locals;
        self.functions = previous_functions;
        self.capture_names = previous_captures;
        self.with_receiver = previous_receiver;
        result
    }
    pub(super) fn new(
        sema: &'a SemaResult,
        module: ModuleId,
        functions: Vec<(String, HirFunctionId)>,
    ) -> Self {
        BodyBuilder {
            sema,
            module,
            functions,
            next_local: 0,
            local_types: Vec::new(),
            locals: Vec::new(),
            capture_names: HashSet::new(),
            loop_depth: 0,
            return_type: None,
            params: Vec::new(),
            with_receiver: None,
            lambdas: Vec::new(),
        }
    }

    // ---------- 局部槽位 ----------

    /// 申请局部槽位并登记类型。
    pub(super) fn fresh_local(&mut self, ty: HirType) -> LocalId {
        let id = LocalId(self.next_local);
        self.next_local += 1;
        self.local_types.push(ty);
        id
    }

    /// 形参槽位：带名字，参与标识符解析，并登记进参数表。
    pub(super) fn declare_param(&mut self, name: String, ty: HirType) -> LocalId {
        let id = self.fresh_local(ty.clone());
        self.locals.push((name.clone(), id));
        self.params.push(LocalSlot { id, name, ty });
        id
    }

    /// 已声明的形参槽位（交给 [`HirFunction::params`]）。
    pub(super) fn parameter_slots(&self) -> Vec<LocalSlot> {
        self.params.clone()
    }

    /// 变量槽位：带名字，参与标识符解析。
    pub(super) fn declare_var(&mut self, name: String, ty: HirType) -> LocalId {
        let id = self.fresh_local(ty);
        self.locals.push((name, id));
        id
    }

    /// 局部类型查询。
    pub(super) fn local_type(&self, id: LocalId) -> HirType {
        self.local_types[id.0 as usize].clone()
    }

    /// 按名字查当前可见局部。
    pub(super) fn lookup_local(&self, name: &str) -> Option<LocalId> {
        self.locals
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, id)| *id)
    }

    /// 在临时作用域内执行，返回后还原名字绑定。
    pub(super) fn with_scope<T>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, BuildError>,
    ) -> Result<T, BuildError> {
        let mark = self.locals.len();
        let result = f(self);
        self.locals.truncate(mark);
        result
    }

    // ---------- 类型来源 ----------

    /// 查 sema 的表达式类型表并转成 HIR 类型。
    ///
    /// 这是 HIR 里所有 `dst_ty` / 局部类型的**唯一事实来源**。
    pub(super) fn expr_type(&self, span: Span) -> Result<HirType, BuildError> {
        let view = self
            .sema
            .expression_types
            .get(self.module, span)
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingExpressionType).with_site(self.module, span)
            })?;
        HirType::from_view(view, &mut |fv| self.synth_function_id(fv))
            .map_err(|error| error.with_site(self.module, span))
    }

    /// 表达式类型，但允许「上下文期望类型」把未定型字面量落地。
    ///
    /// sema 的类型表对 `42` / `1.5` 这类字面量永远登记 `IntLiteral` /
    /// `FloatLiteral`（它们的最终类型由使用位置决定）。因此凡是有期望类型的
    /// 位置（返回位置、二元运算的操作数、赋值右值、var/field 初始化…）都要走
    /// 这个入口；只有不知道该期望什么时才用 [`Self::expr_type`]（→ fail-fast）。
    pub(super) fn expr_type_pref(
        &self,
        span: Span,
        preferred: Option<&HirType>,
    ) -> Result<HirType, BuildError> {
        let view = self
            .sema
            .expression_types
            .get(self.module, span)
            .ok_or_else(|| {
                BuildError::at(BuildErrorKind::MissingExpressionType).with_site(self.module, span)
            })?;
        HirType::from_view_with(view, &mut |fv| self.synth_function_id(fv), preferred)
            .map_err(|error| error.with_site(self.module, span))
    }

    /// 具名实体沿用所属模块；匿名键只描述签名，不代表唯一函数实体。
    pub(super) fn synth_function_id(&self, view: &novella_sema::FunctionView) -> HirFunctionId {
        match &view.name {
            Some(name) => HirFunctionId(format!("m{}::{}", view.module, name)),
            None => HirFunctionId(format!("m{}::anon#{}", self.module, view.params.len())),
        }
    }
}
