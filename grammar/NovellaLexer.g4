lexer grammar NovellaLexer;

// =============================================================================
// Novella 词法文法
//
// 对齐基准（唯一事实源）：
//   - crates/novella-core/src/token.rs    `TokenKind` 的 107 个词法变体（+ Eof / Error 合成项）
//   - crates/novella-lexer/src/lib.rs     logos 的 `RawToken` 规则与正则
//
// 规则约定：
//   - 本文件中的 token 规则名与 `TokenKind` 变体名逐一对应，顺序与 logos 声明顺序一致
//     （logos 与 ANTLR 的匹配策略同为「最长匹配优先，同长取声明在前者」）；
//   - 字面量（`'+'`、`';'` 等）刻意保留原文拼写，避免两处维护同一集合时漂移；
//   - 词法层不做值语义：字符串转义内容、数字进制等一律原样保留，交由语义分析阶段解析。
//
// 与旧参考文的差异（本文件同步移除的结构）：
//   - `SingleLineComment -> channel(HIDDEN)`：行注释改为正式 token `LineComment`
//     （供 LSP 高亮等消费），不再是隐藏通道 token；
//   - `superClass` / `ProcessOpenBrace` / `ProcessStringLiteral` 等动作：代码中不存在
//     对应的基类与回调，词法器亦不做花括号配对或字符串处理。
// =============================================================================

// 词法错误通道：与代码中「错误不进 token 流、只收进 errors 列表」等价
// （对应 `novella_core::token::TokenKind::Error` 与 `novella_lexer::LexError`）。
channels {
	ERROR
}

// ====================== 多字符运算符（必须先于同前缀的单字符） ======================
PlusPlus: '++';
MinusMinus: '--';
LeftShiftArithmetic: '<<';
RightShiftArithmetic: '>>';
LeftShiftArithmeticAssign: '<<=';
RightShiftArithmeticAssign: '>>=';
Equals: '==';
NotEquals: '!=';
IdentityEquals: '===';
IdentityNotEquals: '!==';
LessThanEquals: '<=';
GreaterThanEquals: '>=';
And: '&&';
Or: '||';
NullCoalesce: '??';
Arrow: '=>';
ReturnArrow: '->';
MultiplyAssign: '*=';
DivideAssign: '/=';
ModulusAssign: '%=';
PlusAssign: '+=';
MinusAssign: '-=';
BitAndAssign: '&=';
BitXorAssign: '^=';
BitOrAssign: '|=';

// ====================== 单字符运算符 / 分隔符 ======================
OpenBracket: '[';
CloseBracket: ']';
OpenParen: '(';
CloseParen: ')';
OpenBrace: '{';
CloseBrace: '}';
SemiColon: ';';
Comma: ',';
Assign: '=';
QuestionMark: '?';
Colon: ':';
Dot: '.';
Plus: '+';
Minus: '-';
BitNot: '~';
Not: '!';
Multiply: '*';
Divide: '/';
Modulus: '%';
LessThan: '<';
MoreThan: '>';
BitAnd: '&';
BitXOr: '^';
BitOr: '|';
At: '@';

// ====================== Novella 关键字 ======================
// 关键字一律硬保留：即使某个关键字当前没有对应语法结构（见文件末「未使用关键字」），
// 它在词法层仍是关键字（`TokenKind` 是唯一事实源）。

// 模块相关
From: 'from';
Import: 'import';

// 访问修饰符与变量声明
Pub: 'pub';
Pri: 'pri';
Pkg: 'pkg';
Const: 'const';
Let: 'let';
Static: 'static';
Weak: 'weak';

// 函数与类
Func: 'func';
Class: 'class';
Struct: 'struct';
Trait: 'trait';
New: 'new';
This: 'this';

// 控制流
If: 'if';
Else: 'else';
Elif: 'elif';
While: 'while';
For: 'for';
In: 'in';
Break: 'break';
Continue: 'continue';
Return: 'return';
Default: 'default';
Match: 'match';
Case: 'case';

KeyAnd: 'and';
KeyOr: 'or';
KeyNot: 'not';

// 错误处理
Try: 'try';
Catch: 'catch';
Finally: 'finally';
Throw: 'throw';

// 上下文管理器
With: 'with';
As: 'as';

// 类型系统
Type: 'type';
Is: 'is';

// 异步编程
Async: 'async';
Await: 'await';
Yield: 'yield';


// 外部函数
Extern: 'extern';

// 不安全上下文
Unsafe: 'unsafe';

// ====================== 字面量 ======================
Null: 'null';
BooleanLiteral: 'true' | 'false';

// 数字字面量：不做前导零限制，且字面量中的下划线仅作分隔符保留（值语义延后）
// 十六进制：`0x` / `0X` 后至少一位十六进制数字
HexIntegerLiteral: '0' [xX] [0-9a-fA-F] [0-9a-fA-F_]*;
// 八进制：`0o` / `0O` 后至少一位八进制数字
OctalIntegerLiteral: '0' [oO] [0-7] [0-7_]*;
// 二进制：`0b` / `0B` 后至少一位二进制数字
BinaryIntegerLiteral: '0' [bB] [01] [01_]*;
// 十进制：整数 / 小数 / 指数（`1`、`1.5`、`.5`、`1e10`、`1.5e-3`）；
// 小数点后必须跟随数字，故 `1.` 会切成 `1` 与 Dot
DecimalLiteral
	: [0-9] [0-9_]* ('.' [0-9] [0-9_]*)? ([eE] [+-]? [0-9] [0-9_]*)?
	| '.' [0-9] [0-9_]* ([eE] [+-]? [0-9] [0-9_]*)?;

// ====================== 字符串字面量 ======================
// 普通字符串（双引号 / 单引号）
StringLiteral: '"' DqStringChar* '"' | '\'' SqStringChar* '\'';
// 格式化字符串（f / F 前缀）
FormatStringLiteral: [fF] ('"' DqStringChar* '"' | '\'' SqStringChar* '\'');
// 原始字符串（r / R 前缀）
RawStringLiteral: [rR] ('"' DqStringChar* '"' | '\'' SqStringChar* '\'');
// 字节字符串（b / B 前缀）
ByteStringLiteral: [bB] ('"' DqStringChar* '"' | '\'' SqStringChar* '\'');

// ====================== 标识符 ======================
// 首位：下划线 / Unicode 字母 / `$`；后续：再加 Unicode 十进制数字（`\d` ≡ `\p{Nd}`）
Identifier: [_\p{L}$] [_\p{L}\p{Nd}$]*;

// ====================== 注释 ======================
// 行注释：`//` 到行尾，不含换行符本身（换行由 LineTerminator 吞掉）。
// 是正式 token（不是隐藏通道），与 `TokenKind::LineComment` 一致。
LineComment: '//' ~[\r\n\u2028\u2029]*;

// ====================== 空白与换行 ======================
// 代码中以 `#[logos(skip ...)]` 直接丢弃，故此处用 `-> skip`（而非隐藏通道）
WhiteSpaces: [\t\u000B\u000C\u0020\u00A0]+ -> skip;
LineTerminator: [\r\n\u2028\u2029] -> skip;

// ====================== 词法错误 ======================
// 与代码的两类 `LexError` 一一对应，均落在 ERROR 通道（对应 `TokenKind::Error`，
// 不进正式 token 流）。错误 token 的粒度与 logos 一致：
//
//   - 未闭合字符串（UnterminatedString）：起始引号 + 该字符串规则能消费的最长前缀，
//     整段作为**一个**错误 token。故反斜杠之后即使没有可转义字符（如 `"abc\`）
//     也被并入该 token；引号之后的部分照常按普通规则切分（`"oops` 中的 `oops` 会被吞掉）。
//   - 非法字符（InvalidCharacter）：任何规则的自动机都无法推进时的单字符（如 `#`）。
//
// 若前缀存在“更短但可接受”的匹配（如 `0xZ` 中的 `0`、`f"oops` 中的 `f`），
// logos 会回退到该接受态、不报错，这与 ANTLR 的最长匹配语义一致。
UnterminatedString
	: ('"' (DqStringChar | '\\')* | '\'' (SqStringChar | '\\')*) -> channel(ERROR);

// 兜底：无法匹配任何规则的单字符
UnexpectedCharacter: . -> channel(ERROR);

// ====================== 片段规则 ======================
// 字符串正文：非引号 / 非反斜杠 / 非换行字符，或反斜杠转义序列。
// 转义序列内容不做校验（`\\.`），与代码一致：任意非换行字符均被接受，
// 非法转义留待语义分析阶段报告。
fragment DqStringChar: ~["\\\r\n] | '\\' ~[\r\n];
fragment SqStringChar: ~['\\\r\n] | '\\' ~[\r\n];
