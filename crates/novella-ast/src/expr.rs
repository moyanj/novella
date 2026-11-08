use serde::{Deserialize, Serialize};

use novella_core::Span;

use crate::{
    Spanned,
    stmt::Block,
    types::{Param, TypeExpr},
};

/// 字面量（对应文法 `literal`）。
///
/// 字符串与数字均保留源码原文（含 `f`/`r`/`b` 前缀与进制前缀），
/// 具体值语义在语义分析阶段解析。字面量是纯值不携带位置，
/// 其源码区间由外层 `Expression::Literal` 的 `span` 承载。
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

/// 对象字面量的属性赋值（对应文法 `propertyAssignment`）
///
/// `{ a: 1, b }` 中的 `b` 为简写形态：`value` 为 None，
/// 语义等价于 `b: b`，由语义分析阶段展开。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct PropertyAssignment {
    /// 属性名
    pub name: String,
    /// 属性值表达式；None 表示简写形态 `{foo}`（等价 `foo: foo`）
    pub value: Option<Expression>,
    /// 属性赋值覆盖的源码区间
    pub span: Span,
}

/// lambda 参数形态（对应文法 `lambdaSignature`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum LambdaParams {
    /// 单参数简写：`x => ...`
    Single {
        /// 参数名
        name: String,
        /// 参数标识符的源码区间
        span: Span,
    },
    /// 参数列表：`(a, b) => ...`（对应 `parameterList`）
    List {
        /// 形参列表
        params: Vec<Param>,
        /// 括号参数列表的源码区间
        span: Span,
    },
}

/// lambda 函数体（对应文法 `lambdaExpression` 的 `block | expression`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum LambdaBody {
    /// 表达式体：`=> a + b`
    Expr {
        /// 表达式
        expr: Box<Expression>,
        /// 表达式体的源码区间
        span: Span,
    },
    /// 语句块体：`=> { ... }`
    Block {
        /// 语句块
        block: Block,
        /// 语句块体的源码区间
        span: Span,
    },
}

/// lambda 表达式（对应文法
/// `lambdaExpression: Async? lambdaSignature typeAnnotation? Arrow (block | expression)`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct Lambda {
    /// 是否为异步 lambda（对应文法 `Async?`）
    pub is_async: bool,
    /// 参数形态（单标识符简写或参数列表）
    pub params: LambdaParams,
    /// 返回类型注解（对应文法 `typeAnnotation?`），None 表示缺省
    pub return_type: Option<TypeExpr>,
    /// 函数体（表达式或语句块）
    pub body: LambdaBody,
    /// lambda 表达式覆盖的源码区间
    pub span: Span,
}

/// match 分支的模式（对应文法 `matchCase` 的 `expression | typeAnnotation`）
///
/// `typeAnnotation` 即 `: TypeExpr` 形态，故 `case : T => ...` 表示按类型匹配。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum MatchPattern {
    /// 表达式模式：`case value => ...`
    Expr {
        /// 模式表达式
        expr: Box<Expression>,
        /// 模式表达式的源码区间
        span: Span,
    },
    /// 类型注解模式：`case : T => ...`
    Type {
        /// 模式类型
        ty: TypeExpr,
        /// 类型注解的源码区间
        span: Span,
    },
}

/// match 分支的函数体（对应文法 `matchCase` / `matchDefault` 的 `'=>' expression | block`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum MatchCaseBody {
    /// 表达式体：`=> value`
    Expr {
        /// 表达式
        expr: Box<Expression>,
        /// 表达式体的源码区间
        span: Span,
    },
    /// 语句块体：`{ ... }`
    Block {
        /// 语句块
        block: Block,
        /// 语句块体的源码区间
        span: Span,
    },
}

/// match 分支（对应文法 `matchCase`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct MatchCase {
    /// 匹配模式
    pub pattern: MatchPattern,
    /// 命中后的分支体
    pub body: MatchCaseBody,
    /// 分支整体（`case ... => ...`）覆盖的源码区间
    pub span: Span,
}

/// match 表达式（对应文法
/// `matchExpression: Match expression '{' matchCase+ matchDefault? '}'`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub struct Match {
    /// 被匹配的表达式（`match` 后的 `expression`）
    pub subject: Box<Expression>,
    /// 具名分支列表（对应 `matchCase+`）
    pub cases: Vec<MatchCase>,
    /// 兜底分支（对应 `matchDefault?`），None 表示没有 `default`
    pub default: Option<MatchCaseBody>,
    /// match 表达式覆盖的源码区间
    pub span: Span,
}

/// 表达式（对应文法 `expression`）。
///
/// 节点扁平化设计：一元 / 二元 / 赋值等运算各自成节点，
/// 递归字段用 `Box` 承载以避免无限大小，运算符以枚举集中定义
/// （与词法 token 一一对应，供 parser 直接映射）。
/// 每个变体携带 `span` 记录表达式覆盖的源码区间（供诊断定位），
/// 由 `Spanned` 派生统一生成 [`Expression::span`]。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Spanned)]
pub enum Expression {
    // —— 原子表达式 ——
    /// 字面量：`42`、`"hello"`、`true`、`null` 等（对应 `LiteralExpr`）
    Literal {
        /// 字面量值（保留源码原文）
        value: Literal,
        /// 字面量的源码区间
        span: Span,
    },
    /// 标识符引用：`foo`（对应 `IdentifierExpr`）
    Identifier {
        /// 标识符名
        name: String,
        /// 标识符的源码区间
        span: Span,
    },
    /// 括号表达式：`(a)`（对应 `ParenExpr`；注意与元组 `(a, b)` 区分）
    Paren {
        /// 括号内的表达式
        expr: Box<Expression>,
        /// 含括号的源码区间
        span: Span,
    },
    /// 元组表达式：`(0, 1)`（对应 `TupleExpr`）
    Tuple {
        /// 元组元素列表
        elements: Vec<Expression>,
        /// 元组的源码区间
        span: Span,
    },
    /// 数组字面量：`[1, 2, 3]`（对应 `ArrayLiteralExpr`）
    ArrayLiteral {
        /// 数组元素列表
        elements: Vec<Expression>,
        /// 数组字面量的源码区间
        span: Span,
    },
    /// 对象字面量：`{ a: 1, b }`（对应 `ObjectLiteralExpr`）
    ObjectLiteral {
        /// 属性赋值列表
        properties: Vec<PropertyAssignment>,
        /// 对象字面量的源码区间
        span: Span,
    },
    /// this 引用（对应 `ThisExpr`）
    This {
        /// `this` 的源码区间
        span: Span,
    },

    // —— 一元运算 ——
    /// 一元运算：`-x`、`!ok`、`i++` 等（对应 `PrefixExpr` / `PostfixExpr`）
    Unary {
        /// 运算符
        op: UnaryOp,
        /// 操作数
        operand: Box<Expression>,
        /// 一元表达式覆盖的源码区间
        span: Span,
    },
    /// await 表达式：`await p`（对应 `AwaitExpr`）
    Await {
        /// 被等待的表达式
        expr: Box<Expression>,
        /// await 表达式覆盖的源码区间
        span: Span,
    },

    // —— 二元运算 ——
    /// 二元运算：`a + b`、`x && y` 等（对应各二元运算产生式）
    Binary {
        /// 运算符
        op: BinaryOp,
        /// 左操作数
        left: Box<Expression>,
        /// 右操作数
        right: Box<Expression>,
        /// 二元表达式覆盖的源码区间
        span: Span,
    },
    /// 类型判断：`x is string`（对应 `IsExpr`；右操作数是类型表达式而非值表达式）
    Is {
        /// 被判断的表达式
        operand: Box<Expression>,
        /// 目标类型（对应 `typeExpression`）
        ty: TypeExpr,
        /// 类型判断表达式覆盖的源码区间
        span: Span,
    },
    /// 类型转换：`expr as Type`（对应 `CastExpr`，复用 `As` token）。
    ///
    /// 指针相关转换（指针↔整数、指针↔指针）须位于 unsafe 上下文，
    /// 纯数值转换不受限（design.md §6.3）。
    Cast {
        /// 被转换的表达式
        expr: Box<Expression>,
        /// 目标类型
        ty: TypeExpr,
        /// 转换表达式覆盖的源码区间
        span: Span,
    },
    /// struct 字段初始化构造：`Point{ x: 1, y: 2 }`（对应 `StructInitExpr`）。
    ///
    /// 复用 `objectLiteral` 的字段赋值形态；每个字段须可初始化（显式给出或
    /// 取字段声明 `initializer` 默认值）；无位置参数构造（design.md §5.4）。
    StructInit {
        /// 被构造的 struct 类型名
        name: String,
        /// 字段初始化列表
        properties: Vec<PropertyAssignment>,
        /// 构造表达式覆盖的源码区间
        span: Span,
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
        /// 三元表达式覆盖的源码区间
        span: Span,
    },
    /// 赋值表达式：`a = 1`、`a += 2` 等（对应 `AssignmentExpr`）
    Assignment {
        /// 赋值目标（通常为标识符 / 成员访问 / 索引访问）
        target: Box<Expression>,
        /// 赋值运算符
        op: AssignOp,
        /// 被赋的值
        value: Box<Expression>,
        /// 赋值表达式覆盖的源码区间
        span: Span,
    },

    // —— 访问与调用 ——
    /// 成员访问：`obj.field`（对应 `MemberAccessExpr`）
    MemberAccess {
        /// 被访问的目标表达式
        target: Box<Expression>,
        /// 成员名
        name: String,
        /// 成员访问表达式覆盖的源码区间
        span: Span,
    },
    /// 索引访问：`arr[0]`（对应 `IndexAccessExpr`）
    IndexAccess {
        /// 被索引的目标表达式
        target: Box<Expression>,
        /// 索引表达式
        index: Box<Expression>,
        /// 索引访问表达式覆盖的源码区间
        span: Span,
    },
    /// 函数调用：`foo<T>(a, b)`（对应 `CallExpr`，含可选的 `typeGeneric?`）
    Call {
        /// 被调用的表达式（通常是标识符或成员访问）
        callee: Box<Expression>,
        /// 泛型实参列表（对应 `typeGeneric` 的 `typeArgumentList`），无泛型时为空
        type_args: Vec<TypeExpr>,
        /// 实参列表（对应 `argumentList`）
        args: Vec<Expression>,
        /// 调用表达式覆盖的源码区间
        span: Span,
    },
    /// 对象构造：`new Foo(1)`（对应 `NewExpr`）。
    ///
    /// 构造目标是 `new` 之后的整条后缀表达式（标识符链 / 括号 / 调用）；
    /// 若该表达式整体是一次调用，则其 callee 作为 `target`、`type_args` / `args`
    /// 提升为构造实参（文法 `NewExpr` 注释的约定），否则实参为空。
    New {
        /// 被构造的目标表达式
        target: Box<Expression>,
        /// 泛型实参列表（`new Foo<int>(1)` 的 `<int>`），无泛型时为空
        type_args: Vec<TypeExpr>,
        /// 构造实参列表
        args: Vec<Expression>,
        /// 构造表达式覆盖的源码区间
        span: Span,
    },
    /// 宏调用：`print!(x)`（对应 `MacroCallExpr`）
    MacroCall {
        /// 宏名表达式
        callee: Box<Expression>,
        /// 宏实参列表
        args: Vec<Expression>,
        /// 宏调用表达式覆盖的源码区间
        span: Span,
    },

    // —— 高级表达式 ——
    /// match 表达式：`match x { ... }`（对应 `MatchExpr`）
    Match(Match),
    /// lambda 表达式：`(a, b) => a + b`（对应 `LambdaExpr`）
    Lambda(Lambda),
}
