//! 递归解析的栈扩展（`rust-lang/stacker`）。
//!
//! 递归下降每深入一层括号 / 一元前缀 / 块都消耗一层真实栈帧：debug 构建下
//! 每层可达数十 KB，主线程 8 MiB 栈在百余层即溢出。此前的做法是用
//! `MAX_NESTING = 128` 直接拒绝更深输入——合法但嵌套很深的源码也被拒。
//!
//! 现在改为**按需扩展分段栈**：每个递归入口都包一层 [`Parser::guarded`]，
//! 由 `stacker::maybe_grow` 在剩余栈逼近红区时切换到堆上新分段继续递归，
//! 因此合法深层输入可以正常解析，而真正的病态输入（如几十万层括号）仍由
//! [`MAX_STACK_SEGMENTS`] 兜底拒斥，避免无界占用内存。

use crate::Parser;

/// 单段栈的字节数（`maybe_grow` 的第二个参数）。
///
/// 按 debug 构建每层括号约数十 KB、一次折返（`parse_expression` →
/// `parse_primary` 等）约 8 个守卫估算，4 MiB 一段可容纳数百层；
/// 取值必须**显著大于** [`RED_ZONE`]，否则新分段一进来就再次低于红区，
/// 会逐层无限新开分段。
const STACK_SEGMENT: usize = 4 * 1024 * 1024;

/// 红区（堆栈余量，`maybe_grow` 的第一个参数）：剩余栈空间低于该值时提前切到新分段。
///
/// 取单段栈的 1/8，既能容纳一次折返的栈帧，又不至于频繁切换。
const RED_ZONE: usize = 512 * 1024;

/// 递归守卫次数上限（即「当前递归链」的最大深度）。
///
/// 计数按**守卫调用次数**而非源码层数：一层括号约经 8 次守卫
/// （`parse_expression` → `parse_assignment` → `parse_ternary` → 阶梯 → `parse_unary`
/// → `parse_primary`），故该值约对应十万层括号。`stacker` 只在剩余栈不足时
/// 才真正新开分段，因此该上限约束的是病态输入的内存占用，不影响合法输入。
pub(crate) const MAX_STACK_SEGMENTS: usize = 1 << 20;

impl Parser {
    /// 包裹一次可能递归的解析调用：栈不足时在当前调用点切换到新分段。
    ///
    /// 返回 `None` 表示已耗尽分段预算：病态输入不应继续下钻，
    /// 调用方按「此处应为期望项」自然失败，并触发外层恢复。
    ///
    /// 借用分割：`maybe_grow` 的闭包需要 `&mut Parser`（并递归进入更多
    /// `guarded`），而计数 `self.stack_segments` 也在 `self` 上。这里通过
    /// 局部变量自增、闭包结束后写回的方式避开「同一字段二次可变借用」。
    ///
    /// 计数语义是**当前递归链的深度**（进出成对，见下方写回），不是累计次数：
    /// 同一份源码里先后解析多段深层表达式时，前者退出后不得占用后者的预算。
    pub(crate) fn guarded<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> Option<T> {
        if self.stack_segments >= MAX_STACK_SEGMENTS {
            if !self.stack_exhausted {
                self.stack_exhausted = true;
                self.error_here("语法嵌套层级超出工程上限");
            }
            return None;
        }
        // 进入前的深度，退出时恢复（`f` 内可能提前 `?` 返回，故用局部变量而非裸字段）
        let entry = self.stack_segments;
        self.stack_segments = entry + 1;
        // 注意参数顺序：`maybe_grow(red_zone, stack_size, f)`——先红区、后新分段大小
        let result = stacker::maybe_grow(RED_ZONE, STACK_SEGMENT, || f(self));
        // 无论 `f` 内部走到了哪一层，都恢复到进入前的链深度
        self.stack_segments = entry;
        Some(result)
    }
}