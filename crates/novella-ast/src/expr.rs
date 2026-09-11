use serde::{Deserialize, Serialize};

use crate::{
    stmt::Block,
    types::{Param, TypeExpr},
};

/// 字面量（对应文法 `literal`）。
///
/// 字符串与数字均保留源码原文（含 `f`/`r`/`b` 前缀与进制前缀），
/// 具体值语义在语义分析阶段解析。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Literal {
    /// `null`
    Null,
    /// `true` / `false`
    Boolean(bool),
    /// 字符串字面量（普通 / 格式化 / 原始 / 字节串，保留原文）
    String(String),
    /// 数字字面量（十进制 / 十六进制 / 八进制 / 二进制，保留原文，如 `0`、`1.5`、`0xFF`）
    Number(String),
}

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
}

/// 二元运算符（对应文法各二元运算产生式）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BinaryOp {
    /// 幂运算：`**`（对应 `PowerExpr`）
    Power,
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
    /// 幂后赋值：`**=`
    PowerAssign,
}

/// 对象字面量的属性赋值（对应文法 `propertyAssignment`）
///
/// `{ a: 1, b }` 中的 `b` 为简写形态：`value` 为 None，
/// 语义等价于 `b: b`，由语义分析阶段展开。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropertyAssignment {
    /// 属性名
    pub name: String,
    /// 属性值表达式；None 表示简写形态 `{foo}`（等价 `foo: foo`）
    pub value: Option<Expression>,
}

/// lambda 参数形态（对应文法 `lambdaSignature`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LambdaParams {
    /// 单参数简写：`x => ...`
    Single(String),
    /// 参数列表：`(a, b) => ...`（对应 `parameterList`）
    List(Vec<Param>),
}

/// lambda 函数体（对应文法 `lambdaExpression` 的 `block | expression`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum LambdaBody {
    /// 表达式体：`=> a + b`
    Expr(Box<Expression>),
    /// 语句块体：`=> { ... }`
    Block(Block),
}

/// lambda 表达式（对应文法
/// `lambdaExpression: Async? lambdaSignature typeAnnotation? Arrow (block | expression)`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lambda {
    /// 是否为异步 lambda（对应文法 `Async?`）
    pub is_async: bool,
    /// 参数形态（单标识符简写或参数列表）
    pub params: LambdaParams,
    /// 返回类型注解（对应文法 `typeAnnotation?`），None 表示缺省
    pub return_type: Option<TypeExpr>,
    /// 函数体（表达式或语句块）
    pub body: LambdaBody,
}

/// match 分支的模式（对应文法 `matchCase` 的 `expression | typeAnnotation`）
///
/// `typeAnnotation` 即 `: TypeExpr` 形态，故 `case : T => ...` 表示按类型匹配。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MatchPattern {
    /// 表达式模式：`case value => ...`
    Expr(Box<Expression>),
    /// 类型注解模式：`case : T => ...`
    Type(TypeExpr),
}

/// match 分支的函数体（对应文法 `matchCase` / `matchDefault` 的 `'=>' expression | block`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum MatchCaseBody {
    /// 表达式体：`=> value`
    Expr(Box<Expression>),
    /// 语句块体：`{ ... }`
    Block(Block),
}

/// match 分支（对应文法 `matchCase`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MatchCase {
    /// 匹配模式
    pub pattern: MatchPattern,
    /// 命中后的分支体
    pub body: MatchCaseBody,
}

/// match 表达式（对应文法
/// `matchExpression: Match expression '{' matchCase+ matchDefault? '}'`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Match {
    /// 被匹配的表达式（`match` 后的 `expression`）
    pub subject: Box<Expression>,
    /// 具名分支列表（对应 `matchCase+`）
    pub cases: Vec<MatchCase>,
    /// 兜底分支（对应 `matchDefault?`），None 表示没有 `default`
    pub default: Option<MatchCaseBody>,
}

/// 表达式（对应文法 `expression`）。
///
/// 节点扁平化设计：一元 / 二元 / 赋值等运算各自成节点，
/// 递归字段用 `Box` 承载以避免无限大小，运算符以枚举集中定义
/// （与词法 token 一一对应，供 parser 直接映射）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Expression {
    // —— 原子表达式 ——
    /// 字面量：`42`、`"hello"`、`true`、`null` 等（对应 `LiteralExpr`）
    Literal(Literal),
    /// 标识符引用：`foo`（对应 `IdentifierExpr`）
    Identifier(String),
    /// 括号表达式：`(a)`（对应 `ParenExpr`；注意与元组 `(a, b)` 区分）
    Paren(Box<Expression>),
    /// 元组表达式：`(0, 1)`（对应 `TupleExpr`）
    Tuple(Vec<Expression>),
    /// 数组字面量：`[1, 2, 3]`（对应 `ArrayLiteralExpr`）
    ArrayLiteral(Vec<Expression>),
    /// 对象字面量：`{ a: 1, b }`（对应 `ObjectLiteralExpr`）
    ObjectLiteral(Vec<PropertyAssignment>),
    /// this 引用（对应 `ThisExpr`）
    This,

    // —— 一元运算 ——
    /// 一元运算：`-x`、`!ok`、`i++` 等（对应 `PrefixExpr` / `PostfixExpr`）
    Unary {
        /// 运算符
        op: UnaryOp,
        /// 操作数
        operand: Box<Expression>,
    },
    /// await 表达式：`await p`（对应 `AwaitExpr`）
    Await(Box<Expression>),

    // —— 二元运算 ——
    /// 二元运算：`a + b`、`x && y` 等（对应各二元运算产生式）
    Binary {
        /// 运算符
        op: BinaryOp,
        /// 左操作数
        left: Box<Expression>,
        /// 右操作数
        right: Box<Expression>,
    },
    /// 类型判断：`x is string`（对应 `IsExpr`；右操作数是类型表达式而非值表达式）
    Is {
        /// 被判断的表达式
        operand: Box<Expression>,
        /// 目标类型（对应 `typeExpression`）
        ty: TypeExpr,
    },

    // —— 三元与赋值 ——
    /// 三元条件表达式：`a ? b : c`（对应 `TernaryExpr`）
    Ternary {
        /// 条件
        condition: Box<Expression>,
        /// 条件为真时的值
        then_expr: Box<Expression>,
        /// 条件为假时的值
        else_expr: Box<Expression>,
    },
    /// 赋值表达式：`a = 1`、`a += 2` 等（对应 `AssignmentExpr`）
    Assignment {
        /// 赋值目标（通常为标识符 / 成员访问 / 索引访问）
        target: Box<Expression>,
        /// 赋值运算符
        op: AssignOp,
        /// 被赋的值
        value: Box<Expression>,
    },

    // —— 访问与调用 ——
    /// 成员访问：`obj.field`（对应 `MemberAccessExpr`）
    MemberAccess {
        /// 被访问的目标表达式
        target: Box<Expression>,
        /// 成员名
        name: String,
    },
    /// 索引访问：`arr[0]`（对应 `IndexAccessExpr`）
    IndexAccess {
        /// 被索引的目标表达式
        target: Box<Expression>,
        /// 索引表达式
        index: Box<Expression>,
    },
    /// 函数调用：`foo<T>(a, b)`（对应 `CallExpr`，含可选的 `typeGeneric?`）
    Call {
        /// 被调用的表达式（通常是标识符或成员访问）
        callee: Box<Expression>,
        /// 泛型实参列表（对应 `typeGeneric` 的 `typeArgumentList`），无泛型时为空
        type_args: Vec<TypeExpr>,
        /// 实参列表（对应 `argumentList`）
        args: Vec<Expression>,
    },
    /// 对象构造：`new Foo(1)`（对应 `NewExpr`）
    New {
        /// 被构造的目标表达式
        target: Box<Expression>,
        /// 构造实参列表
        args: Vec<Expression>,
    },
    /// 宏调用：`print!(x)`（对应 `MacroCallExpr`）
    MacroCall {
        /// 宏名表达式
        callee: Box<Expression>,
        /// 宏实参列表
        args: Vec<Expression>,
    },

    // —— 高级表达式 ——
    /// match 表达式：`match x { ... }`（对应 `MatchExpr`）
    Match(Match),
    /// lambda 表达式：`(a, b) => a + b`（对应 `LambdaExpr`）
    Lambda(Lambda),
}
