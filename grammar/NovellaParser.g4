parser grammar NovellaParser;

options {
	tokenVocab = NovellaLexer;
	superClass = NovellaParserBase;
}

// ====================== 程序结构 ======================
program: sourceElements? EOF;

sourceElements: sourceElement+;

sourceElement:
	moduleImport
	| typeAliasDeclaration
	| traitDeclaration
	| classDeclaration
	| functionDeclaration
	| externFunctionDeclaration
	| macroDeclaration
	| decoratorStatement
	| statement;

// ====================== 模块系统 ======================
moduleImport: fromImport | directImport;

fromImport:
	From StringLiteral Import identifier (',' identifier)* SemiColon;

directImport: Import StringLiteral SemiColon;

// ====================== 装饰器 ======================
// decoratorStatement 有两种形态：
//   - decorator SemiColon —— 独立装饰器语句，如 @foo;
//   - decorator block     —— 装饰块：装饰器直接修饰一个语句块，如 @cfg(feature = "limine") { ... }
decoratorStatement: decorator (SemiColon | block);

// 语义上等价 Rust 的 #[name(...)]：@name(args) 或 @name 简写形式。
// 括号内为自由 token 流（任意词法单元，允许嵌套括号），原样保留到 AST 的
// Decorator.tokens，结构化解析（如 @cfg(target_os = "linux")）延后到语义分析阶段。
decorator:
	At identifier (
		'(' decoratorToken* ')'
	)?;

// 装饰器括号内的自由 token：匹配任意词法单元（含括号、字面量、标识符等）。
decoratorToken: .;

// ====================== 类型系统 ======================
typeAliasDeclaration:
	Type identifier typeParameters? Assign typeExpression SemiColon;

typeParameters: '<' typeParameterList '>';

typeParameterList: typeParameter (',' typeParameter)*;

typeParameter: identifier (Colon typeExpression)?;

typeExpression:
	arrowFunctionType
	| primaryType
	| functionType
	| typeExpression '&' typeExpression
	| typeExpression '|' typeExpression;

primaryType:
	'(' typeList? ')'		# TupleTypeCase
	| predefinedType		# PredefinedTypeCase
	| typeReference			# ReferenceTypeCase
	| primaryType '[' ']'	# ArrayTypeCase
	| objectType			# ObjectTypeCase;

objectType: '{' (objectTypeMember (';' | ',')?)* '}';

objectTypeMember: identifier typeAnnotation;

arrowFunctionType:
	typeParameters? '(' typeList? ')' ReturnArrow typeExpression;

typeList: typeExpression (',' typeExpression)*;

predefinedType: BooleanLiteral | StringLiteral | NullLiteral;

typeReference: identifier typeGeneric?;

typeGeneric: '<' typeArgumentList '>';

typeArgumentList: typeExpression (',' typeExpression)*;

functionType:
	Func typeParameters? '(' parameterList? ')' ReturnArrow typeExpression;

typeAnnotation: Colon typeExpression;

// ====================== trait 与类声明 ======================
traitDeclaration:
	decorator* Trait identifier typeParameters? traitBody;

traitBody: '{' traitMember* '}';

traitMember: methodDeclaration | fieldDeclaration;

classDeclaration:
	decorator* Pub? Class identifier typeParameters? classHeritage? classBody;

classHeritage: '(' typeReference ')';

classBody: '{' classMember* '}';

classMember: methodDeclaration | fieldDeclaration;

// ====================== 成员声明 ======================
fieldDeclaration:
	decorator* accessModifier? Static? identifier typeAnnotation? (
		Assign expression
	)? SemiColon;

methodDeclaration:
	decorator* accessModifier? Static? Func identifier typeParameters? '(' parameterList? ')'
		typeAnnotation? ReturnArrow? typeExpression? (
		block
		| SemiColon
	);

accessModifier: Pub | Pri;

// ====================== 函数与宏声明 ======================
functionDeclaration:
	decorator* Pub? Func identifier typeParameters? '(' parameterList? ')' typeAnnotation?
		ReturnArrow? typeExpression? block;

parameterList: parameter (',' parameter)*;

parameter: identifier typeAnnotation? (Assign expression)?;

macroDeclaration:
	Macro identifier '!' '(' parameterList? ')' block;

externFunctionDeclaration:
	Extern Pub? Func identifier typeParameters? '(' parameterList? ')' typeAnnotation? ReturnArrow?
		typeExpression? SemiColon;

// ====================== 语句 ======================
statement:
	variableStatement
	| returnStatement
	| breakStatement
	| continueStatement
	| throwStatement
	| yieldStatement
	| block
	| ifStatement
	| whileStatement
	| forStatement
	| tryStatement
	| withStatement
	| asyncStatement
	| streamStatement
	| expressionStatement;

block: '{' statement* '}';

variableStatement:
	Pub? (Const | Let) variableDeclarationList SemiColon;

variableDeclarationList:
	variableDeclaration (',' variableDeclaration)*;

variableDeclaration: (identifier | bindingPattern) typeAnnotation? (
		Assign expression
	)?;

bindingPattern: '(' identifier (',' identifier)* ')';

expressionStatement: expression SemiColon;

// ====================== 控制流语句 ======================
ifStatement:
	If '(' expression ')' statement (
		Elif '(' expression ')' statement
	)* (Else statement)?;

whileStatement: While '(' expression ')' statement;

forStatement:
	For '(' forInit? SemiColon expression? SemiColon expression? ')' statement	# ForLoop
	| For '(' identifier In expression ')' statement							# ForInStatement;

forInit: variableDeclarationList | expression;

// ====================== 错误处理与上下文 ======================
tryStatement:
	Try block (catchClause+ finallyClause? | finallyClause);

catchClause: Catch '(' identifier typeAnnotation? ')' block;

finallyClause: Finally block;

withStatement:
	With expression (
		As (identifier | bindingPattern) typeAnnotation?
	)? block;

// ====================== 跳转与异步语句 ======================
returnStatement: Return expression? SemiColon;

breakStatement: Break SemiColon;

continueStatement: Continue SemiColon;

throwStatement: Throw expression SemiColon;

yieldStatement: Yield expression? SemiColon;

asyncStatement:
	Async Func identifier '(' parameterList? ')' typeAnnotation? ReturnArrow? typeExpression block;

streamStatement:
	Async? Stream '<' typeExpression '>' identifier '(' parameterList? ')' block;

// ====================== 表达式 ======================
expression:
	primaryExpression										# PrimaryExpr
	| tupleExpression										# TupleExpr
	| matchExpression										# MatchExpr
	| expression '.' identifier								# MemberAccessExpr
	| expression '[' expression ']'							# IndexAccessExpr
	| expression typeGeneric? '(' argumentList? ')'			# CallExpr
	| expression '!' '(' argumentList? ')'					# MacroCallExpr
	| New expression '(' argumentList? ')'					# NewExpr
	| expression '++'										# PostfixExpr
	| expression '--'										# PostfixExpr
	| '+' expression										# PrefixExpr
	| '-' expression										# PrefixExpr
	| ('!' | KeyNot) expression								# PrefixExpr
	| '~' expression										# PrefixExpr
	| Await expression										# AwaitExpr
	| expression Is typeExpression							# IsExpr
	| expression Power expression							# PowerExpr
	| expression ('*' | '/' | '%') expression				# MultiplicativeExpr
	| expression ('+' | '-') expression						# AdditiveExpr
	| expression ('<<' | '>>') expression					# ShiftExpr
	| expression ('<' | '>' | '<=' | '>=') expression		# RelationalExpr
	| expression ('==' | '!=' | '===' | '!==') expression	# EqualityExpr
	| expression '&' expression								# BitAndExpr
	| expression '^' expression								# BitXorExpr
	| expression '|' expression								# BitOrExpr
	| expression ('&&' | KeyAnd) expression					# LogicalAndExpr
	| expression ('||' | KeyOr) expression					# LogicalOrExpr
	| expression '??' expression							# CoalesceExpr
	| expression '?' expression ':' expression				# TernaryExpr
	| lambdaExpression										# LambdaExpr
	| expression Assign expression							# AssignmentExpr
	| expression (
		'+='
		| '-='
		| '*='
		| '/='
		| '%='
		| '&='
		| '|='
		| '^='
		| '<<='
		| '>>='
		| '**='
	) expression # AssignmentExpr;

primaryExpression:
	literal					# LiteralExpr
	| identifier			# IdentifierExpr
	| '(' expression ')'	# ParenExpr
	| arrayLiteral			# ArrayLiteralExpr
	| objectLiteral			# ObjectLiteralExpr
	| This					# ThisExpr;

tupleExpression: '(' expression (',' expression)* ')';

argumentList: expression (',' expression)* ','?;

// ====================== 字面量和数据结构 ======================
literal:
	NullLiteral
	| BooleanLiteral
	| StringLiteral
	| FormatStringLiteral
	| RawStringLiteral
	| ByteStringLiteral
	| numericLiteral;

numericLiteral:
	DecimalLiteral
	| HexIntegerLiteral
	| OctalIntegerLiteral
	| BinaryIntegerLiteral;

arrayLiteral: '[' expression? (',' expression)* ','? ']';

objectLiteral:
	'{' (propertyAssignment (',' propertyAssignment)* ','?)? '}';

propertyAssignment: identifier ':' expression | identifier;

lambdaExpression:
	Async? lambdaSignature typeAnnotation? Arrow (
		block
		| expression
	);

lambdaSignature: identifier | '(' parameterList? ')';

// ====================== match 表达式 ======================
matchExpression:
	Match expression '{' matchCase+ matchDefault? '}';

matchCase:
	Case (expression | typeAnnotation) ('=>' expression | block);

matchDefault: Default ('=>' expression | block);

// ====================== 标识符 ======================
identifier:
	Identifier
	| From
	| Import
	| Pub
	| Pri
	| Const
	| Let
	| Func
	| Class
	| Trait
	| New
	| This
	| If
	| Else
	| Elif
	| While
	| For
	| Break
	| Continue
	| Return
	| Default
	| Try
	| Catch
	| Finally
	| Throw
	| With
	| As
	| Type
	| Is
	| Async
	| Await
	| Stream
	| Yield
	| Macro
	| Extern
	| Match
	| Case;