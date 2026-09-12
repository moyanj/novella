# Novella 语言设计

> 版本：1.1 · 状态：与代码对齐的现行设计（含已确认的待落地变更） ·
> 基准 commit：69ad1a4（`feat(parser): 整理装饰器与可见性解析骨架`）
>
> 对齐方式：**代码是事实源**。本文描述语言「应当呈现的样子」，其中
> 「已实现」指 lexer/AST/parser 当前已落地的部分，「已确认」指设计已定稿、
> 待按 §10 落地清单实施的变更（内存操作与 struct）。所有改动须同步
> `grammar/*.g4` 与 LSP 高亮（见 §9）。

---

## 1. 语言概览

novella 是一门类 TS/Rust 混合风格、面向系统编程的语言：TS 的血统体现在
`let/const`、对象字面量、`match/case`、`as` 转换、lambda、`??` 空值合并等
现代表达语法上；Rust 的血统体现在 `u8..u64/i8..i64/usize/isize` 宽度类型、
`pub/pri`、`trait`、`unsafe`、`with` 资源管理等。编译产物面向汇编。

- 词法：logos 手写规则，`RawToken` → `TokenKind` 纯投影，105 个正式变体
  （+ `Eof` / `Error` 合成项）；
- 语法：手写递归下降解析器（骨架，逐条实现中），错误可恢复；
- AST：全节点携带 `Span`，`#[derive(Spanned)]` 生成 `span()`；全量 serde，
  用于 CLI 的 JSON 输出；
- 语义 / 代码生成：占位（`add()` 桩），设计规则见 §6 / §8。

## 2. 设计原则与约定

1. **代码是唯一事实源**：`TokenKind` ↔ 词法、AST ↔ 文法两侧对齐，文法跟随
   代码；改动代码后必须同步 `grammar/*.g4`（反之亦然，但以代码为准）。
2. **TokenKind ↔ RawToken 双枚举**：lexer 的 `RawToken` 是 logos 投影，靠
   `From` 的 match 穷尽性保证不漏转。
3. **AST 全量挂 span**：除纯值枚举（运算符、`Literal`、`PredefinedType`、
   `Scope` 等）外，每个节点携带 `span` 字段。
4. **值语义延后**：字面量保留源码原文（进制前缀、`f/r/b` 前缀），解析在
   语义分析阶段。
5. **注释语法层透明**：`LineComment` 是正式 token（供高亮），parser 前过滤。
6. **错误可恢复**：词法 / 语法错误只进各自的 errors / diagnostics 列表，
   不中断后续分析。
7. **安全分层（已确认）**：默认安全；需要直接碰内存时用裸指针，但
   「穿指针访问」一律要求 `unsafe { }` 上下文（§6）。
8. **学习曲线平缓（已确认）**：语言面最小、无特事特办；日常代码走
   安全路径（对象/集合/`with`），底层能力显式 opt-in。

## 3. 词法

### 3.1 注释

| 形态 | Token | 说明 |
|------|-------|------|
| `// ...`（到行尾） | `LineComment` | 正式 token；CLI 喂 parser 前过滤 |
| `/* ... */` | —— | **不支持**：`/*` 被切为 `Divide` + `Multiply` |

### 3.2 标识符

`[_\p{L}$][_\p{L}\p{Nd}$]*`（`TokenKind::Identifier`）。
标识符是**硬保留字环境**：关键字不可作标识符；`identifier` 文法规则只接受
`Identifier` token（无上下文关键字机制，属已知限制，放宽须先动 parser）。

### 3.3 字面量

| 种类 | 形态 | Token |
|------|------|-------|
| 空值 | `null` | `Null` |
| 布尔 | `true` / `false` | `BooleanLiteral` |
| 十六进制整数 | `0x1F` / `0X1f` | `HexIntegerLiteral` |
| 八进制整数 | `0o17` | `OctalIntegerLiteral` |
| 二进制整数 | `0b1010` | `BinaryIntegerLiteral` |
| 十进制数字 | `42` / `1.5` / `.5` / `1e10` / `1.5e-3` | `DecimalLiteral` |
| 普通字符串 | `"abc"` / `'abc'` | `StringLiteral` |
| 格式化字符串 | `f"..."` / `f'...'` | `FormatStringLiteral` |
| 原始字符串 | `r"..."` / `r'...'` | `RawStringLiteral` |
| 字节字符串 | `b"..."` / `b'...'` | `ByteStringLiteral` |

约定：`1.` 会被切为 `1` + `.`（小数点后必须跟数字）；字符串转义内容不校验，
非法转义留待语义阶段；下划线仅作数字分隔保留（值语义延后）。

### 3.4 运算符与分隔符

多字符（最长匹配优先，先于单字符）：

```
++  --  **  **=  <<  >>  <<=  >>=  ==  !=  ===  !==  <=  >=
&&  ||  ??  =>  ->  *=  /=  %=  +=  -=  &=  ^=  |=
```

单字符：`[ ] ( ) { } ; , = ? : . + - ~ ! * / % < > & ^ | @`

> **已确认变更**：删除 `**`（幂）与 `**=`。原因：`**p` 会被词法切为 `Power`，
> 需 parser 特判拆两层解引用，是「最费解的语法」；删除后 `**p` 天然是两个
> `*`，幂改为 `pow(...)` 内建（若回归）。同时移除 `BinaryOp::Power`、
> `AssignOp::PowerAssign` 与相关文法、高亮映射（上表删除后多字符运算符为
> 23 个）。

### 3.5 关键字（硬保留）

| 分组 | 关键字 |
|------|--------|
| 模块 | `from` `import` |
| 修饰与声明 | `pub` `pri` `const` `let` `static` `func` `class` `trait` `new` `this` |
| 控制流 | `if` `else` `elif` `while` `for` `in` `break` `continue` `return` `default` `match` `case` |
| 逻辑词 | `and` `or` `not` |
| 错误处理 | `try` `catch` `finally` `throw` |
| 上下文 | `with` `as` |
| 类型 | `type` `is` |
| 异步 | `async` `await` `stream` `yield` |
| 宏 | `macro` |
| 外部 | `extern` |
| **新增（已确认）** | `unsafe` `struct` |

> 已知残留：`stream` / `macro` 词法层仍是关键字，但 AST 无对应结构（`Macro`
> 只有调用侧 `Expression::MacroCall`）；确认不再支持时应连同高亮一并移除。

#### 内置符号（已确认）

语言内置三组符号（非关键字 token，词法层不新增保留字）：

| 内置 | 定位 | 出处 |
|------|------|------|
| `@` | 双身份：语句位是编译器指令前缀（`@cfg(...)` 等）；路径位是路径别名「当前 crate 根」（等价 Rust `crate`） | §4.5 / §4.3.1 |
| `std` | 标准库路径别名（`from "std:xxx" import ...`） | §4.3.1 |
| `super` | 父级路径别名（`from "super:xxx" import ...`） | §4.3.1 |

### 3.6 词法错误

| 错误 | 触发 | 说明 |
|------|------|------|
| `InvalidCharacter { ch }` | 单字符无法匹配任何规则 | 如 `#` |
| `UnterminatedString { quote }` | 引号开头、缺少闭合引号 | 起始引号 + 最长可消费前缀作一个错误 token |

错误 token 只进 `errors` 列表、不进 `tokens`；EOF token 的 span 指向输入末尾，
行列按末尾处计算（行号 1-based、列号 0-based 字节偏移）。

## 4. 语法结构与 AST

### 4.1 程序结构

```
program: sourceElement* EOF;        // 空文件合法
sourceElement: moduleImport | typeAliasDeclaration | traitDeclaration
             | classDeclaration | functionDeclaration
             | externFunctionDeclaration | decoratorStatement | statement;
```

AST：`Program { items: Vec<Item>, span }`。顶层解析逐条 `parse_item`，
语法错误记录后继续。

### 4.2 顶层 item（`Item`）

| Item 变体 | 对应文法 | 说明 |
|-----------|----------|------|
| `Import(ImportItem)` | `moduleImport` | 具名 / 命名空间导入 |
| `TypeAlias(TypeAliasItem)` | `typeAliasDeclaration` | `type A<T> = ...;` |
| `Trait(TraitItem)` | `traitDeclaration` | 无 `pub`；成员同 class |
| `Class(ClassItem)` | `classDeclaration` | 支持继承、`pub`、装饰器、泛型 |
| `Function(FunctionItem)` | `functionDeclaration` / `externFunctionDeclaration` | 顶层函数 / extern |
| `Decorator(Decorator)` | `decoratorStatement`（`@foo;`） | 独立装饰器语句 |
| `DecoratorBlock { decorator, block }` | `decoratorStatement`（`@foo { }`） | 装饰块 |
| `Statement(Box<Statement>)` | `statement` | 顶层语句（含 `pub let`） |

顶层可见性：`pub` 只允许修饰 `class` / `func` / `let` / `const` / `extern`
（`pub extern func`）；`extern` 之后不能再写可见性。装饰器只允许修饰
`@foo;` / `@foo { }` / `func` / `class` / `trait`。

### 4.3 导入

```
fromImport: From StringLiteral Import importSpecifier (',' importSpecifier)* SemiColon;
directImport: Import StringLiteral SemiColon;
importSpecifier: identifier (As identifier)?;      // `as` 写 local 名
```

AST：`ImportItem { raw_path, prefix, path, kind: ImportKind }`；
`ImportKind::Named { specifiers }` 与 `ImportKind::Namespace { local_alias }`。

#### 4.3.1 路径别名（`xxx:` 前缀，已确认）

导入路径字面量支持 **`别名:路径`** 形式，`:` 前为路径别名（也即现有
`ImportItem.prefix` 字段，`path` 为去除别名的逻辑路径）：

```
from "std:exception" import ValueError, ZeroDivisionError;
```

- 内置路径别名（语言自带，语义分析阶段解析 `prefix` 时识别）：
  - **`@`** —— 当前 crate 根（等价 Rust 的 `crate`），
    `from "@:sub:mod" import ...` 从本 crate 根出发的绝对路径；
  - **`std`** —— 标准库（`"std:exception"` → `std` 库的 `exception` 模块；
    也可写作 `"std"` 直引）；
  - **`super`** —— 当前作用域的父级（类似相对 import 的 `..` 语义，
    常用于模块内的父命名空间引用；`from "super:xxx" import ...`）。
- 未知别名：语义分析阶段报「未知路径别名」；不在词法层引入新 token
  （`@` / `std` / `super` 在 `"..."` 内只是字符串内容，与语句位的 `@` 指令
  前缀互不影响）；
- 别名与 `importSpecifier` 的 `as`（本地重命名）互不冲突：`xxx:` 作用于
  **路径**，`as` 作用于**符号**。

### 4.4 类型别名

```
typeAliasDeclaration: Type identifier typeParameters? Assign typeExpression SemiColon;
typeParameters: '<' typeParameterList '>';
typeParameter: identifier (Colon typeExpression)?;
```

### 4.5 装饰器（编译器指令）

```
decorator: At identifier ('(' decoratorArgument* ')')?;
decoratorArgument: '(' decoratorArgument* ')' | ~(OpenParen | CloseParen);
```

**语义：编译器指令，等价于 Rust 的 attribute `#[...]`，而不是 Python 式
运行时装饰器**——`@xxx(...)` 由编译器在**编译期**按 `xxx` 解释并作用于其修饰
的声明/块（可影响布局、代码生成、检查等），不构造运行时包装。

- 括号内为**自由 token 流**（含嵌套括号，平铺保留），结构化解释延后到语义
  阶段（如 `@cfg(target_os = "linux")`、`@packed`、`@align(8)`、`@extern("C")`
  、`@deprecated`），届时按 `name` 查对应指令的处理规则；
- `@name` 无括号简写等价于空 token 流；
- 可修饰对象：独立语句 `@foo;`、装饰块 `@foo { }`、`func` / `class` / `trait`
  （文法仅这三类声明支持 `decorator*`）；
- 装饰器语句/块只允许单个装饰器；多个装饰器报错。

### 4.6 class / trait

```
classDeclaration: decorator* Pub? Class identifier typeParameters? classHeritage? classBody;
classHeritage: '(' typeReference ')';              // 文法仅单父类，AST 按多继承预留
classBody: '{' classMember* '}';
traitDeclaration: decorator* Trait identifier typeParameters? traitBody;
classMember: methodDeclaration | fieldDeclaration;
fieldDeclaration: decorator* accessModifier? Static? identifier typeAnnotation? (Assign expression)? SemiColon;
methodDeclaration: decorator* accessModifier? Static? Func identifier typeParameters?
    '(' parameterList? ')' typeAnnotation? ReturnArrow? typeExpression? (block | SemiColon);
accessModifier: Pub | Pri;
```

AST：`ClassItem { attributes, name, type_parameters, members, parent, scope }`；
`TraitItem { attributes, name, type_parameters, members }`；
`ClassMember::Field(FieldMember)` / `ClassMember::Method(MethodMember)`；
`MemberModifiers { scope: Option<Scope>, is_static }`。

#### 4.6.1 类的特殊方法（已确认）

**命名以双下划线 `__` 开头即可（结尾 `__` 不强制）**，如 `__init`、
`__destroy`、`__str` 等（写 `__init__` 风格亦可，语义一致）；由编译器在
特定时机自动调用，用户不直接调用。

- 词法无需改动：`__xxx` 本就匹配 `Identifier` 规则（`_` 可为首字符）；
  `__` 前缀的识别在语义阶段按名字判定；
- 作用范围：**仅 class**（以及 trait 声明的方法签名）；**struct 不支持特殊
  方法**——struct 方法名不得带 `__` 前缀（见 §4.7）；
- 特殊方法的可用清单（`__init` / `__destroy` / `__str` …）与调用时机
  （构造、析构、字符串化、`with` 协议等）由语义阶段按需定义并登记入
  design.md，此处只固定命名约定；
- 双下划线同时是**保留命名空间**：普通方法不得以 `__` 开头，防止与未来
  特殊方法撞名。

### 4.7 struct（值类型，已确认新增）

```
structDeclaration: decorator* Pub? Struct identifier typeParameters? structBody;
structBody: '{' structMember* '}';
structMember: methodDeclaration | fieldDeclaration;   // 复用 class 成员文法
```

AST：新增 `Item::Struct(StructItem)`，
`StructItem { attributes, name, type_parameters, members: Vec<ClassMember>, span }`。

语义定位（与 class 的分工见 §8.2）：

- **值语义**：赋值 / 传参 / 返回均为逐位拷贝（含 class 引用字段的「指针拷贝」，
  见 §8.3）；
- **无继承**：struct 不可有 `parent`，不可被继承；
- **无构造器**：用字段初始化构造 `TypeName{ x: 1, y: 2 }`（见 §5.4）；
- **方法可选**：允许方法，方法内 `this` 为**编译器注入的可信指针**，
  字段访问无需 unsafe（见 §8.4）；
- **无特殊方法**：struct 不识别 `__` 前缀方法（保留命名空间仅限 class，
  见 §4.6.1）——struct 方法名不得以 `__` 开头；
- **`==` / `!=` 不适用于 struct 值**（见 §8.5），指针比较不受影响；
- **布局**：按字段声明顺序 + 自然对齐（C/FFI 兼容），无虚表、无隐藏字段。

### 4.8 函数

```
functionDeclaration: decorator* Pub? Async? Func identifier typeParameters?
    '(' parameterList? ')' typeAnnotation? ReturnArrow? typeExpression? block;
externFunctionDeclaration: Pub? Extern Func identifier typeParameters?
    '(' parameterList? ')' typeAnnotation? ReturnArrow? typeExpression? SemiColon;
parameterList: parameter (',' parameter)*;
parameter: identifier typeAnnotation? (Assign expression)?;    // 形参默认值
```

AST：`FunctionItem { attributes, name, r#type, block, scope, is_extern, is_async }`；
`FunctionType { type_parameters, params, return_type }`；
`Param { name, ty, default }`。返回类型注解 `: T` 与 `-> T` 两种写法等价。

> 已确认：`FunctionItem.is_unsafe` 字段预留（`unsafe fn` 本期不做）。

#### 4.8.1 程序入口 `main`（已确认）

- 语言要求**必须有顶层 `main` 函数**作为程序入口：`func main() { ... }`；
  （理论形态上入口可缺省——如仅作为库被其他程序导入时，语义分析阶段按
  「是否产出可执行目标」判定；默认编译为可执行文件时报「缺少 main」）。
- `main` 是**硬性入口约定**，非保留字：`main` 仍是普通 `Identifier`，靠名字
  与「顶层 + 无参 + 无泛型」形态识别，不必新增关键字 token；
- 入口判定：顶层函数，函数名 `main`，无参数；返回类型可为 `void` / `int`
  （退出码，语义阶段定），其余形态报错。

### 4.9 语句

```
statement: variableStatement | returnStatement | breakStatement | continueStatement
         | throwStatement | yieldStatement | emptyStatement | block | ifStatement
         | whileStatement | forStatement | tryStatement | withStatement
         | unsafeStatement | expressionStatement;
```

| Statement 变体 | 文法 | 说明 |
|----------------|------|------|
| `Variable(VariableStmt)` | `Pub? (Const|Let) variableDeclarationList SemiColon` | 支持元组解构 `let (a, b)` |
| `Return` / `Break` / `Continue` / `Throw` / `Yield` | 对应跳转语句 | `yield` 值可缺省 |
| `Block(Block)` | `block: '{' statement* '}'` | 花括号块 |
| `If(IfStmt)` | `if/elif/else` 链 | 分支体为任意 `statement` |
| `While` | `while (expr) stmt` | |
| `For(ForStmt)` | `for (init?; cond?; upd?)` / `for (x in expr)` | init 段声明不带 `const/let`，一律按 `Let` 语义 |
| `Try(TryStmt)` | `try { } catch (e: T) { } finally { }` | 无 catch 必须有 finally |
| `With(WithStmt)` | `with expr (as name: T)? { }` | 资源管理 |
| **`Unsafe(UnsafeStmt)`（新增）** | `unsafe { }` | 唯一 unsafe 上下文入口 |
| `Expression(Expression)` | `expressionStatement` | 表达式语句 |

`UnsafeStmt { block: Box<Block>, span }`：块内仍是普通 `Statement`，语法层不
白名单过滤；「哪些操作要求 unsafe」由语义层按 §6.4 判定。

### 4.10 表达式与优先级

（ANTLR 左递归：**靠前备选 = 优先级更高**。下表自上而下优先级递减。
已确认删除 `Power` 幂，新增 `&`/`*` 前缀与 `as` 转换。）

| 优先级 | 文法备选 | AST |
|--------|----------|-----|
| 原子 | `literal` / `identifier` / `(expr)` / 数组 / 对象 / `this` | `Literal` / `Identifier` / `Paren` / `ArrayLiteral` / `ObjectLiteral` / `This` |
| 元组 | `(a, b, ...)`（≥2 元素） | `Tuple` |
| match | `match x { case ... }` | `Match` |
| 后缀 | `.` 成员 / `[ ]` 索引 / 调用 / `!()` 宏调用 / `new` 构造 / `++` `--` | `MemberAccess` / `IndexAccess` / `Call` / `MacroCall` / `New` / `Unary(PostInc/Dec)` |
| 前缀 | `+ - ~ ! not` **`& *`（新增）** / `await` | `Unary` / `Await` |
| 类型判断 | `expr is Type` | `Is` |
| 乘除 | `* / %` | `Binary` |
| 加减 | `+ -` | `Binary` |
| **转换（新增）** | `expr as Type` | `Cast` |
| 移位 | `<< >>` | `Binary` |
| 关系 | `< > <= >=` | `Binary` |
| 相等 | `== != === !==` | `Binary` |
| 位与 | `&` | `Binary` |
| 位异或 | `^` | `Binary` |
| 位或 | `|` | `Binary` |
| 逻辑与 | `&&` / `and` | `Binary` |
| 逻辑或 | `\|\|` / `or` | `Binary` |
| 空值合并 | `??` | `Binary` |
| 三元 | `? :` | `Ternary` |
| lambda | `x => ...` / `(a, b) => ...` | `Lambda` |
| 赋值 | `=` 与全部复合赋值 | `Assignment` |

> **`as` 的优先级**（已确认：低绑式）：位于「加减」之后、「移位」之前——
> 低于算术、高于关系比较，与 TS 观感一致：
> `p + 1 as u32` ⇒ `(p + 1) as u32`；`p as usize + 1` ⇒ `(p as usize) + 1`；
> `*p as u8` ⇒ `(*p) as u8`（一元高于 `as`）。
> 需要整体转换时通常不必加括号；`(a + b) as u8` 也可显式写。

### 4.11 表达式节点

`Expression` 变体：`Literal` `Identifier` `Paren` `Tuple` `ArrayLiteral`
`ObjectLiteral` `This` `Unary` `Await` `Binary` `Is` `Ternary` `Assignment`
`MemberAccess` `IndexAccess` `Call` `New` `MacroCall` `Match(Match)`
`Lambda(Lambda)` **`Cast`（新增）**。

运算符枚举：
- `UnaryOp`：`Plus` `Minus` `Not` `BitNot` `PostIncrement` `PostDecrement`
  **`AddressOf` `DerefPointer`（新增）**；
- `BinaryOp`：`Power`（**删除**）`Multiply` `Divide` `Modulus` `Plus` `Minus`
  `LeftShift` `RightShift` `LessThan` `GreaterThan` `LessThanEquals`
  `GreaterThanEquals` `Equals` `NotEquals` `IdentityEquals` `IdentityNotEquals`
  `BitAnd` `BitXor` `BitOr` `LogicalAnd` `LogicalOr` `NullCoalesce`；
- `AssignOp`：`Assign` `PlusAssign` `MinusAssign` `MultiplyAssign` `DivideAssign`
  `ModulusAssign` `BitAndAssign` `BitOrAssign` `BitXorAssign` `LeftShiftAssign`
  `RightShiftAssign` `PowerAssign`（**删除**）。

### 4.12 字面量 AST 与 lambda / match 结构

- `Literal`：`Null` / `Boolean(bool)` / `String(String)`（保留原文） /
  `Number(String)`（保留原文与进制前缀）；
- `Lambda { is_async, params: LambdaParams, return_type, body: LambdaBody }`；
  `LambdaParams::Single` / `LambdaParams::List`；`LambdaBody::Expr` / `Block`；
- `Match { subject, cases, default }`；`MatchPattern::Expr` / `Type`（`case : T`）；
  `MatchCaseBody::Expr` / `Block`；
- `PropertyAssignment { name, value: Option<Expression> }`：`{ a: 1, b }` 简写。

## 5. 类型系统

### 5.1 类型表达式（`TypeExpr`）

| 变体 | 形态 | 说明 |
|------|------|------|
| `Predefined { ty }` | `int` / `float` / `boolean` / `string` / `void` / `u8..u64` / `i8..i64` / `f8..f64` / `usize` / `isize` / `null` | 类型名在词法层是普通 `Identifier`，由 parser/语义按名字归类 |
| `Reference { name, generic_args }` | `Foo` / `Foo<T>` | 类型引用 |
| `Array { element }` | `T[]` | 数组类型 |
| `Tuple { elements }` | `(T1, T2)` | 元组类型 |
| `Function(FunctionType)` | `<T>(a: int) -> int` / `func (a) -> T` | 两种写法合并 |
| `Union { types }` | `A | B | C` | 扁平存储 |
| **`Pointer { element }`（新增）** | `T*`（可嵌套 `T**`，允许 `void*`） | 指针类型 |

`TypeParameter { name, bound }`；`typeAnnotation: Colon typeExpression`。

### 5.2 预定义类型（`PredefinedType`）

`Null` `Boolean` `String` `Int` `Float` `Void` `U8` `U16` `U32` `U64` `I8`
`I16` `I32` `I64` `F8` `F16` `F32` `F64` `Usize` `Isize`。

### 5.3 指针类型（已确认新增）

- 后缀形态 `T*`，可连写 `u8**` ≡ `Pointer(Pointer(u8))`（右结合）；
- 可在 `primaryType` 级别组合：`(A | B)*`、`(i32, i32)*`；
- `void*` 合法：可隐式赋给任意 `T*`，对 `void*` 解引用报错、参与算术报错；
- 指针宽度 = `usize`（平台决定，语义阶段定型）；
- 文法：`primaryType: ... | primaryType '*';`（`'*'` 即 `Multiply` token）。

### 5.4 struct 构造表达式（已确认新增）

```
structInit: identifier objectLiteral;   // 如 Point{ x: 1, y: 2 }
```

- 复用 `objectLiteral` 的字段赋值形态（`PropertyAssignment`）；
- 语义：每个字段必须可初始化——显式给出，或字段声明带 `initializer` 时取默认值；
- 无位置参数构造（`Point(1, 2)` 不支持，避免与函数调用混淆）；
- AST：`Expression::StructInit { name, properties, span }`（或并入现有
  表达式家族，落地时定具体变体名）。

## 6. 内存操作与 unsafe（已确认新增）

### 6.1 概览

```
let p: u64* = &v;              // 取址（安全）
unsafe {
    *p = 9;                    // 解引用写（unsafe）
    let d: u64 = *p;           // 解引用读（unsafe）
    **pp = 10;                 // 嵌套解引用（`**` 就是两个 `*`，无特例）
    let a: usize = p as usize; // 指针 → 整数（unsafe）
    p[2] = 3;                  // 下标 ≡ *(p+2)（unsafe）
    p.m = 5;                   // 穿指针成员访问（unsafe）
}
```

### 6.2 操作符

| 操作 | 语法 | 复用 token | unsafe? |
|------|------|-----------|---------|
| 取址 | `&expr` | `BitAnd` 前缀位 | 否（安全） |
| 解引用 | `*expr` | `Multiply` 前缀位 | 是 |
| 下标 | `p[n]` | 既有 `IndexAccess` | 是 |
| 成员 | `p.m` | 既有 `MemberAccess`（自动解引用，等价 `(*p).m`） | 是 |
| 指针算术 | `p±n` `p1-p2` `++p` `p+=n` | 既有二元/后缀/赋值变体 | 是 |
| 判空 / 比较 | `p==null` `p==q` | 既有 `Equals`/`NotEquals` | 否（安全） |
| 转换 | `expr as Type` | `As` token | 指针相关是，纯数值否 |

### 6.3 `as` 转换矩阵

| 源 \ 目标 | 整数 | `T*` | `void*` | 其他 |
|-----------|------|------|---------|------|
| 整数 | 允许 | unsafe | unsafe | 拒绝 |
| `T*` | unsafe | unsafe | unsafe | 拒绝 |
| `void*` | unsafe | unsafe | 允许 | 拒绝 |
| 其他 | 整数形态允许 | 拒绝 | 拒绝 | 按类型系统 |

### 6.4 unsafe 上下文（一条线规则）

「**穿过指针访问内存**」即需 unsafe：

- 解引用 `*p`、下标 `p[i]`、成员访问 `p.m`（读与写统一，无「读安全写不安全」
  的中间态）、指针算术（`±`/`++`/`--`/`+=`/`-=`）、指针互转与指针↔整数转换；
- 取址 `&x`（左值即可）、判空 `p==null`、指针比较 `p==q`、纯数值 `as`
  均为安全操作，unsafe 外可用；
- `unsafe { }` 可嵌套；块内允许普通语句；本期不提供 `unsafe fn` 与
  `unsafe` 表达式形态；
- 悬垂指针 / 越界 / NULL 解引用：unsafe 语义下不做静态证明（未定义行为），
  生命周期检查归属后续引用系统。

### 6.5 语义规则摘要（供 `novella-sema` 实现）

| # | 规则 |
|---|------|
| S1 | `&x`：x 为左值 ⇒ `T*`。安全。 |
| S2 | `*p`：p 为 `T*` ⇒ `T`。unsafe。 |
| S3 | `p[i]`：p 为 `T*` ⇒ `T`。unsafe。 |
| S4 | 指针算术：unsafe；`p1-p2` ⇒ `isize`；非法组合（指针+指针等）报错。 |
| S5 | 指针相关 `as`：unsafe；纯数值转换不受限。 |
| S6 | `p.m`：p 为 `T*` 自动解引用；读、写均 unsafe（struct 方法内 `this` 除外，§8.4）。 |
| S7 | `p==q` / `p==null`：安全；异种指针比较报错。 |
| S8 | `void*`：隐式赋任意 `T*`；解引用 / 算术报错。 |
| S9 | `null`：类型 `void*`，隐式转任意 `T*`；未初始化指针解引用报错。 |
| S10 | 指针不隐式转整数或其他指针（必须 `as`）。 |

## 7. 错误处理与诊断

- 词法：`LexError::InvalidCharacter` / `UnterminatedString`，经
  `TokenError::to_diagnostic` 转通用诊断，只进 `errors`；
- 语法：parser 的 `error_here` / `error_at` 收集进 `diagnostics`，不中断；
- 通用结构：`Diagnostic { severity, message, location, note }`（serde），
  CLI `render_diagnostic` 按 rustc 风格渲染（`error: ...` + `--> 文件:行:列`
  + 源码行 + caret，列号 1-based 展示、caret 按字符不错位）；
- 阶段衔接：lexer → parser →（sema/codegen）→ CLI 输出 JSON
  （tokens + program + diagnostics）。

## 8. struct 与 class 的分工

### 8.1 对比

| | `class` | `struct` |
|---|---------|----------|
| 语义 | 引用语义（引用计数，后续设计） | 值语义（拷贝即复制） |
| 分配 | 堆 / 计数管理 | 栈 / 手动（配合指针） |
| 构造 | `new Foo(...)` | `Foo{ x: 1 }` |
| 继承 | 有（单父类，按多继承预留） | 无 |
| 内存角色 | 日常对象 | 底层数据载体、FFI、寄存器/驱动 |

### 8.2 值语义

`let b = a;`（a、b 均为 struct）即逐字段/逐位拷贝；函数参数、返回值同样按值。
struct 数组 `T[]` 为连续内存。

### 8.3 class 引用字段 = 指针拷贝

struct 内允许 class 引用字段，拷贝时**按位复制句柄**（不深拷贝、不自动增减
引用计数）；引用计数由使用方负责——与裸指针的手动管理哲学一致。

### 8.4 struct 方法与可信 `this`

- struct 允许方法（复用 `methodDeclaration` 文法与 `MethodMember` AST）；
- 方法内 `this` 是**编译器注入的可信指针**（指向调用时实际对象），
  因此方法体内 `this.x` 访问**无需 unsafe**——这是对 §6.4 统一规则的
  唯一例外，其正当性在于 this 由编译器保证有效性，非用户拼装；
- 方法调用形态：值上调用（`a.norm()`）自动取址；指针上调用（`p.norm()`）
  沿用穿指针规则（调用点本身在 unsafe 内）。

### 8.5 struct 的 `==`

`a == b`（struct 值）**非法**（编译错误，提示用逐字段比较或后续 `eq`
内置）；指针比较 `pa == pb`、`pa == null` 不受影响（§6.2）。

## 9. 词法/文法/高亮同步约定

- `TokenKind` ↔ `RawToken` ↔ `NovellaLexer.g4` 三处同步（新增/删除变体）；
- AST ↔ `NovellaParser.g4`：只描述 AST 承载的结构，文法不保留 AST 没有的
  结构；两侧不一致以代码为准；
- LSP：`token_type_index` 是穷尽 match，关键字/运算符新增须同步
  `semantic.rs`（`TOKEN_TYPES` legend 顺序即下标）；新关键字 → `KEYWORD`；
- 删除 token 属**破坏性 JSON 变更**：旧 token JSON 含 `"Power"` 无法反序列化，
  当前产物为开发期快照，无长期格式承诺，直接替换即可。

## 10. 落地清单（已确认变更，按序实施）

| 步 | 文件 | 改动 |
|----|------|------|
| 1 | `crates/novella-core/src/token.rs` | 删 `Power`/`PowerAssign`；增 `Unsafe`、`Struct` |
| 2 | `crates/novella-lexer/src/lib.rs` | 删 2 规则+映射；增 `unsafe`、`struct` 规则+映射 |
| 3 | `crates/novella-ast/src/types.rs` | 增 `TypeExpr::Pointer` |
| 4 | `crates/novella-ast/src/expr.rs` | 增 `UnaryOp::AddressOf`/`DerefPointer`、`Expression::Cast`（及 struct 构造节点）；删 `BinaryOp::Power`、`AssignOp::PowerAssign` |
| 5 | `crates/novella-ast/src/stmt.rs` | 增 `Statement::Unsafe` |
| 6 | `crates/novella-ast/src/program.rs` | 增 `Item::Struct`、`StructItem` |
| 7 | `grammar/NovellaLexer.g4` | 删 `Power`/`PowerAssign`；增 `Unsafe`、`Struct` |
| 8 | `grammar/NovellaParser.g4` | 删幂行；增指针后缀、`as`、`unsafeStatement`、`structDeclaration`、struct 构造 |
| 9 | `crates/novella-parser/src/lib.rs` | 类型后缀、前缀取址/解引用、`as` 分支、unsafe 语句、struct 声明与构造 |
| 10 | `crates/novella-lsp/src/semantic.rs` | 删 2 映射；增 `Unsafe`、`Struct` 映射 |
| 11 | `novella-sema`（下一阶段） | §6.5 / §8 规则实现 |
| 12 | `grammar/README.md` 等 | 同步更新（含 `Power` 移除说明） |

依赖：1→2→（lexer 依赖 core）；3/4/5/6 相互独立；7 依赖 1；8 依赖 3/4/5/6；
9 依赖 2/3/4/5/6；10 依赖 1；11 依赖 9。

## 11. 已知限制与待定项

- 标识符为硬保留字环境（无软关键字），`obj.type` 这类写法当前不可解析；
- for-init 段声明不带 `const/let`，统一按 `Let` 语义；
- `stream` / `macro` 关键字无对应结构（`Macro` 仅有调用侧）；
- `Scope` 枚举拼写为 `Pubilc`（历史笔误，改名需同步全部引用与 JSON 输出）；
- 标识符为硬保留字环境（无软关键字），`obj.type` 这类写法当前不可解析；
- for-init 段声明不带 `const/let`，统一按 `Let` 语义；
- `stream` / `macro` 关键字无对应结构（`Macro` 仅有调用侧）；
- `Scope` 枚举拼写为 `Pubilc`（历史笔误，改名需同步全部引用与 JSON 输出）；
- `FunctionItem.block` 为 `Option<Box<Statement>>`（尺寸优化）；
- 特殊方法**清单待定**：`__init` / `__destroy` / `__str` / `__enter` /
  `__exit`（with 协议）等由语义阶段登记（命名约定已定：`__` 开头即可，见 §4.6.1）；
- 路径别名解析（`std` / `super`）与 `main` 入口判定在语义阶段落地；
- `==` 对 struct 的逐字段比较、`unsafe fn`、`unsafe` 表达式、`volatile`、
  `sizeof/offsetof`、`alloc/free`、slice 视图、函数指针类型、引用/借用与
  生命周期均为后续候选。

## 12. 快速上手（完整示例）

```novella
// 路径导入（std 内置路径别名）
from "std:exception" import ValueError, ZeroDivisionError;

// 编译器指令（attribute 语义）
@deprecated
class Vec2 { x: f32; y: f32 }

// class：特殊方法用双下划线开头（仅 class，struct 无；结尾不强制）
class Account {
    balance: i64;
    func __init(initial: i64) {
        this.balance = initial;
    }
    func __str() -> string {
        return f"balance={this.balance}";
    }
}

// 底层路径：struct + 指针 + unsafe（手动管理内存）
pub struct Point {
    pub x: i32;
    pub y: i32;
}

func demo() {
    let a = Point{ x: 1, y: 2 };   // struct 字段初始化构造
    let b: Point = a;               // 值拷贝
    let p: Point* = &a;             // 取址（安全）
    unsafe {
        p.x = 5;                    // 穿指针写（unsafe）
        let c: Point = *p;          // 解引用拷贝
    }
}

// 程序入口：必须有 main
func main() {
    demo();
}
```
