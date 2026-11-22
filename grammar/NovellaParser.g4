parser grammar NovellaParser;

// =============================================================================
// Novella 语法文法
//
// 对齐基准（唯一事实源）：
//   - crates/novella-ast/    AST 节点定义：本文件只描述 AST 能承载的结构；
//                            AST 中已不存在的结构，本文件不再保留
//   - crates/novella-parser/ 手写递归下降解析器：本文件按代码行为描述；
//                            上下文敏感之处（`from "m" import ...` 的 `import`、
//                            `with expr as binding` 的 `as`）由
//                            parser 侧的 token 预处理与上下文标记消歧，文法本身保持 LL(1) 外观
//   - 词法面见 NovellaLexer.g4（与 novella-core 的 `TokenKind` 逐条对齐）
//
// 与旧参考文的差异（本文件同步移除的结构）：
//   - `objectType` / `objectTypeMember`：`TypeExpr` 无对象类型变体；
//   - `asyncStatement`：`async func ...` 并入 `FunctionItem.is_async`，
//     异步 lambda 由 `Lambda.is_async` 承载；
//   - 字面量形态的 `predefinedType`（`BooleanLiteral | StringLiteral | NullLiteral`）：
//     `PredefinedType` 是一组类型名（int / float / u8 / ...），词法层并无这些保留字。
//
// 行注释在语法层透明：`LineComment` 是正式 token（供高亮等消费），由 parser
// preprocess 一处过滤，CLI / LSP 不另行过滤，故本文件不引用该 token。
// =============================================================================

options {
	tokenVocab = NovellaLexer;
}

// ====================== 程序结构 ======================
// 空文件合法（对应 `Parser::parse` 的 `while !self.at_eof()` 循环，逐条解析至流尾）
program: sourceElement* EOF;

sourceElement:
	moduleImport
	| typeAliasDeclaration
	| traitDeclaration
	| classDeclaration
	| structDeclaration
	| functionDeclaration
	| externFunctionDeclaration
	| directiveStatement
	| statement;

// ====================== 模块系统 ======================
// 对应 `ImportItem`：`ImportKind::Named`（from 形态）与 `ImportKind::Namespace`（import 形态）
moduleImport: fromImport | directImport;

// 具名导入：`from "mod" import a, b as c;`
fromImport: From StringLiteral Import importSpecifier (',' importSpecifier)* SemiColon;

// 对应 `ImportSpecifier`：`imported` 为符号名，`as` 后的名字写入 `local`（缺省表示同名）
importSpecifier: identifier (As identifier)?;

// 命名空间导入：`import "mod";`（整模块绑定，本地别名由语义分析按路径推导）
directImport: Import StringLiteral SemiColon;

// ====================== 编译器指令 ======================
// directiveStatement 两种形态，分别对应 `Item::Directive` 与 `Item::DirectiveBlock`：
//   - directive SemiColon —— 独立指令语句，如 `@no_std;`
//     `@no_std;` 的入口限定与禁用标准库语义由 sema 在模块加载阶段处理。
//   - directive block     —— 指令块，如 `@cfg(feature = "limine") { ... }`
//
// 文法只允许单条指令：代码显式报错「指令语句 / 指令块只允许一个编译器指令」
//（`Parser::parse_directive_statement`），故此处不是 `directive*`。
directiveStatement: directive (SemiColon | block);

// 编译器指令（对应 `CompilerDirective`）：`@name` 或 `@name(...)`。
//
// 指令在编译期解释，不是运行时机制。括号内为**自由 token 流**，原样收集进
// `CompilerDirective.tokens`（不含外层括号本身），结构化解释（如
// `@cfg(target_os = "linux")`）延后到语义分析阶段——符合「值语义延后」约定。
// 名字必须是标识符：代码中 `@` 后不是 `Identifier` 即报「`@` 后应有编译器指令名」。
directive: At identifier ('(' directiveArgument* ')')?;

// 括号内自由 token：允许嵌套括号（与代码的括号配对收集一致，嵌套括号平铺在 tokens 中）。
// 括号未闭合属致命错误，由解析阶段的诊断报告（代码：`编译器指令括号未闭合`）。
directiveArgument: '(' directiveArgument* ')' | ~(OpenParen | CloseParen);

// ====================== 类型系统 ======================
// 类型别名（对应 `TypeAliasItem`）
typeAliasDeclaration: Type identifier typeParameters? Assign typeExpression SemiColon;

// 泛型参数列表（对应 `TypeParameter`：`T` 或 `T: Bound`）
typeParameters: '<' typeParameterList '>';

typeParameterList: typeParameter (',' typeParameter)*;

typeParameter: identifier (Colon typeExpression)?;

// 类型表达式（对应 `TypeExpr`）。
// 联合类型解析时折叠为扁平列表（`TypeExpr::Union.types`），不保留二元嵌套结构。
typeExpression:
	arrowFunctionType
	| primaryType
	| functionType
	| typeExpression '|' typeExpression;

// 基础类型：只有 `null` 在词法层是独立 token（`Null`）。
// 其余类型名（boolean / string / int / float / void / u8..u64 / i8..i64 / f16..f64 / usize / isize）
// 在词法层都是普通 Identifier，故不在此写死集合——也不在 parser 归类：parser 一律产出
// `TypeExpr::Reference`，由语义分析按名字归入 `PredefinedType`，避免在词法层或语法层
// 各维护一份类型名表而与 `TokenKind` 这一事实源冲突。
predefinedType: Null;

// 主类型：元组 / 基础类型 / 类型引用 / 数组 / 指针（对应 `TypeExpr::Tuple` /
// `Predefined` / `Reference` / `Array` / `Pointer`）。
// 指针后缀 `'*'` 复用 `Multiply` token（类型位后缀用法，与表达式前缀解引用
// 区分）；`u8**` 词法即 `u8` + `*` + `*`，parser 直接循环套两层，无 `**` 特例。
primaryType:
	'(' typeList? ')'		# TupleTypeCase
	| predefinedType		# PredefinedTypeCase
	| typeReference			# ReferenceTypeCase
	| primaryType '[' ']'	# ArrayTypeCase
	| primaryType '*'		# PointerTypeCase;

typeList: typeExpression (',' typeExpression)*;

// 类型引用：`Foo` / `mod.Foo<T>`（对应 `TypeExpr::Reference{qualifier, name, generic_args}`；
// `identifier` 前段记入 qualifier，命名空间归属由语义分析按 `import "mod";` 绑定解析）
typeReference: identifier (Dot identifier)* typeGeneric?;

typeGeneric: '<' typeArgumentList '>';

typeArgumentList: typeExpression (',' typeExpression)*;

// 箭头函数类型：`<T>(int) -> string`（对应 `TypeExpr::Function`）
arrowFunctionType: typeParameters? '(' typeList? ')' ReturnArrow typeExpression;

// 具名函数类型：`func (a: int) -> int`（与 arrowFunctionType 合并进同一 `FunctionType` 节点）
functionType: Func typeParameters? '(' parameterList? ')' ReturnArrow typeExpression;

// 类型注解：`name: Type`（对应各处 `typeAnnotation?`）
typeAnnotation: Colon typeExpression;

// ====================== trait 与类声明 ======================
// trait 声明（对应 `TraitItem`：attributes / name / type_parameters / members / scope）
traitDeclaration:
	directive* (Pub | Pri | Pkg)? Trait identifier typeParameters? traitBody;

traitBody: '{' traitMember* '}';

// trait 成员与类成员同构（对应 `TraitItem.members: Vec<ClassMember>`）
traitMember: methodDeclaration | fieldDeclaration;

// 类声明（对应 `ClassItem`：attributes / name / type_parameters / members / parent / scope）
classDeclaration:
	directive* (Pub | Pri | Pkg)? Class identifier typeParameters? classHeritage? classBody;

// 继承子句 = 「父类 / trait」混合列表（以代码为准：parser 支持逗号分隔多项）。
// 各项语义由 sema 按符号表分类——class 项是父类（仅首项生效，多于一个报多继承），
// trait 项是实现的 trait（D04，见 docs/sema-coverage.md）。
classHeritage: '(' typeReference (',' typeReference)* ')';

classBody: '{' classMember* '}';

// 类成员（对应 `ClassMember`）
classMember: methodDeclaration | fieldDeclaration;

// ====================== struct 声明 ======================
// struct 声明（对应 `StructItem`：attributes / name / type_parameters / heritage /
// members / scope）。值语义：无构造器（字段初始化构造，见 structInit）；
// 继承子句按 classHeritage 语法解析后保留在 `StructItem.heritage`——struct 是否
// 允许继承由语义分析判定（parser 只保留信息）；成员文法复用 class 成员。
structDeclaration:
	directive* (Pub | Pri | Pkg)? Struct identifier typeParameters? classHeritage? structBody;

structBody: '{' classMember* '}';

// ====================== 成员声明 ======================
// 字段成员（对应 `FieldMember`：attributes / modifiers / name / ty / initializer）。
// `Weak?` 弱引用修饰（仅 class / struct 字段，对应 object-model 的 weak 语法）：
// 赋值不 retain、不参与引用计数，访问过期对象属未定义行为（无空安全保证）。
fieldDeclaration:
	directive* accessModifier? Static? Weak? identifier typeAnnotation? (
		Assign expression
	)? SemiColon;

// 方法成员（对应 `MethodMember`：attributes / modifiers / name / type_parameters /
// params / return_type / block）。
// 返回类型注解沿用 AST 文档所记的 `typeAnnotation? ReturnArrow? typeExpression?`
//（解析结果统一落在 `MethodMember.return_type` 一个字段上）；
// `(block | SemiColon)` 对应 `MethodMember.block` 的 `Some` / `None`。
methodDeclaration:
	directive* accessModifier? Static? Func identifier typeParameters? '(' parameterList? ')'
		typeAnnotation? ReturnArrow? typeExpression? (
		block
		| SemiColon
	);

// 成员可见性（对应 `MemberModifiers.scope: Option<Scope>`）
accessModifier: Pub | Pri | Pkg;

// ====================== 函数声明 ======================
// 顶层函数声明（对应 `FunctionItem`：attributes / name / r#type / block / scope /
// is_extern / is_async）。
// `Async?` 对应 `FunctionItem.is_async`（此前的 `asyncStatement` 已并入本节点）。
functionDeclaration:
	directive* (Pub | Pri | Pkg)? Async? Func identifier typeParameters? '(' parameterList? ')' typeAnnotation?
		ReturnArrow? typeExpression? block;

parameterList: parameter (',' parameter)*;

// 形参（对应 `Param`：name / ty / default）
parameter: identifier typeAnnotation? (Assign expression)?;

// extern 函数声明（对应 `FunctionItem`：is_extern = true、block = None，以 `;` 结束）。
// 可见性写在 `extern` 之前（`pub extern func`）：代码中 `extern pub func` 会报
//「`extern` 后不应有可见性修饰符」。编译器指令（如 `@link("c")`）保留在
// `FunctionItem.directives`，语义由语义分析解释。
externFunctionDeclaration:
	directive* (Pub | Pri | Pkg)? Extern Func identifier typeParameters? '(' parameterList? ')' typeAnnotation? ReturnArrow?
		typeExpression? SemiColon;

// ====================== 语句 ======================
statement:
	variableStatement
	| returnStatement
	| breakStatement
	| continueStatement
	| throwStatement
	| yieldStatement
	| emptyStatement
	| block
	| ifStatement
	| whileStatement
	| forStatement
	| tryStatement
	| withStatement
	| unsafeStatement
	| expressionStatement;

block: '{' statement* '}';

// 空语句 `;`：语句位产出 `Statement::Empty`（供 sema 区分空语句与语句缺失）；
// 顶层 item 位的孤分号由 `Parser::parse_item` 消费后跳过，不产出节点
emptyStatement: SemiColon;

// 变量声明语句（对应 `VariableStmt`：pattern / ty / value / scope / r#type）。
// 单绑定产出 `Statement::Variable`；多绑定产出 `Statement::VariableList`
//（同一作用域按序求值，不包 Block）
variableStatement: (Pub | Pri | Pkg)? (Const | Let) variableDeclarationList SemiColon;

variableDeclarationList: variableDeclaration (',' variableDeclaration)*;

// 绑定目标：标识符或元组解构（对应 `BindingPattern::Ident` / `BindingPattern::Tuple`）
variableDeclaration: (identifier | bindingPattern) typeAnnotation? (
		Assign expression
	)?;

bindingPattern: '(' identifier (',' identifier)* ')';

expressionStatement: expression SemiColon;

// ====================== 控制流语句 ======================
// if / elif / else 链（对应 `IfStmt.branches`：`IfBranch::ElseIf` 与 `IfBranch::Else`）。
// 每个分支体是 `statement`（不限于花括号块），与 `IfBranch.block: Box<Statement>` 一致。
ifStatement:
	If '(' expression ')' statement (
		Elif '(' expression ')' statement
	)* (Else statement)?;

whileStatement: While '(' expression ')' statement;

// for 语句（对应 `ForKind::ForLoop` 与 `ForKind::ForIn`）；
// 循环体是 `statement`（对应 `ForStmt.block: Box<Statement>`）。
forStatement:
	For '(' forInit? SemiColon expression? SemiColon expression? ')' statement	# ForLoop
	| For '(' (identifier | bindingPattern) In expression ')' statement			# ForInStatement;

// init 段：声明列表或表达式（对应 `ForInit::Declaration` / `ForInit::Expression`）
forInit: variableDeclarationList | expression;

// ====================== 错误处理与上下文 ======================
// try 语句（对应 `TryStmt`：block / catches / finally）；
// 无 catch 时必须存在 finally，与 `catches` 为空即需 `finally` 的约束一致。
tryStatement:
	Try block (catchClause+ finallyClause? | finallyClause);

// catch 子句（对应 `CatchClause`：name / ty / block）
catchClause: Catch '(' identifier typeAnnotation? ')' block;

finallyClause: Finally block;

// with 语句（对应 `WithStmt`：expr / binding / block）
withStatement:
	With expression (
		As (identifier | bindingPattern) typeAnnotation?
	)? block;

// unsafe 语句（对应 `Statement::Unsafe`：block / span）。
// 唯一「unsafe 上下文」入口；块内仍为普通语句，哪些操作要求 unsafe 由
// 语义分析按 design.md §6.4「一条线」规则判定，语法层不做白名单过滤。
unsafeStatement: Unsafe block;

// ====================== 跳转语句 ======================
// 跳转语句（对应 `Statement::Return` / `Break` / `Continue` / `Throw` / `Yield`）
returnStatement: Return expression? SemiColon;

breakStatement: Break SemiColon;

continueStatement: Continue SemiColon;

throwStatement: Throw expression? SemiColon;

yieldStatement: Yield expression? SemiColon;

// ====================== 表达式 ======================
// 备选顺序即优先级（ANTLR 左递归规则：靠前的备选优先级更高）
expression:
	primaryExpression										# PrimaryExpr
	| tupleExpression										# TupleExpr
	| matchExpression										# MatchExpr
	| expression '.' identifier								# MemberAccessExpr
	| expression '[' expression ']'							# IndexAccessExpr
	| expression typeGeneric? '(' argumentList? ')'			# CallExpr
	// 对象构造（对应 `Expression::New{target, type_args, args}`）：`new` 之后是整条
	// 后缀表达式；若其顶层是调用表达式，构建 AST 时把该 `Call` 的 `type_args` /
	// `args` 提到 `New` 上、`callee` 作为 `target`（`new Vec<int>(1)` 的泛型实参
	// 因此保留在 `New.type_args`）；否则整条表达式即 `target`、实参为空
	//（如 `new getFoo()()`：`target` 是 `getFoo()` 调用本身）。
	// 旧写法 `New expression '(' argumentList? ')'` 无法解析 `new Foo(1)`：内层
	// `expression` 会先按 `CallExpr` 吃掉 `(1)`，随后再要求一个 `(` 便解析失败。
	| New expression										# NewExpr
	| expression '++'										# PostfixExpr
	| expression '--'										# PostfixExpr
	| '+' expression										# PrefixExpr
	| '-' expression										# PrefixExpr
	| ('!' | KeyNot) expression								# PrefixExpr
	| '~' expression										# PrefixExpr
	// 取址 `&x`（复用 BitAnd 的表达式前缀位，安全操作）
	| '&' expression										# PrefixExpr
	// 解引用 `*p`（复用 Multiply 的表达式前缀位，unsafe 操作）
	| '*' expression										# PrefixExpr
	| Await expression										# AwaitExpr
	| expression Is typeExpression							# IsExpr
	| expression ('*' | '/' | '%') expression				# MultiplicativeExpr
	| expression ('+' | '-') expression						# AdditiveExpr
	// 类型转换 `expr as Type`（复用 As 关键字；低绑式，位于加减之后）
	| expression As typeExpression							# CastExpr
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
	) expression # AssignmentExpr;

// 原子表达式（对应 `Expression::Literal` / `Identifier` / `Paren` / `ArrayLiteral` /
// `ObjectLiteral` / `This` / `StructInit`）
// 首选 `identifier objectLiteral`（struct 字段初始化构造 `Point{ x: 1 }`，
// 置于 identifier 之前、靠更长备选分流；语义层负责名字→类型绑定）。
primaryExpression:
	literal					# LiteralExpr
	| identifier typeGeneric? objectLiteral	# StructInitExpr
	| identifier			# IdentifierExpr
	| '(' expression ')'	# ParenExpr
	| arrayLiteral			# ArrayLiteralExpr
	| objectLiteral			# ObjectLiteralExpr
	| This					# ThisExpr;

// 元组表达式：至少两个元素（对应 `Expression::Tuple`，单元素 `(a)` 是 Paren）
tupleExpression: '(' expression ',' expression (',' expression)* ','? ')';

// 实参列表（允许尾随逗号，对应 `Call.args` / `New.args`）
argumentList: expression (',' expression)* ','?;

// ====================== 字面量和数据结构 ======================
// 字面量（对应 `Literal`）：字符串的三种前缀形态与数字的四种进制统一落在
// `Literal::String` / `Literal::Number`，原文保留（值语义延后）
literal:
	Null
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

// 数组字面量（对应 `Expression::ArrayLiteral`）
arrayLiteral: '[' (expression (',' expression)* ','?)? ']';

// 对象字面量（对应 `Expression::ObjectLiteral`）
objectLiteral:
	'{' (propertyAssignment (',' propertyAssignment)* ','?)? '}';

// 属性赋值：`a: 1`，简写 `a`（对应 `PropertyAssignment.value` 为 `None`）
propertyAssignment: identifier ':' expression | identifier;

// lambda 表达式（对应 `Lambda`：is_async / params / return_type / body）。
// 单参数简写 `x: T => body` 中的 `: T` 是 **lambda 返回注解**（typeAnnotation?，
// 落 `Lambda.return_type`），不是参数类型注解；参数注解须用 `(x: T) => body` 形态
lambdaExpression:
	Async? lambdaSignature typeAnnotation? Arrow (
		block
		| expression
	);

// lambda 参数形态（对应 `LambdaParams::Single` / `LambdaParams::List`）
lambdaSignature: identifier | '(' parameterList? ')';

// ====================== match 表达式 ======================
// match 表达式（对应 `Match`：subject / cases / default）
matchExpression:
	Match expression '{' matchCase+ matchDefault? '}';

// 分支（对应 `MatchCase`，模式为 `MatchPattern::Expr` 或 `MatchPattern::Type`）
// 表达式歧义时须为分支保留 `=>` 或 `{`，例如 `case name => value` 与 `case name { ... }`。
matchCase:
	Case (expression | typeAnnotation) ('=>' expression | block);

// 兜底分支（对应 `Match.default: Option<MatchCaseBody>`）
matchDefault: Default ('=>' expression | block);

// ====================== 标识符 ======================
// 标识符：代码只接受 `TokenKind::Identifier` 这一种 token——
// `Parser::except_identifier` 及各调用点（`parse_directive` / `parse_variable_declaration` /
// `parse_function`）都要求当前 token 是 Identifier，词法层也没有上下文关键字机制。
// 因此这里不列出「可作标识符的关键字」集合：关键字当前是硬保留字。
// 若将来要支持软关键字（如 `obj.type`），需先在 parser 侧放宽这些校验，
// 再把本规则改成 `Identifier | <候选关键字…>`，并保持与词法层一致。
identifier: Identifier;
