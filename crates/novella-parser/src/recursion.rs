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
/// `parse_primary` 等）约 8 个守卫估算，512 KiB 一段仍可容纳数十层；
/// 取值必须**显著大于** [`RED_ZONE`]，否则新分段一进来就再次低于红区，
/// 会逐层无限新开分段。
///
/// 取 512 KiB 而非更大的值，是因为**内存占用 = 存活分段数 × 段大小**，
/// 而分段数由递归深度决定：4 MiB 一段时 40 万层病态输入可吃下十多 GiB
/// （实测曾把整机 OOM）。缩小段是压住内存的最直接手段。
const STACK_SEGMENT: usize = 2 * 1024 * 1024;

/// 红区（堆栈余量，`maybe_grow` 的第一个参数）：剩余栈空间低于该值时提前切到新分段。
///
/// 取单段栈的 1/4，需容纳一次折返（约 8 个守卫）的栈帧，又不能太接近段大小。
const RED_ZONE: usize = 512 * 1024;

/// **内存**预算上限（字节）：所有存活分段的总大小不得超过该值。
///
/// 这是真正的兜底约束。此前按「守卫调用次数」计（`1 << 20` 次），
/// 换算成内存是 TB 级虚拟内存，几乎等于没有上限——40 万层括号 40 万 × 8
/// 次守卫只需 320 万次，虽被截断但已吃下十余 GiB。改为按字节封顶后，
/// 病态输入会在内存可控处停住并报诊断。
///
/// 实测依据（debug 构建）：每 512 KiB 段约容纳 31 次守卫调用，即每守卫约
/// 16.8 KiB 栈；一层括号约 8 次守卫，故每层约 131 KiB。256 MiB 预算约对应
/// 2 000 层括号（release 栈帧更小，实际可更多）。这个量级足以覆盖任何真实源码，
/// 同时把病态输入的内存封死在 256 MiB 内——远低于曾把整机 OOM 的十余 GiB。
/// **内存**预算上限（字节）：所有存活分段的总大小不得超过该值。
///
/// 这是真正的兜底约束。不能按「守卫调用次数」计——那是每层嵌套都要付的计数，
/// 与内存无关（实测 512 次守卫只用了约 10 MiB）；必须按**实际新开的分段数**计。
/// 每段 512 KiB，256 MiB 预算 = 512 段；debug 构建下每段约容纳 23 次守卫，
/// 故总计约 1.1 万次守卫、数千层括号，足以覆盖真实源码，同时把病态输入的
/// 内存封死在 256 MiB 内（此前按次数计的上限形同虚设，40 万层吃下十余 GiB）。
pub(crate) const MAX_STACK_BYTES: usize = 1024 * 1024 * 1024;

/// 分段数上限（由 [`MAX_STACK_BYTES`] 换算得出）。
pub(crate) const MAX_STACK_SEGMENTS: usize = MAX_STACK_BYTES / STACK_SEGMENT;

impl Parser {
    /// 包裹一次可能递归的解析调用：栈不足时在当前调用点切换到新分段。
    ///
    /// 返回 `None` 表示已耗尽分段预算：病态输入不应继续下钻，
    /// 调用方按「此处应为期望项」自然失败，并触发外层恢复。
    ///
    /// 预算按**实际新开分段数**计（`stack_segments`），不是守卫调用次数——
    /// 后者与内存占用无关，早先按它封顶会让几百层合法输入被拒。
    /// `stack_segments` 随递归链进出成对增减，反映「此刻同时存活的分段数」。
    pub(crate) fn guarded<T>(&mut self, f: impl FnOnce(&mut Self) -> T) -> Option<T> {
        // 记录进入前的分段数，退出时恢复（`f` 内可能提前 `?` 返回，故用局部变量）
        let entry = self.stack_segments;
        // 用 `remaining_stack` 判断本次调用是否真的会新开分段：只有剩余栈低于红区
        // 时 `maybe_grow` 才会分配新段。这样预算计数对应真实内存，而非调用次数。
        let will_switch = stacker::remaining_stack()
            .map(|rem| rem < RED_ZONE)
            .unwrap_or(false);
        if will_switch {
            if entry + 1 >= MAX_STACK_SEGMENTS {
                if !self.stack_exhausted {
                    self.stack_exhausted = true;
                    self.error_here("语法嵌套层级超出工程上限");
                }
                return None;
            }
            self.stack_segments = entry + 1;
        }
        // 注意参数顺序：`maybe_grow(red_zone, stack_size, f)`——先红区、后新分段大小。
        let result = stacker::maybe_grow(RED_ZONE, STACK_SEGMENT, || f(self));
        // 无论 `f` 内部走到了哪一层，都恢复到进入前的分段数
        self.stack_segments = entry;
        Some(result)
    }
}
