//! HIR 函数体结构：「块 → 语句/指令」两级。
//!
//! # 为什么不是表达式树
//!
//! AST 的表达式树里混着语法糖（括号、三元、赋值、成员访问、lambda…），
//! 还夹带着解析器为了消歧而维护的上下文状态。codegen 若直接遍历它，
//! 等于把求值顺序、类型推导、语法糖展开**重做一遍**。
//!
//! HIR 在构造期就把这些都做掉了：
//! - 求值顺序已经显式化为语句序列（`HirStmt` 逐条执行）；
//! - 每个中间值都落在 [`LocalId`] 上（codegen 只负责 `alloca` + `store`）；
//! - [`HirOperand`] 只表示**已经求好值**的东西，不含任何待求值表达式。
//!
//! 结果：codegen 只需 `for stmt in block.stmts { emit(stmt) }`，
//! 不需要递归下降，也不需要维护任何语义状态。

use novella_core::Span;
use serde::{Deserialize, Serialize};

use crate::ty::{FunctionSignature, HirFunctionId, HirType};

/// 局部变量标识：在**同一函数体内**唯一。
///
/// 构造期单调递增分配，codegen 用它建 `alloca` 并以 `LocalId` 反查；
/// 不做跨函数唯一（避免与寄存器分配耦合）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LocalId(pub u32);

/// 已求值的操作数（**不含待求值表达式**）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HirOperand {
    /// 读局部变量
    Local(LocalId),
    /// 整数常量（字面量已在构造期定型并求值，含进制前缀与下划线剥离）
    IntConst(i128),
    /// 未定型整数常量：值实施上下文定型（如 `let x: i64 = 1;` 的 `1`）。
    ///
    /// 携带源码区间是为了让 codegen 能报出确切位置 —— 值一旦被要求给出
    /// 具体类型而仍未定型，就是构造期的疏漏，必须 fail-fast 而不是猜。
    UntypedInt { value: i128, span: Span },
    /// 浮点常量
    FloatConst(f64),
    /// 未定型浮点常量（同 [`HirOperand::UntypedInt`]）
    UntypedFloat { value: f64, span: Span },
    /// 布尔常量
    BoolConst(bool),
    /// 函数引用（取函数地址 / 构造函数指针）
    Function(HirFunctionId),
    /// 空引用（`null` 字面量；已在构造期确定目标类型来自上下文，此处只表示空值）
    Null,
    /// 无值（`void` 调用结果、空返回）
    Unit,
    /// 自由函数值（指向函数的指针，可赋给函数类型变量 / 作为实参传递）
    FunctionPtr(HirFunctionId),
    /// 前提：条件 / 实参 / 返回值有时需要先执行一批指令（嵌套调用、短路展开）。
    ///
    /// 把「前置指令 + 后续语句」写成显式块，是为了让 codegen 不必猜
    /// ——它只按顺序发射块内语句，条件在块内部求值完成。
    /// 承载状态说明：`with self` 的降级会往操作数里塞「隐式接收者」，
    /// 该状态与具体指令形态绑定，因此用专用变体表达而不是加隐式参数。
    WithReceiver {
        /// 接收者（`self` 绑定的值）
        receiver: Box<HirOperand>,
        /// 加载接收者字段（`with` 体内对成员名的隐式解析）
        value: Box<HirOperand>,
    },
}

/// 块：语句序列。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HirBlock {
    pub stmts: Vec<HirStmt>,
}

impl HirBlock {
    /// 新建空块。
    pub fn new() -> Self {
        HirBlock::default()
    }

    /// 追加一条语句，便于构造期链式书写。
    pub fn push(&mut self, stmt: HirStmt) {
        self.stmts.push(stmt);
    }

    /// 是否为空块（codegen 可据此省掉空 basic block）。
    pub fn is_empty(&self) -> bool {
        self.stmts.is_empty()
    }
}

/// 语句：控制流结构 + 指令。
///
/// 控制流与指令分开命名，是为了让 codegen 的 `match` 一眼能看出
/// 哪些分支需要创建 basic block（`If` / `Loop`），哪些只是顺序发射。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HirStmt {
    /// 顺序指令
    Instr(HirInstr),
    /// 无条件块（`with` / `unsafe` / try 的块体先落到这里）
    Block(HirBlock),
    /// 分支：`cond` 在进入分支前求值完成
    If {
        cond: HirCond,
        then_block: HirBlock,
        else_block: Option<HirBlock>,
    },
    /// 带前测条件的循环（`while` / `for` / `for-in` 统一降级到这个形态）。
    ///
    /// `cond` 为 `None` 表示无条件循环（如 `for (;;)`；`break` 负责退出）。
    Loop {
        cond: Option<HirCond>,
        body: HirBlock,
        /// 每轮尾部执行；continue 直接跳到这里。
        step: HirBlock,
    },
    /// `break`：跳出最近的循环 / `Loop`
    Break,
    /// `continue`：进入最近循环的下一次迭代
    Continue,
    /// 完整表达式结束：逆序释放临时拥有值，具名绑定保留到词法作用域退出。
    /// 未执行分支中的槽位无需清理；已经清空的槽位可重复经过清理点。
    ReleaseTemps(Vec<LocalId>),
    /// 语法作用域标记；局部生命周期由结构化 Block / Loop 及 ReleaseTemps 表达。
    EnterScope,
    LeaveScope,
    /// 无操作（空语句 / 恒定真值替换后留下的空位），便于保持语句序列与源码一致
    Nop,
}

/// 顺序指令：每条都对应一次确定的运行期动作。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum HirInstr {
    /// 构造拥有 UTF-16 缓冲区的标准库 String 实例。
    StringLiteral {
        value: Vec<u16>,
        ty: HirType,
        dst: LocalId,
    },
    /// 取局部槽位地址，不复制局部值。
    LocalAddress {
        local: LocalId,
        ty: HirType,
        dst: LocalId,
    },
    /// target 为 struct 存储地址 / class 引用 / 指向聚合的指针。
    FieldAddress {
        target: HirOperand,
        target_ty: HirType,
        index: usize,
        ty: HirType,
        dst: LocalId,
    },
    /// 数组 / 指针元素地址；目标与索引已求值一次。
    IndexAddress {
        target: HirOperand,
        index: HirOperand,
        element_ty: HirType,
        dst: LocalId,
    },
    /// 向已求出的地址写值。
    StoreIndirect {
        address: HirOperand,
        value: HirOperand,
        ty: HirType,
    },
    /// 构造定型元组；元素按源码顺序求值后进入本指令。
    Tuple {
        elements: Vec<HirOperand>,
        ty: HirType,
        dst: LocalId,
    },
    /// 构造动态数组（长度等于 elements.len()，具体分配由运行时实现）。
    Array {
        elements: Vec<HirOperand>,
        element_ty: HirType,
        dst: LocalId,
    },
    /// 分配一个 class 实例，并在分配后调用其 `__init`。
    ///
    /// `init` 为空表示类型没有构造函数；接收者由 codegen 自动插入为新实例。
    New {
        ty: HirType,
        init: Option<FunctionSignature>,
        args: Vec<HirOperand>,
        dst: LocalId,
    },
    /// 取元组的已知位置元素，不涉及动态索引或边界推测。
    TupleExtract {
        target: HirOperand,
        index: usize,
        ty: HirType,
        dst: LocalId,
    },
    /// 数组长度（usize）。
    ArrayLength { target: HirOperand, dst: LocalId },
    /// 局部变量申请（函数入口或首次可见处），`init` 可选。
    Alloc {
        id: LocalId,
        ty: HirType,
        init: Option<HirOperand>,
    },
    /// 写入局部变量（`alloca` 已由 `Alloc` 建立）
    Store { id: LocalId, value: HirOperand },
    /// 读取局部变量到新局部（把操作数落成值，便于后续指令消费）
    Load { dst: LocalId, src: LocalId },
    /// 函数调用。`dst` 为 `None` 表示结果丢弃；`dst_ty` 为 `None` 表示 void 返回。
    Call {
        callee: HirOperand,
        args: Vec<HirOperand>,
        dst: Option<LocalId>,
        dst_ty: Option<HirType>,
    },
    /// 一元运算（`dst_ty` 决定具体指令，如浮点取负 vs 整数取负）
    Unary {
        op: HirUnaryOp,
        operand: HirOperand,
        dst: LocalId,
        dst_ty: HirType,
    },
    /// 二元运算（输入类型决定有 / 无符号与浮点变体）
    Binary {
        op: HirBinaryOp,
        lhs: HirOperand,
        rhs: HirOperand,
        /// 输入类型不能从比较的 bool 结果反推，必须显式携带。
        lhs_ty: HirType,
        rhs_ty: HirType,
        dst: LocalId,
        dst_ty: HirType,
    },
    /// 类型转换（隐式提升 / 显式 `as`），`to` 为目标类型
    Cast {
        operand: HirOperand,
        from: HirType,
        to: HirType,
        dst: LocalId,
    },
    /// `is` 类型判定：只对 class / trait / struct 的名义键有意义
    TypeTest {
        operand: HirOperand,
        /// 输入类型用于静态类型判定或选择运行时类型检查。
        operand_ty: HirType,
        /// 判定目标类型
        target: HirType,
        dst: LocalId,
    },
    /// 取字段值：`target.field`（`target` 为带 `receiver` 的指针值）
    FieldLoad {
        target: HirOperand,
        /// 目标类型（必须是 struct / class 的名义键，codegen 据此查布局）
        target_ty: HirType,
        /// 字段名
        field: String,
        /// 字段下标（按声明顺序，与 [`crate::item::HirField`] 一致）
        index: usize,
        /// 字段类型（= 结果类型）
        ty: HirType,
        dst: LocalId,
    },
    /// 写字段值：`target.field = value`
    FieldStore {
        target: HirOperand,
        target_ty: HirType,
        field: String,
        index: usize,
        value: HirOperand,
        /// 强字段管理新旧强引用；weak 字段改为管理控制块弱计数。
        is_weak: bool,
    },
    /// 取元素：`target[index]`（`target` 为指针 / 数组值）
    IndexLoad {
        target: HirOperand,
        /// 元素类型（= 结果类型）
        element_ty: HirType,
        index: HirOperand,
        dst: LocalId,
    },
    /// 写元素：`target[index] = value`
    IndexStore {
        target: HirOperand,
        element_ty: HirType,
        index: HirOperand,
        value: HirOperand,
    },
    /// 返回（`None` 表示 void 返回）
    Return(Option<HirOperand>),
}

/// 一元运算：语义阶段已确定操作数类型，此处只需运算符本身。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HirUnaryOp {
    /// 恒等（源码 `+x`）：codegen 可直接把操作数原样搬运，或省掉该指令
    Identity,
    /// 取地址（`&expr`）：只在 `unsafe` 上下文外允许，语义阶段已校验
    AddressOf,
    /// 解引用（`*expr`）：只在 `unsafe` 上下文中出现，语义阶段已校验
    Deref,
    /// 数值取负
    Neg,
    /// 逻辑取反
    Not,
    /// 按位取反
    BitNot,
}

/// 条件：求值所需的语句 + 最终值。
///
/// 「先执行、后判断」被显式建模，是为了让循环条件每轮重新求值时
/// 前置语句也跟着每轮执行（把前置指令提前发射会造成只求值一次的 bug）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HirCond {
    /// 判断前必须完成的语句（通常为空）
    pub prelude: HirBlock,
    /// 条件值
    pub value: HirOperand,
}

impl HirCond {
    /// 无前置语句的条件（最常见形态）。
    pub fn new(value: HirOperand) -> Self {
        HirCond {
            prelude: HirBlock::new(),
            value,
        }
    }

    /// 是否带前置语句（codegen 可据此跳过空块的额外 basic block）。
    pub fn has_prelude(&self) -> bool {
        !self.prelude.is_empty()
    }
}

/// 二元运算：**类型信息已在操作数上**（`dst` 的 `HirType` 决定具体指令），
/// 因此这里不再区分 float / signed / unsigned 的变体。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum HirBinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Shl,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    /// 相等比较
    Eq,
    /// 不等
    Ne,
    /// 数值比较，操作数决定有 / 无符号
    Lt,
    Le,
    Gt,
    Ge,
    /// 逻辑与（短路已在构造期展开成控制流，这里的操作数已求值）
    LogAnd,
    /// 逻辑或（同上）
    LogOr,
    /// 指针差（结果类型为整数）
    PtrDiff,
}
