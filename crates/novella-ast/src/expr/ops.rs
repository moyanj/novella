//! 运算符枚举：一元 / 二元 / 赋值，与词法 token 一一对应，供 parser 直接映射。

use serde::{Deserialize, Serialize};

/// 一元运算符（对应文法 `PrefixExpr` 与 `PostfixExpr`）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum UnaryOp {
    /// 一元正号：`+`（对应 `PrefixExpr`）
    Plus,
    /// 一元负号：`-`（对应 `PrefixExpr`）
    Minus,
    /// 逻辑非：`!` / `not`（对应 `PrefixExpr`；关键字 `not` 与符号 `!` 在词法层已统一）
    Not,
    /// 按位取反：`~`（对应 `PrefixExpr`）
    BitNot,
    /// 后缀自增：`++`（对应 `PostfixExpr`）
    PostIncrement,
    /// 后缀自减：`--`（对应 `PostfixExpr`）
    PostDecrement,
    /// 取地址：`&x`（对应文法 `'&' expression` 前缀备选，复用 `BitAnd`）。
    /// 只允许对左值（变量 / 字段 / 数组元素 / 解引用结果）取址；安全操作。
    AddressOf,
    /// 解引用：`*x`（对应文法 `'*' expression` 前缀备选，复用 `Multiply`）。
    /// 必须位于 unsafe 上下文；`**x` 词法即两个 `*`，无拆分特例。
    DerefPointer,
}

/// 二元运算符（对应文法各二元运算产生式）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinaryOp {
    /// 乘法：`*`（对应 `MultiplicativeExpr`）
    Multiply,
    /// 除法：`/`（对应 `MultiplicativeExpr`）
    Divide,
    /// 取模：`%`（对应 `MultiplicativeExpr`）
    Modulus,
    /// 加法：`+`（对应 `AdditiveExpr`）
    Plus,
    /// 减法：`-`（对应 `AdditiveExpr`）
    Minus,
    /// 左移：`<<`（对应 `ShiftExpr`）
    LeftShift,
    /// 右移：`>>`（对应 `ShiftExpr`）
    RightShift,
    /// 小于：`<`（对应 `RelationalExpr`）
    LessThan,
    /// 大于：`>`（对应 `RelationalExpr`）
    GreaterThan,
    /// 小于等于：`<=`（对应 `RelationalExpr`）
    LessThanEquals,
    /// 大于等于：`>=`（对应 `RelationalExpr`）
    GreaterThanEquals,
    /// 相等：`==`（对应 `EqualityExpr`）
    Equals,
    /// 不等：`!=`（对应 `EqualityExpr`）
    NotEquals,
    /// 严格相等：`===`（对应 `EqualityExpr`）
    IdentityEquals,
    /// 严格不等：`!==`（对应 `EqualityExpr`）
    IdentityNotEquals,
    /// 按位与：`&`（对应 `BitAndExpr`）
    BitAnd,
    /// 按位异或：`^`（对应 `BitXorExpr`）
    BitXor,
    /// 按位或：`|`（对应 `BitOrExpr`）
    BitOr,
    /// 逻辑与：`&&` / `and`（对应 `LogicalAndExpr`）
    LogicalAnd,
    /// 逻辑或：`||` / `or`（对应 `LogicalOrExpr`）
    LogicalOr,
    /// 空值合并：`??`（对应 `CoalesceExpr`）
    NullCoalesce,
}

/// 赋值运算符（对应文法 `AssignmentExpr`，按文法复合赋值出现顺序排列）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AssignOp {
    /// 简单赋值：`=`
    Assign,
    /// 加后赋值：`+=`
    PlusAssign,
    /// 减后赋值：`-=`
    MinusAssign,
    /// 乘后赋值：`*=`
    MultiplyAssign,
    /// 除后赋值：`/=`
    DivideAssign,
    /// 取模后赋值：`%=`
    ModulusAssign,
    /// 按位与后赋值：`&=`
    BitAndAssign,
    /// 按位或后赋值：`|=`
    BitOrAssign,
    /// 按位异或后赋值：`^=`
    BitXorAssign,
    /// 左移后赋值：`<<=`
    LeftShiftAssign,
    /// 右移后赋值：`>>=`
    RightShiftAssign,
}
