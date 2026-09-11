lexer grammar NovellaLexer;

channels {
	ERROR
}

options {
	superClass = NovellaLexerBase;
}

// ====================== 注释 ======================
MultiLineComment: '/*' .*? '*/' -> channel(HIDDEN);
SingleLineComment:
	'//' ~[\r\n\u2028\u2029]* -> channel(HIDDEN);

// ====================== 标点符号和运算符 ======================
OpenBracket: '[';
CloseBracket: ']';
OpenParen: '(';
CloseParen: ')';
OpenBrace: '{' {self.ProcessOpenBrace()};
CloseBrace: '}' {self.ProcessCloseBrace()};
SemiColon: ';';
Comma: ',';
Assign: '=';
QuestionMark: '?';
Colon: ':';
Dot: '.';
PlusPlus: '++';
MinusMinus: '--';
Plus: '+';
Minus: '-';
BitNot: '~';
Not: '!';
Multiply: '*';
Divide: '/';
Modulus: '%';
Power: '**';
NullCoalesce: '??';
LeftShiftArithmetic: '<<';
RightShiftArithmetic: '>>';
LessThan: '<';
MoreThan: '>';
LessThanEquals: '<=';
GreaterThanEquals: '>=';
Equals: '==';
NotEquals: '!=';
IdentityEquals: '===';
IdentityNotEquals: '!==';
BitAnd: '&';
BitXOr: '^';
BitOr: '|';
And: '&&';
Or: '||';
MultiplyAssign: '*=';
DivideAssign: '/=';
ModulusAssign: '%=';
PlusAssign: '+=';
MinusAssign: '-=';
LeftShiftArithmeticAssign: '<<=';
RightShiftArithmeticAssign: '>>=';
BitAndAssign: '&=';
BitXorAssign: '^=';
BitOrAssign: '|=';
PowerAssign: '**=';
Arrow: '=>';
ReturnArrow: '->';
At: '@';

// ====================== 字面量 ======================
NullLiteral: 'null';
BooleanLiteral: 'true' | 'false';

// 数字字面量
DecimalLiteral:
	DecimalIntegerLiteral '.' [0-9] [0-9_]* ExponentPart?
	| '.' [0-9] [0-9_]* ExponentPart?
	| DecimalIntegerLiteral ExponentPart?;

HexIntegerLiteral: '0' [xX] [0-9a-fA-F] HexDigit*;
OctalIntegerLiteral: '0' [oO] [0-7] [_0-7]*;
BinaryIntegerLiteral: '0' [bB] [01] [_01]*;

// ====================== Novella 关键字 ======================

// 模块相关
From: 'from';
Import: 'import';

// 访问修饰符和变量声明
Pub: 'pub';
Pri: 'pri';
Const: 'const';
Let: 'let';
Static: 'static';

// 函数和类
Func: 'func';
Class: 'class';
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
Stream: 'stream';
Yield: 'yield';

// 宏系统
Macro: 'macro';

// 外部函数
Extern: 'extern';

// ====================== 标识符和字符串 ======================
Identifier: IdentifierStart IdentifierPart*;

StringLiteral:
	(
		'"' DoubleStringCharacter* '"'
		| '\'' SingleStringCharacter* '\''
	) {self.ProcessStringLiteral()};

FormatStringLiteral:
	(
		('f' | 'F') '"' DoubleStringCharacter* '"'
		| ('f' | 'F') '\'' SingleStringCharacter* '\''
	);

RawStringLiteral:
	(
		('r' | 'R') '"' RawDoubleStringCharacter* '"'
		| ('r' | 'R') '\'' RawSingleStringCharacter* '\''
	);

ByteStringLiteral:
	(
		('b' | 'B') '"' DoubleStringCharacter* '"'
		| ('b' | 'B') '\'' SingleStringCharacter* '\''
	);

// ====================== 空白字符和换行 ======================
WhiteSpaces: [\t\u000B\u000C\u0020\u00A0]+ -> channel(HIDDEN);
LineTerminator: [\r\n\u2028\u2029] -> channel(HIDDEN);

// ====================== 错误字符 ======================
UnexpectedCharacter: . -> channel(ERROR);

// ====================== 片段规则 ======================
fragment DoubleStringCharacter:
	~["\\\r\n]
	| '\\' EscapeSequence
	| LineContinuation;
fragment SingleStringCharacter:
	~['\\\r\n]
	| '\\' EscapeSequence
	| LineContinuation;
fragment EscapeSequence:
	CharacterEscapeSequence
	| '0'
	| HexEscapeSequence
	| UnicodeEscapeSequence;
fragment CharacterEscapeSequence:
	SingleEscapeCharacter
	| NonEscapeCharacter;
fragment HexEscapeSequence: 'x' HexDigit HexDigit;
fragment UnicodeEscapeSequence:
	'u' HexDigit HexDigit HexDigit HexDigit
	| 'u' '{' HexDigit HexDigit+ '}';
fragment SingleEscapeCharacter: ['"\\bfnrtv];
fragment NonEscapeCharacter: ~['"\\bfnrtv0-9xu\r\n];
fragment LineContinuation: '\\' [\r\n\u2028\u2029]+;
fragment HexDigit: [_0-9a-fA-F];
fragment DecimalIntegerLiteral: '0' | [1-9] [0-9_]*;
fragment ExponentPart: [eE] [+-]? [0-9_]+;
fragment IdentifierPart:
	IdentifierStart
	| [\p{Mn}]
	| [\p{Nd}]
	| [\p{Pc}]
	| '\u200C'
	| '\u200D';
fragment IdentifierStart:
	[\p{L}]
	| [$_]
	| '\\' UnicodeEscapeSequence;

fragment RawDoubleStringCharacter: ~["\\\r\n] | '\\' .;

fragment RawSingleStringCharacter: ~['\\\r\n] | '\\' .;