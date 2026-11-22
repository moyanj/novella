# Novella 语言设计

> 版本：2.0 · 状态：与代码对齐的现行设计（含已确认的待落地变更） ·
> 基准 commit：067edaa（`feat(grammar): 以代码为基准对齐文法并补充文档`）
>
> 本文从**语言视角**描述 novella：语法、语义、类型系统、对象模型、执行模型与
> 内置设施——即「语言长什么样、程序怎么写、语义怎么解释」；对象模型（class
> 引用计数、weak、with 协议）见 §7，异常与执行模型见 §9。编译器如何实现这些规则
> （词法器/解析器/语义检查/代码生成的工程结构）见
> **[`compiler-design.md`](compiler-design.md)**；两侧以**代码为事实源**对齐。

---

## 1. 语言概览

novella 是一门类 TS/Rust 混合风格、面向系统编程的语言：TS 的血统体现在
`let/const`、对象字面量、`match/case`、`as` 转换、lambda、`??` 空值合并等
现代表达语法上；Rust 的血统体现在 `u8..u64/i8..i64/usize/isize` 宽度类型、
`pub/pri`、`trait`、`unsafe`、`with` 资源管理等。编译产物面向汇编。

**内存分层**（核心定位）：

- **安全路径（默认）**：对象 / 集合 / `with` / 资源管理，内存由语言层机制负责
  （class 引用计数，后续设计），日常代码看不到指针；
- **底层路径（显式 opt-in）**：`struct`（值类型，手动管理）+ 裸指针 + `unsafe`
  块，面向内核 / 驱动 / FFI / 寄存器访问。

## 2. 设计原则

1. **值语义延后**：字面量保留源码原文（进制前缀、`f/r/b` 前缀），具体值解析
   在语义分析阶段。
2. **注释语法层透明**：`LineComment` 是正式 token（供高亮），parser 前过滤。
3. **错误可恢复**：词法 / 语法错误只进各自的 errors / diagnostics 列表，
   不中断后续分析（同一文件尽量一次报全）。
4. **安全分层（已确认）**：默认安全；直接碰内存需裸指针，且「穿指针访问」
   一律要求 `unsafe { }` 上下文（§6）。
5. **学习曲线平缓（已确认）**：语言面最小、无特事特办（无 `**` 拆分之类的
   特殊规则）；日常代码走安全路径，底层能力显式 opt-in。
6. **双下划线是保留命名空间**：class 特殊方法用 `__` 前缀，普通方法不得以
   `__` 开头（§4.6.1）。

## 3. 词法

### 3.1 注释

| 形态 | Token | 说明 |
|------|-------|------|
| `// ...`（到行尾） | `LineComment` | 正式 token；喂 parser 前过滤 |
| `/* ... */` | —— | **不支持**：`/*` 被切为 `Divide` + `Multiply` |

### 3.2 标识符

`[_\p{L}$][_\p{L}\p{Nd}$]*`（`TokenKind::Identifier`），支持中文等 Unicode
字母。标识符是**硬保留字环境**：关键字不可作标识符（无上下文关键字机制，
属已知限制）。

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

约定：`1.` 会切为 `1` + `.`（小数点后必须跟数字）；字符串转义内容不校验，
非法转义留待语义阶段；数字中的下划线仅作分隔符保留（值语义延后）。

### 3.4 运算符与分隔符

多字符（最长匹配优先，先于单字符）：

```
++  --  <<  >>  <<=  >>=  ==  !=  ===  !==  <=  >=
&&  ||  ??  =>  ->  *=  /=  %=  +=  -=  &=  ^=  |=
```

单字符：`[ ] ( ) { } ; , = ? : . + - ~ ! * / % < > & ^ | @`

> **已确认变更**：删除 `**`（幂）与 `**=`。原因：`**p` 会被词法切为 `Power`，
> 需 parser 特判拆两层解引用，是「最费解的语法」；删除后 `**p` 天然是两个
> `*`。幂改为 `pow(...)` 内建（若回归）。删除后多字符运算符为 25 个。

### 3.5 关键字（硬保留）

| 分组 | 关键字 |
|------|--------|
| 模块 | `from` `import` |
| 修饰与声明 | `pub` `pri` `const` `let` `static` `func` `class` `struct` `trait` `new` `this` |
| 控制流 | `if` `else` `elif` `while` `for` `in` `break` `continue` `return` `default` `match` `case` |
| 逻辑词 | `and` `or` `not` |
| 错误处理 | `try` `catch` `finally` `throw` |
| 上下文 | `with` `as` |
| 类型 | `type` `is` |
| 异步 | `async` `await` `yield` |
| 外部 | `extern` |
| unsafe | `unsafe` |
| 弱引用 | `weak`（字段修饰） |

#### 内置符号（已确认）

语言内置三组符号（非关键字 token，词法层不新增保留字）：

| 内置 | 定位 |
|------|------|
| `@` | 双身份：语句位是编译器指令前缀（`@cfg(...)` 等）；路径位是路径别名「当前 crate 根」（等价 Rust `crate`） |
| `std` | 标准库路径别名（`from "std:xxx" import ...`） |
| `super` | 父级路径别名（`from "super:xxx" import ...`） |

### 3.6 词法错误

| 错误 | 触发 | 说明 |
|------|------|------|
| `InvalidCharacter` | 单字符无法匹配任何规则 | 如 `#` |
| `UnterminatedString` | 引号开头、缺少闭合引号 | 起始引号 + 最长可消费前缀 |

错误不进正式 token 流，只进入错误列表；同一文件继续分析其余部分。

## 4. 语法结构与 AST

### 4.1 程序结构

```
program: sourceElement* EOF;        // 空文件合法
sourceElement: moduleImport | typeAliasDeclaration | traitDeclaration
             | classDeclaration | structDeclaration | functionDeclaration
             | externFunctionDeclaration | directiveStatement | statement;
```

AST 根：`Program { items: Vec<Item>, span }`。

### 4.2 顶层 item（`Item`）

| Item | 对应文法 | 说明 |
|------|----------|------|
| `Import` | `moduleImport` | 具名 / 命名空间导入 |
| `TypeAlias` | `typeAliasDeclaration` | `type A<T> = ...;` |
| `Trait` | `traitDeclaration` | 支持可见性与编译器指令；成员同 class |
| `Class` | `classDeclaration` | 支持继承、`pub`、编译器指令、泛型 |
| `Struct`（**新增**） | `structDeclaration` | 值类型，见 §4.7 |
| `Function` | `functionDeclaration` / `externFunctionDeclaration` | 顶层函数 / extern |
| `Directive` | `directiveStatement`（`@foo;`） | 独立编译器指令语句 |
| `DirectiveBlock` | `directiveStatement`（`@foo { }`） | 编译器指令块 |
| `Statement` | `statement` | 顶层语句（含 `pub let`） |

顶层可见性：`pub` 只允许修饰 `class` / `struct` / `trait` / `func` / `let` /
`const` / `extern`（`pub extern func`）；`extern` 之后不能再写可见性。指令
（`@`）只允许修饰 `@foo;` / `@foo { }` / `func` / `class` / `struct` /
`trait` 声明与 class/struct 成员（含字段）。

#### 条件编译指令（2026-11-15）

`@cfg(predicate)` 可修饰函数、class/struct/trait、字段、方法及顶层指令块。
多个 cfg 按 and 合并；false 的节点在导入扫描、声明收集前剔除，不参与名称和类型检查，
但词法/语法错误仍报告。已处理的 cfg 不继续交给 HIR。独立 `@cfg(...);` 无修饰目标，报错。

predicate 支持 `true` / `false` / `unix` / `windows`，
`target_os = "linux"`、`target_arch = "x86_64"`、`target_family = "unix"`、
`feature = "name"`，以及递归 `all(...)` / `any(...)` / `not(...)`。
not 恰有一个参数；all/any 可为空（分别为 true/false）。所有参数都解析，
未知键、未知组合及多余 token 均报错，不因其他条件 false 而掩盖错误。
目标和 feature 由 AnalysisOptions 提供，默认宿主平台且 feature 集合为空。
`@packed` / `@align` / `@extern` / `@deprecated` 等示例不构成已实现指令。

#### 禁用标准库指令（2026-11-21）

入口模块可声明一次独立的 `@no_std;`，对本次编译的整个导入闭包生效。
指令在入口解析后、默认环境加载前处理；不读取标准库文件，不注入 `std:site`，
也不把标准库模块送入 HIR 或链接阶段。显式 `std:` 导入（包括 `std:exception`）报错。
依赖模块不能声明此指令；指令不接受参数，不能修饰声明或指令块。

```novella
@no_std;

func main() -> int {
    return 42;
}
```

该模式下没有默认的 `String` / `string`，普通、raw、f-string 字面量需要标准库而报错；
字节字面量 `b"..."` 仍按 `u8[]` 做语义检查。`@no_std` 仅禁用 Novella 标准库，
不改变目标平台的启动代码、C 链接驱动或显式用户 `extern` 声明。

#### 裸机入口与导出

`@no_main;` 是入口模块一次性的独立无参指令，禁止自动生成 C `main` 包装。
`@export("symbol")` 修饰有体顶层函数并采用 C ABI；只允许标量、裸指针、函数指针与
void 边界，拒绝泛型、async、extern、聚合和托管值。链接名字必须合法且唯一。
裸机目标 `x86_64-unknown-none-elf` 必须配合 `@no_std; @no_main;`；运行时 `none` 与
`managed` 独立选择。完整契约、限制与启动验证见 [裸机支持](freestanding.md)。

### 4.3 导入

```
fromImport: From StringLiteral Import importSpecifier (',' importSpecifier)* SemiColon;
directImport: Import StringLiteral SemiColon;
importSpecifier: identifier (As identifier)?;      // `as` 写本地名
```

AST：`ImportItem { raw_path, prefix, path, kind }`；
`ImportKind::Named { specifiers }`（`from ... import a, b as c`）与
`ImportKind::Namespace { local_alias }`（`import "mod"`）。

#### 4.3.1 路径别名（`xxx:` 前缀，已确认）

导入路径字面量支持 **`别名:路径`** 形式，`:` 前为路径别名（即
`ImportItem.prefix` 字段，`path` 为去除别名的逻辑路径）：

```
from "std:exception" import ValueError, ZeroDivisionError;
```

- 内置路径别名（语义分析阶段解析 `prefix` 时识别）：
  - **`@`** —— 当前 crate 根（等价 Rust 的 `crate`），
    `from "@:sub/mod" import ...` 从本 crate 根（最近的 `nov.toml` 所在目录）
    出发的绝对路径；
  - **`std`** —— 标准库路径别名；`"std:io"` 对应显式 std_root 下的 `io.nov`。
    未配置或缺少声明时报错，不从文档示例合成 API；`std:exception` 仅保留编译器的 Exception 静态根；
  - **`super`** —— 当前作用域的父级（类似相对 import 的 `..` 语义，
    `from "super:xxx" import ...`）。
- 路径语法：`:` 仅作别名分隔符且**最多一个**（`alias:path`）；别名之后的
  路径段一律以 `/` 分隔，段内再出现 `:` 由语义分析报错；crate 根由当前文件
  向上最近的 `nov.toml` 标记，找不到时 `@:` 报错；
- 未知别名：语义分析阶段报「未知路径别名」；`@` / `std` / `super` 在
  `"..."` 内只是字符串内容，与语句位的 `@` 指令前缀互不影响；
- 与 `importSpecifier` 的 `as`（本地重命名）互不冲突：`xxx:` 作用于**路径**，
  `as` 作用于**符号**。

#### 4.3.2 模块语义（已确认，sema 第一期落地）

- **模块 = 文件**：路径段按 `/` 映射目录，末段自动补 `.nov` 扩展名；
  无前缀路径相对**当前文件所在目录**解析；`super:` 先上溯一级目录；
- **装载模型**：从入口文件出发按导入闭包装载（worklist），**允许循环
  导入**——先全量装载并收集顶层声明，再统一绑定导入与检查模块体；
- **可见性**：仅显式 `pub` 的顶层符号可被其他模块导入（无 `pub` 默认
  模块私有）；导入到本地的符号**只读**（不可赋值）；
- **同名冲突**：导入绑定名与本地顶层符号重名、或两条导入引入同名绑定时
  报错（不允许隐式覆盖）；
- **`std`**：标准库符号登记为**外部符号**（类型未知、调用不查实参），
  待 std 存根落地后收紧；
- **模块顶层语句**：照常做语义检查（执行顺序属 codegen 阶段决定）；
- **项目清单 `nov.toml`**：语义分析仅用它定位 crate 根（`@:` 解析）；
  `novella build` 读取 `package.name` 来命名可执行文件。`version` 当前仅作元数据。
  `novella new` 生成的清单示例：

  ```toml
  [package]
  name = "my-crate"
  version = "0.1.0"
  ```

### 4.4 类型别名

```
typeAliasDeclaration: Type identifier typeParameters? Assign typeExpression SemiColon;
typeParameters: '<' typeParameterList '>';
typeParameter: identifier (Colon typeExpression)?;
```

- **可见性限制（本期）**：别名**不可跨模块导入**（文法上 `type` 前无
  `pub`，`pub` 清单不含类型别名），仅模块内可见；
- **泛型代入**：使用点检查实参数量并递归代入字段、方法、数组、指针、元组、
  函数与别名；具名实例保留实参身份，不擦除为原始类型。别名在定义环境解析，
  定义位置的类型投影保留符号形参，不被不同实例覆盖；
- **循环检测**：`type A = B; type B = A;` 类循环在展开点报「类型别名
  循环」；
- `T: Trait` 约束参与成员查找与实参检查，包括 trait 的泛型实参。类型参数用
  模块和声明位置区分；调用可从实参递归推导，不从期望返回类型反推，冲突或
  无法推导时报错。后端单态化是独立阶段。

### 4.5 编译器指令 `@`（directive 语义）

```
directive: At identifier ('(' directiveArgument* ')')?;
directiveArgument: '(' directiveArgument* ')' | ~(OpenParen | CloseParen);
```

**语义：编译器指令，等价于 Rust 的 attribute `#[...]`**——`@xxx(...)` 由编译器
在**编译期**按 `xxx` 解释并作用于其修饰的声明/块（可影响布局、代码生成、检查等），
不构造运行时包装。

- 括号内为**自由 token 流**（含嵌套括号，平铺保留），结构化解释延后到语义
  阶段（如 `@cfg(target_os = "linux")`、`@packed`、`@align(8)`、`@extern("C")`、
  `@deprecated`），届时按 `name` 查对应指令的处理规则；
- `@name` 无括号简写等价于空 token 流；
- 可修饰对象：独立指令语句 `@foo;`、指令块 `@foo { }`、`func` / `class` /
  `struct` / `trait` 声明与 class/struct 成员（方法与**字段**；字段指令
  保留在 `FieldMember.directives`，语义由语义分析解释）；
- 指令语句/块只允许单个指令；多个指令报错。

### 4.6 class / trait

```
classDeclaration: directive* Pub? Class identifier typeParameters? classHeritage? classBody;
classHeritage: '(' typeReference ')';              // 文法仅单父类，语言按多继承预留
classBody: '{' classMember* '}';
traitDeclaration: directive* (Pub|Pri|Pkg)? Trait identifier typeParameters? traitBody;
classMember: methodDeclaration | fieldDeclaration;
fieldDeclaration: directive* accessModifier? Static? Weak? identifier typeAnnotation? (Assign expression)? SemiColon;
methodDeclaration: directive* accessModifier? Static? Func identifier typeParameters?
    '(' parameterList? ')' typeAnnotation? ReturnArrow? typeExpression? (block | SemiColon);
accessModifier: Pub | Pri;
```

AST：`ClassItem { directives, name, type_parameters, members, parent, scope }`；
`TraitItem { directives, name, type_parameters, members, scope }`；
`ClassMember::Field` / `ClassMember::Method`；
`MemberModifiers { scope: Option<Scope>, is_static, is_weak }`。

- 方法**必须**以 `func` 开头：传统省略形态（`constructor(...)` /
  裸 `name(...)`）不受支持，parser 对之报「方法声明须以 `func` 开头」；
  构造函数语义由特殊方法 `__init`（§4.6.1）承载；
- `weak` 只允许修饰**字段**（成员修饰）；`weak` 修饰方法由 parser 直接报
  「`weak` 不能修饰方法」。

语义：

- class 是**引用语义**（引用计数，后续设计）：`new Foo(...)` 构造，赋值/传参
  共享对象；
- 字段可有初始化表达式（`x: i32 = 5`）与可见性 / `static` 修饰；
- 方法可 `pub` / `pri` / `static`，返回类型注解 `: T` 与 `-> T` 等价，纯声明
  形态（`;` 结尾）为抽象方法。

#### 4.6.1 类的特殊方法（已确认）

**命名以双下划线 `__` 开头即可（结尾 `__` 不强制）**，如 `__init`、
`__destroy`、`__str` 等（写 `__init__` 风格亦可，语义一致）；由编译器在
特定时机由编译器自动调用；用户也可以显式调用，仍须通过可见性、接收者和签名检查。

- 词法零改动：`__xxx` 本就匹配 `Identifier`（`_` 可为首字符）；`__` 前缀的
  识别在语义阶段按名字判定；
- 作用范围：**仅 class**（及 trait 声明的方法签名）；**struct 不支持特殊
  方法**——struct 方法名不得带 `__` 前缀（§4.7）；
- 已登记清单（sema 第一期）：`__init`（构造，`new` 的实参按其签名检查；
  无 `__init` 的 class `new` 不得传实参）、`__destroy`（析构）、`__str`
  （字符串化）、`__enter` / `__exit`（`with` 协议）；清单之外的 `__`
  前缀方法名报错；调用时机属 codegen；
- 双下划线同时是**保留命名空间**：普通方法不得以 `__` 开头，防止撞名。

### 4.7 struct（值类型，已确认新增）

```
structDeclaration: directive* (Pub|Pri|Pkg)? Struct identifier typeParameters? classHeritage? structBody;
structBody: '{' structMember* '}';
structMember: methodDeclaration | fieldDeclaration;   // 复用 class 成员文法
```

AST：`StructItem { directives, name, type_parameters, heritage, members: Vec<ClassMember>, scope, span }`。

语义定位（与 class 的分工见 §8）：

- **值语义**：赋值 / 传参 / 返回均为逐位拷贝（含 class 引用字段的「指针拷贝」，
  见 §8.3）；
- **无继承**：struct 不可有 `parent`，不可被继承——parser 仍按 classHeritage
  语法解析并把结果保留在 `StructItem.heritage`（AST 信息完整），
  「struct 继承不合法」由语义分析判定；
- **无构造器**：用字段初始化构造 `TypeName{ x: 1, y: 2 }`（§5.4）；
- **方法可选**：允许方法，方法内 `this` 为**编译器注入的可信指针**，字段访问
  无需 unsafe（§8.4）；
- **无特殊方法**：struct 不识别 `__` 前缀方法（保留命名空间仅限 class）；
- **`==` / `!=` 不适用于 struct 值**（§8.5），指针比较不受影响；
- **布局**：按字段声明顺序 + 自然对齐（C/FFI 兼容），无虚表、无隐藏字段。

### 4.8 函数

```
functionDeclaration: directive* Pub? Async? Func identifier typeParameters?
    '(' parameterList? ')' typeAnnotation? ReturnArrow? typeExpression? block;
externFunctionDeclaration: directive* Pub? Extern Func identifier typeParameters?
    '(' parameterList? ')' typeAnnotation? ReturnArrow? typeExpression? SemiColon;
parameterList: parameter (',' parameter)*;
parameter: identifier typeAnnotation? (Assign expression)?;    // 形参默认值
```

AST：`FunctionItem { directives, name, r#type, block, scope, is_extern, is_async }`；
`FunctionType { type_parameters, params, return_type }`；`Param { name, ty, default }`。

- 返回类型注解 `: T` 与 `-> T` 两种写法等价；普通有体函数和方法省略注解时，根据自身可达的 `return` 推导返回类型，嵌套 lambda 的返回独立处理；
  - 无值 `return` 与可达落尾按 `void` 参与合一；没有返回值时推导为 `void`；
  - 返回类型必须能合一，未定型整数与浮点最终落到 `i32` / `f32`，并检查字面量范围；不兼容分支或无法确定的类型要求补充注解；
  - 支持前向引用、跨模块及方法依赖；递归推导形成循环时，必须通过显式返回注解打断依赖；不从调用点推导参数或返回类型；
  - `extern` 和无函数体声明省略返回注解时仍为 `void`；特殊方法遵循各自协议的既定签名规则；
  - AST 的 `FunctionType.return_type` 只保存显式注解：省略时为 `None`（JSON 为 `null`），显式注解为 `Some`；推导结果由 sema 的类型表提供；
- 形参类型标注**可选**（`parameter: identifier typeAnnotation? ...`）：本期
  未标注的形参类型按「未知」处理——函数体与调用点均跳过该参的检查，
  参数类型推导属后续期；
- `extern func`（无函数体，`;` 结尾）声明外部函数，用于 FFI；
  - 参数类型必须显式标注（FFI 边界不做参数推导）；
  - 不得带泛型参数（FFI 边界无单态化）；
  - 不得命名 `main`（入口必须是 novella 函数）；
  - **调用 extern 函数是不安全操作**，必须在 `unsafe { }` 块内
    （对齐 Rust `unsafe extern fn`；外部符号无法验证参数有效性）；
  - 调用约定由 `@extern("C")` 类指令承载（指令语义属后续期），默认 C ABI；
- `async func` 为异步函数（`FunctionItem.is_async`）；
- `unsafe fn` 本期不做；`FunctionItem.is_unsafe` 字段为预留项，待 `unsafe fn`
  落地时一并加入（当前 AST 尚未含该字段）。

#### 4.8.1 程序入口 `main`（已确认）

- 语言要求**必须有顶层 `main` 函数**作为程序入口：`func main() { ... }`；
  （理论形态上入口可缺省——如仅作为库被其他程序导入时，语义分析阶段按
  「是否产出可执行目标」判定；默认编译为可执行文件时报「缺少 main」）。
- `main` 是**硬性入口约定**，非保留字：`main` 仍是普通 `Identifier`，靠名字
  与「顶层 + 无参 + 无泛型」形态识别；
- 入口判定：顶层函数，函数名 `main`，无参数；返回类型可为 `void` / `int`
  （退出码，语义阶段定），其余形态报错；
- 已落地的附加判定（sema 第一期）：`main` 不得带泛型、不得为 `async`、
  不得为 `extern`；仅**入口模块**要求存在 `main`（被导入的库模块不要求）。

### 4.9 语句

```
statement: variableStatement | returnStatement | breakStatement | continueStatement
         | throwStatement | yieldStatement | emptyStatement | block | ifStatement
         | whileStatement | forStatement | tryStatement | withStatement
         | unsafeStatement | expressionStatement;
```

| 语句 | 文法 | 说明 |
|------|------|------|
| 变量声明 | `Pub? (Const|Let) variableDeclarationList SemiColon` | 支持元组解构 `let (a, b)`；单绑定产 `Statement::Variable`，多绑定产 `Statement::VariableList`（同作用域按序求值，不包 Block） |
| 跳转 | `return` / `break` / `continue` / `throw` / `yield` | `yield` 值可缺省 |
| 块 | `block: '{' statement* '}'` | 花括号块 |
| if | `if/elif/else` 链 | 分支体为任意 `statement` |
| while | `while (expr) stmt` | |
| for | `for (init?; cond?; upd?)` / `for (x in expr)` | init 段声明不带 `const/let`，一律按 `let` 语义 |
| try | `try { } catch (e: T) { } finally { }` | 无 catch 必须有 finally |
| with | `with expr (as name\|pattern (: T)?)? { }` | 资源管理；表头**不带括号**（旧 `with (expr) { }` 形态已废弃，parser 会提示）；`as` 后跟单标识符或元组解构模式（嵌套括号内 / 实参内的 `as` 仍是类型转换） |
| **unsafe（新增）** | `unsafe { }` | 唯一 unsafe 上下文入口（§6） |
| 表达式语句 | `expression ;` | |

#### `for ... in` 迭代协议

- `for (pattern in expr)` 先求值一次 `expr`。数组按下标顺序遍历，绑定类型为数组元素类型。
- 其他类型通过可访问的实例方法 `iter()` 提供迭代器。每次进入循环时调用一次 `iter()`，由实现负责返回独立的迭代状态。
- 每轮先调用迭代器的 `hasNext() -> boolean`。返回 `false` 时结束。返回 `true` 时调用一次 `next() -> T`，再按 `T` 绑定变量或解构元组。`continue` 进入下一次 `hasNext()`，`break` 立即退出。
- 三个方法都不声明方法级泛型或显式参数，实例接收者 `self` 可以按普通方法规则写出。`next()` 须返回非 `void` 类型。成员查找沿用普通可见性、继承、类型实参代入和 trait 约束规则。隐式调用也参与异常流检查。

当前 `novella check` 支持该协议。HIR 的 `for-in` 降级目前覆盖数组路径。

### 4.10 表达式与优先级

（ANTLR 左递归：**靠前备选 = 优先级更高**。下表自上而下优先级递减。
已确认删除 `Power` 幂，新增 `&`/`*` 前缀与 `as` 转换。）

| 优先级 | 文法备选 | AST |
|--------|----------|-----|
| 原子 | `literal` / `identifier` / `(expr)` / 数组 / 对象 / `this` | `Literal` / `Identifier` / `Paren` / `ArrayLiteral` / `ObjectLiteral` / `This` |
| 元组 | `(a, b, ...)`（≥2 元素；`()` 与单元素 `(a,)` 均非法——`()` 报「空元组不是合法表达式」、`(a,)` 报「单元素元组不受支持」） | `Tuple` |
| match | `match x { case ... }` | `Match` |
| 后缀 | `.` 成员 / `[ ]` 索引 / 调用 / `new` 构造 / `++` `--` | `MemberAccess` / `IndexAccess` / `Call` / `New` / `Unary(PostInc/Dec)` |
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
| lambda | `x => ...` / `(a, b) => ...`（含 `x: T => ...`，其中 `: T` 是返回注解） | `Lambda` |
| 赋值 | `=` 与全部复合赋值 | `Assignment` |

元组用零基整数字面量索引，支持括号和一元正负号。动态索引与越界索引报错。
索引表达式的类型为对应槽位的类型。可写元组的槽位可以赋值。

> **`as` 的优先级**（已确认：低绑式）：位于「加减」之后、「移位」之前——
> 低于算术、高于关系比较，与 TS 观感一致：
> `p + 1 as u32` ⇒ `(p + 1) as u32`；`p as usize + 1` ⇒ `(p as usize) + 1`；
> `*p as u8` ⇒ `(*p) as u8`（一元高于 `as`）。

### 4.11 表达式节点与运算符

`Expression` 变体：`Literal` `Identifier` `Paren` `Tuple` `ArrayLiteral`
`ObjectLiteral` `This` `Unary` `Await` `Binary` `Is` `Ternary` `Assignment`
`MemberAccess` `IndexAccess` `Call` `New` `Match` `Lambda`
**`Cast`（新增）** 及 **struct 构造**（§5.4）。

运算符枚举：

- `UnaryOp`：`Plus` `Minus` `Not` `BitNot` `PostIncrement` `PostDecrement`
  `AddressOf` `DerefPointer`（新增）；
- `BinaryOp`：`Multiply` `Divide` `Modulus` `Plus` `Minus`
  `LeftShift` `RightShift` `LessThan` `GreaterThan` `LessThanEquals`
  `GreaterThanEquals` `Equals` `NotEquals` `IdentityEquals` `IdentityNotEquals`
  `BitAnd` `BitXor` `BitOr` `LogicalAnd` `LogicalOr` `NullCoalesce`；
- `AssignOp`：`Assign` `PlusAssign` `MinusAssign` `MultiplyAssign` `DivideAssign`
  `ModulusAssign` `BitAndAssign` `BitOrAssign` `BitXorAssign` `LeftShiftAssign`
  `RightShiftAssign`。

### 4.12 字面量 AST 与 lambda / match / 对象

- `Literal`：`Null` / `Boolean(bool)` / `String(String)`（保留原文） /
  `Number(String)`（保留原文与进制前缀）；
- `Lambda { is_async, params, return_type, body }`；
  `LambdaParams::Single` / `List`；`LambdaBody::Expr` / `Block`；
- `Match { subject, cases, default }`；`MatchPattern::Expr` / `Type`（`case : T`）；
  `MatchCaseBody::Expr` / `Block`；
- `PropertyAssignment { name, value: Option<Expression> }`：`{ a: 1, b }` 简写
  （`b` 等价 `b: b`，语义分析阶段展开）。

## 5. 类型系统

### 5.1 类型表达式（`TypeExpr`）

| 变体 | 形态 | 说明 |
|------|------|------|
| `Predefined` | `int` / `float` / `boolean` / `string` / `void` / `u8..u64` / `i8..i64` / `f8..f64` / `usize` / `isize` / `null` | 类型名在词法层是普通 `Identifier`，由语义按名字归类 |
| `Reference` | `Foo` / `Foo<T>` / `mod.Foo` | 类型引用（class / struct / 别名）；`qualifier` 承载命名空间限定段（经导入的模块别名解析，多级限定 `a.b.T` 本期不支持） |
| `Array` | `T[]` | 数组类型 |
| `Tuple` | `(T1, T2)` | 元组类型（≥2 元素；`()` / `(T,)` 非法；括号分组 `(T)` 即 `T` 本身） |
| `Function` | `<T>(a: int) -> int` / `func (a) -> T` | 两种写法合并；类型位箭头**唯一为 `->`**，类型位写 `=>` 报错（`=>` 专属 lambda） |
| `Union` | `A \| B \| C` | 扁平存储，集合语义 |
| **`Pointer`（新增）** | `T*`（可嵌套 `T**`，允许 `void*`） | 指针类型（§5.3） |

`TypeParameter { name, bound }`（`T` 或 `T: Bound`）；`typeAnnotation: Colon typeExpression`。

### 5.2 预定义类型（`PredefinedType`）

`Null` `Boolean` `Int` `Float` `Void` `U8` `U16` `U32` `U64` `I8` `I16`
`I32` `I64` `F8` `F16` `F32` `F64` `Usize` `Isize`。

宽度整数 / 浮点类型直接映射目标平台原生类型；`usize` / `isize` 为指针宽度。
**`int` ≡ `i32`、`float` ≡ `f32`**（语义阶段直接归一化，不存在独立的
「任意宽度」类型）；`string` / `boolean` / `void` / `null` 不出现在
`PredefinedType` 枚举中，由语义按名字归类为内置类型。

### 5.2.1 类型兼容与隐式转换（已确认，sema 第一期落地）

赋值 / 传参 / 返回的兼容判定（`assignable`）：

- **无隐式数值转换**：`i32` 与 `i64`、`int` 与 `float` 之间赋值报错，
  必须显式 `as`；
- **字面量归化**：整数字面量可赋任意整数类型、浮点字面量可赋任意浮点
  类型（字面量带「未定型」标记，进入变量后定型；按目标位宽溢出检查属
  后续期）；
- `null`（类型 `void*`）可赋任意 `T*` 与 class 引用（S5/§7.2）；
- `void*` ↔ 任意 `T*` 双向隐式（S8）；
- **子类引用 → 父类引用**隐式（单继承链）；父类 → 子类必须显式 `as`
  （unsafe）；
- 元组 / 数组同形后逐元素递归适用以上规则；联合类型：目标是联合时
  源可赋任一成员即可，源是联合时全部成员可赋目标才可赋（保守）；
- 类型参数 `T` 与错误恢复类型按各自规则处理；std 符号必须来自真实声明，不能按不透明外部符号放行
  （不再追加诊断）。

### 5.3 指针类型（已确认新增）

- 后缀形态 `T*`，可连写 `u8**` ≡ `Pointer(Pointer(u8))`（右结合，最右 `*` 在
  最外层）；
- 可在 `primaryType` 级别组合：`(A | B)*`、`(i32, i32)*`；
- 优先级：类型位 `*` 只出现在类型表达式**末端**，低于 `|` 联合
  （`A | B*` 即 `A | Pointer(B)`）；「指向联合的指针」写 `(A | B)*`；
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
- AST：`Expression::StructInit { name, properties, span }`（或并入现有表达式
  家族，落地时定具体变体名）。

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

### 6.2 操作符与 unsafe 属性

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

布尔 ↔ 整数等「非指针」算法转换不受 unsafe 约束；凡涉及指针两侧任一方向的
转换均须 unsafe 上下文。

### 6.4 unsafe 上下文（一条线规则）

「**穿过指针访问内存**」即需 unsafe：

- 解引用 `*p`、下标 `p[i]`、成员访问 `p.m`（读与写统一，无「读安全写不安全」
  的中间态）、指针算术（`±`/`++`/`--`/`+=`/`-=`）、指针互转与指针↔整数转换；
- 取址 `&x`（左值即可）、判空 `p==null`、指针比较 `p==q`、纯数值 `as`
  均为安全操作，unsafe 外可用；
- `unsafe { }` 可嵌套；块内允许普通语句；本期不提供 `unsafe fn` 与
  `unsafe` 表达式形态；
- 悬垂指针 / 越界 / NULL 解引用：unsafe 语义下不做静态证明（未定义行为），
  生命周期检查归属后续引用系统；
- **唯一例外**：struct 方法内的 `this` 是编译器注入的可信指针，`this.x`
  访问无需 unsafe（§8.4）。

### 6.5 语义规则摘要（供语义分析实现）

| # | 规则 |
|---|------|
| S1 | `&x`：x 为左值 ⇒ `T*`。安全。 |
| S2 | `*p`：p 为 `T*` ⇒ `T`。unsafe。 |
| S3 | `p[i]`：p 为 `T*` ⇒ `T`。unsafe。 |
| S4 | 指针算术：unsafe；`p1-p2` ⇒ `isize`；非法组合（指针+指针等）报错。 |
| S5 | 指针相关 `as`：unsafe；纯数值转换不受限。 |
| S6 | `p.m`：p 为 `T*` 自动解引用；读、写均 unsafe（struct 方法内 `this` 除外）。 |
| S7 | `p==q` / `p==null`：安全；异种指针比较报错。 |
| S8 | `void*`：隐式赋任意 `T*`；解引用 / 算术报错。 |
| S9 | `null`：类型 `void*`，隐式转任意 `T*`；未初始化指针解引用报错。 |
| S10 | 指针不隐式转整数或其他指针（必须 `as`）。 |

## 7. 对象模型与内存管理

### 7.1 定位回顾

| | class | struct |
|---|-------|--------|
| 语义 | **引用语义（ARC，本篇）** | 值语义（复制时递归维护拥有成员） |
| 分配 | 堆：对象头 + 字段区 | 栈 / 内嵌 / 手动 |
| 构造 | `new Foo(...)` → `__init` | `Foo{ x: 1 }` 字段初始化 |
| 析构 | 引用归零 → `__destroy`，再自动清理字段 | 无用户析构；拥有成员自动 release |
| 隐藏状态 | 引用计数头部（运行时） | 无 |

### 7.2 对象布局

```
class 对象（堆）：
┌────────────┬──────────────────────────┐
│ refcount   │ 字段区（按声明顺序+对齐）  │
│ (usize)    │                          │
└────────────┴──────────────────────────┘
       │
       └─ 引用（句柄）在语言层是「不透明引用」，不是裸指针；
          `&obj` 才显式取出 T*（指向字段区起始），且须保持对象存活
```

- 对象头对语言层透明：用户看不到 refcount，`as` 不能把引用当整数；
- class 引用默认可空：`null` 可赋给任意 class 引用；解引用前须判空。

2026-11-21：runtime 已实现对齐载荷、独立强/弱控制块和归零析构回调；class 自动 ARC 已接入原生后端。
以上图示仅表示不透明对象头与载荷关系，不规定其二进制字段偏移。
用户析构后逆序清理本类字段，再执行父类析构链。实现与可执行示例见
[runtime 说明](../crates/novella-runtime/README.md)。

### 7.3 引用计数规则（ARC）

**守恒原则：每次产生引用拷贝必须 retain（+1），每次引用失效必须 release（-1）；
归零时析构并释放。**

自动 retain/release 的时机：

| 场景 | 动作 |
|------|------|
| `new Foo(...)` 赋给变量 | 计数 1（转移，无 retain） |
| 引用赋值 `b = a` | retain(a) → release(旧 b) |
| 普通形参 | 被调用方 retain，返回前 release；实例接收者借用，由调用方保活 |
| 存入字段 / 数组元素 | retain |
| 从函数返回引用 | 转移所有权（调用方接收，无 retain/release 对） |
| 局部变量离开作用域 / 被覆盖 | release |
| `weak` 字段赋值 | 创建新 weak handle，再释放旧 handle；不增加强计数（§7.4） |

编译优化（后端）：转移场景（`new` 直接赋、返回值、`if` 分支内唯一引用）可
省略 retain/release 对，语义层只保证**引用守恒**，不规定具体计数次数。

完整表达式结束后释放匿名临时值；具名局部保持到覆盖或词法作用域退出。
break、continue、return 均清理离开的作用域。struct、tuple 和数组递归维护拥有成员。
析构时 strong 已归零，`self` / `this` 只能借用来访问字段；
sema 拒绝把它复制到局部、拥有字段、普通实参或返回值，也禁止调用自身实例方法，
避免经 helper 复活已开始析构的对象；字段对象的方法仍允许。

#### 7.3.1 示例

```novella
class Node {
    value: i64;
    child: Node;                 // 默认强引用
    weak parent: Node;           // 弱引用（§7.4）
    func __init(v: i64) {
        this.value = v;
    }
    func __destroy() {           // 归零时调用（资源清理）
        // 关闭句柄等
    }
}

func build() -> Node {
    let root = new Node(1);      // root: count=1（转移）
    let child = new Node(2);     // child: count=1
    root.child = child;          // child: count=2（retain）
    child.parent = root;         // weak：不 retain
    return root;                 // 转移所有权
}                                // child 离开作用域: count=1（仍被 root 持有）
```

### 7.4 循环引用与弱引用

ARC 缺陷：`root.child = child; child.parent = root;`（全强）形成环，永不归零。
对策：**弱引用 `weak`**。

- 语法：**仅字段**支持 `weak` 修饰（与 `static` 同位，文法
  `fieldDeclaration: directive* accessModifier? Static? Weak? identifier ...`），
  局部变量不做；
- 类型：`weak` 字段必须保存 class 引用（包括别名与已实例化的泛型 class）；
  struct 字段也可保存这样的弱引用。标量、裸指针、数组及不能证明为 class 的类型参数
  不能标记为 `weak`；无注解字段在初值推导后检查；
- 存储：字段保存独立控制块的 weak handle，不增加对象强计数；覆盖及字段销毁释放旧 handle；
- 读取：原子 upgrade 成功后得到拥有强引用的普通 class 值，完整表达式结束时清理；
  最后强引用开始析构后 upgrade 返回 `null`，对象不可复活。字段内 handle 不必物理清零；
- 空安全：读取过期 weak 得到 `null`，后续成员解引用仍须调用方判空；没有静态非空证明；
- weak 字段禁止取裸地址，避免控制块句柄被误当成 class 载荷地址；
- 原子计数保护 upgrade 与最后 release 的竞态；并发修改同一字段仍需调用方同步。

sema、HIR、codegen 与 runtime 已接通 weak 字段；显式 ABI 示例见
[runtime-weak](../examples/runtime-weak.nov)，自动 ARC 示例见
[arc-weak](../examples/arc-weak.nov) 与 [arc-lifetime](../examples/arc-lifetime.nov)。

```novella
class Tree {
    children: Tree[];            // 强（数组元素 retain）
    weak parent: Tree;           // 弱：打破父子环
    func __init() { this.children = []; }
}
```

### 7.5 `with` 资源管理协议

`with expr { }`（文法已定）调用对象的特殊方法管理生命周期：

| 时机 | 调用 |
|------|------|
| 进入块 | `expr.__enter()`（若定义） |
| 正常 / 异常离开 | `expr.__exit()`（若定义，恒调用） |
| 引用归零（含 with 结束） | `__destroy()` |

- `with` **不改变所有权**：它只是协议调度；对象是否释放仍由引用计数决定；
- `__enter` / `__exit` 是已定的 `__` 前缀特殊方法族（见 §4.6.1）；
- 典型用法（文件、锁、内存映射）：

```novella
with openFile("data.txt") as f: File {
    let line = f.readline();     // 块内使用
}                                // 离开：__exit() 被调用；引用归零则 __destroy()
```

### 7.6 与指针 / unsafe 的边界

| 操作 | 规则 |
|------|------|
| `&obj`（class 引用取址） | 得 `T*`，指向对象字段区起始；**安全操作**，但持有裸指针期间对象存活由调用方保证 |
| `*p` 解引用 / `p.m` / `p[i]` | 沿用 unsafe 一条线规则（§6） |
| `new` 返回 | 引用，**不可隐式转** `T*`，须 `&obj` |
| `as` | class 引用不可与整数/指针互转（引用是运行时句柄） |
| 裸指针持有 class 对象 | 悬垂风险属 unsafe 语义（未定义行为） |

内存泄漏 / 生命周期不在静态层证明（归属后续引用/借用设计）。

### 7.7 数组与容器

- `T[]`（class 元素）= 引用数组：元素 retain/release，数组本身由语言管理；
- `T[]`（struct / 标量元素）= 连续内存（已定）；
- `std` 容器（List / Map）基于以上语义实现；包级 API 草案见
  [`stdlib-design.md`](stdlib-design.md)（状态：待评审，未实现）。

### 7.8 待确认决策点

1. `weak` 语法：已定案为**字段修饰符**（与 `static` 同位），已入文法与词法；
2. 2026-11-21 weak 契约修订：读取以原子 upgrade 获取临时强引用，过期返回 null；
3. `with as` 已确认绑定原始资源。可选 `__enter` / `__exit` 无显式形参并返回
   void；`__init` / `__destroy` 返回 void，`__str` 返回 string；
4. 是否提供 `@arc` / 配置项切到 tracing GC（本稿：不做，ARC 唯一）。


## 8. struct 与 class 的分工

### 8.1 对比

| | `class` | `struct` |
|---|---------|----------|
| 语义 | 引用语义（引用计数，后续设计） | 值语义（拷贝即复制） |
| 分配 | 堆 / 计数管理 | 栈 / 手动（配合指针） |
| 构造 | `new Foo(...)` | `Foo{ x: 1 }` |
| 继承 | 有（单父类，按多继承预留） | 无 |
| 特殊方法 | 有（`__` 前缀） | 无 |
| 内存角色 | 日常对象 | 底层数据载体、FFI、寄存器/驱动 |

### 8.2 值语义

`let b = a;`（a、b 均为 struct）即逐字段/逐位拷贝；函数参数、返回值同样按值。
struct 数组 `T[]` 为连续内存。

### 8.3 class 引用字段 = 指针拷贝

struct 内允许 class 引用字段，拷贝时**按位复制句柄**（不深拷贝、不自动增减
引用计数）；引用计数由使用方负责——与裸指针的手动管理哲学一致。

### 8.4 struct 方法与可信 `this`

- struct 允许方法（复用 `methodDeclaration` 文法与 `MethodMember` AST）；
- 方法内 `this` 是**编译器注入的可信指针**（指向调用时实际对象），因此
  `this.x` 访问**无需 unsafe**——这是对 §6.4 统一规则的唯一例外，其正当性
  在于 this 由编译器保证有效性，非用户拼装；
- 方法调用形态：值上调用（`a.norm()`）自动取址；指针上调用（`p.norm()`）
  沿用穿指针规则（调用点本身在 unsafe 内）。

### 8.5 struct 的 `==`

`a == b`（struct 值）**非法**（编译错误，提示用逐字段比较或后续 `eq` 内置）；
指针比较 `pa == pb`、`pa == null` 不受影响（§6.2）。

## 9. 异常与执行模型

### 9.1 异常处理

#### 9.1.1 抛出

- `throw expr;`：expr 类型必须是 **class 引用**（`Exception` 或子类，
  `std:exception` 提供，见 §9.1.4）；抛 struct/标量报错；
- throw 对异常对象**retain 一次**并随传播路径持有，catch 消费后 release；
- 允许 `throw;`（重抛当前异常，仅 catch 块内合法）。

#### 9.1.2 捕获

```
tryStatement: Try block (catchClause+ finallyClause? | finallyClause);
catchClause: Catch '(' identifier typeAnnotation? ')' block;
```

- 多个 catch 按**声明顺序**匹配，第一个类型相符（含子类）的生效；
- `catch (e)`（无类型注解）等价 `catch (e: Exception)`——捕获一切；
- 类型不匹配的异常继续向上传播；无 try 包裹的异常到达 `main` 之外 =>
  运行时终止（打印异常消息 + 栈回溯，终止语义由 codegen 定义）。

#### 9.1.3 finally

- 无论正常 / 异常 / return / break / continue 离开 try，finally 恒执行；
- finally 内的 throw / return 覆盖外层控制流（与主流语言一致）；
- `catch` 与 `finally` 可共存，`try-finally`（无 catch）合法。

```novella
func div(a: i64, b: i64) -> i64 {
    if (b == 0) throw new ZeroDivisionError("division by zero");
    return a / b;
}

func demo() {
    try {
        let x = div(10, 0);
    } catch (e: ZeroDivisionError) {
        println(e.message);
    } catch (e: Exception) {
        println("other: " + e.message);
    } finally {
        cleanup();               // 恒执行
    }
}
```

#### 9.1.4 错误对象模型

编译器为 `std:exception` 的具名/命名空间导入提供 `Exception` 静态名义根。
当前根构造器无实参，throw/catch 只接受该根或子类；`throw;` 仅 catch 内可用，
catch 不得被前面的异常类型覆盖。以下字段与标准派生异常是标准库的后续实现示意，
不代表它们已由外部符号自动获得相同的静态身份：

```
class Exception {
    message: string;
    func __init(message: string) { this.message = message; }
}
class ValueError (Exception) { ... }      // 继承（见 §4.6）
class ZeroDivisionError (Exception) { ... }
```

用户可定义自己的 Exception 子类（继承 + `__init`）。

### 9.2 求值顺序与控制流语义

- 表达式求值：二元运算按**左操作数先于右操作数**求值；`&&` / `||` 短路
  （右侧可能不求值）；`??` 短路；`?.`（若未来引入）短路；
- 普通赋值：先求 RHS，再定位 LHS，最后写回（`a[i] = f()` 中 `f()` 先执行）；
- 复合赋值：定位 LHS 一次、读取旧值、求 RHS、计算并写回；
- 函数 / 方法实参：**从左到右**求值；
- 返回值：`return expr;` 先求 expr；`return;` 返回默认值（void）。

### 9.3 async / await 执行模型

#### 9.3.1 定义

```
async func foo() -> T { ... await p ... }
await expr;         // 仅 async 上下文
```

- `async func` 编译为**状态机**（async 函数不立即分配线程）；await 使状态机
  挂起并把控制权交还调度器；
- 调度模型：**单线程协作式（v1）**——事件循环 / 任务队列，无抢占；
  （多线程 / 并行模型列为后续，见 §9.6）；
- await 期间：被捕获的变量按引用计数保持存活（闭包 retain），无泄漏。

#### 9.3.2 语义约束

- `await` 只能出现在 async 函数（或异步 lambda）体；
- `Future<T>` 为可写的内建类型；async 调用始终得到 Future<T>，await 只接受 Future。
- async 函数返回值可以是 `T` 或 `void`：调用方 `await f()` 得到 `T`；
  不 await 时得到 `Future<T>`（句柄，可存储 / 传递）；
- async 块内可嵌套 await；同步代码不可 await（报错）。

```novella
async func fetch(url: string) -> Response {
    let resp = await http_get(url);      // 挂起、交还控制权
    return resp;
}

async func main_async() {
    let fut = fetch("https://x");        // Future<Response>，不阻塞
    let r = await fut;                   // 挂起直至完成
}
```

> main 保持同步，不能直接使用 await；
> 上例用 `async func main_async` 演示将其包在 async 上下文中的写法。

### 9.4 yield

- `yield` 关键字**保留**，`yieldStatement`（`yield expr?;`）仍在语法层；
- `yield` 的完整语义**待定**：候选方向为协程 / 可恢复函数（后续评审），v1 先仅保留语法形态、由语义阶段按「未支持」报错或按未来协程设计实现；

### 9.5 与对象模型 / unsafe 交互

- 异常路径必须**保持引用守恒**（unwind 沿途释放引用）——语义层规则与
  ARC 一致，codegen 负责栈展开时按作用域 release；
- async 挂起期间对象存活由引用计数保证（闭包捕获）；
- unsafe 代码内 throw/await：throw 允许（异常对象正常计数）；await 在 unsafe
  块内可允许（不改变内存语义）；unwind 后的裸指针视为悬垂（未定义）——
  `声明：抛异常/await 挂起期间跨作用域持有裸指针是未定义行为`。

### 9.6 运行时后续决策点

1. 调度模型：单线程协作（本稿）vs 线程池并行（后续扩展是否预留接口）；
2. 顶层 await 已确认不支持：`main` 同步；await 仅用于 async 上下文；
3. 异常实现的运行时选型：栈展开表（unwind）vs 方案式（返回值）；v1 需要
   unwind，成本在 codegen；
4. `yield` 的空值语义与 `Option` 的关系（本稿先不引入 `Option`，用
   `?`+`null` 约定，后续评审）。

## 10. 错误处理语义

- 错误分两类：词法错误（非法字符、未闭合字符串）与语法错误（token 序列不
  符合文法）；均**可恢复**——记录后继续分析同一文件其余部分；
- 语义错误（类型不符、unsafe 违规、未知路径别名、缺少 `main` 等）由语义
  分析阶段产出；
- 当前静态覆盖以 [sema-coverage.md](sema-coverage.md) 为准。@cfg 在声明收集前生效，
  其他未定义指令及 yield 明确报 error；std 需要真实模块声明并执行完整检查。
  多级限定模块、限定名 struct 初始化仍受既有 parser 范围约束；未使用符号
  warning 不是本轮完成标准。异常展开、异步状态机、ARC 与泛型单态化尚不属于
  本轮运行时交付。
- 诊断渲染为 rustc 风格（`error: ...` + `--> 文件:行:列` + 源码行 + caret，
  列号 1-based 展示、caret 按字符不错位）——渲染细节属编译器设计。

## 11. 快速上手（完整示例）

```novella
// 路径导入（std 内置路径别名）
from "std:exception" import ValueError, ZeroDivisionError;

// 编译器指令（directive，等价 Rust attribute）
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

## 12. 静态语义补全的已确认规则（2026-11-15）

- class 括号列表按解析到的符号区分唯一父类和 trait，不按排列位置猜测。trait
  只有方法声明，不允许字段与默认方法体。实现签名、继承覆写与抽象类实例化
  均在静态阶段检查；构造器不继承。子类字段不能遮蔽继承成员，方法不能遮蔽字段。
- 成员默认/pri 可在同模块、所属类型及已确认的子类内部访问；跨模块类型外需 pub。
  static 成员通过类型访问，实例成员通过实例访问。self 仅实例方法首参，
  显式注解须为所属类型（含符号泛型实参）；static 无 self/this。
- 匿名对象按字段名和类型作结构检查，允许宽对象赋给窄对象；局部变量仅在
  所有可达路径赋值后读取。未标注普通形参继续容错，不启动普通形参类型推导。
- 值 match 须穷尽：布尔 true/false、可判定联合类型模式可省 default，其他情况
  需 default。块分支为 void，禁止与值分支混合；重复常量、被前项覆盖的类型
  模式拒绝。void match 允许部分覆盖。
- match 按顺序检查模式。命中分支的入口只继承已经求值的模式产生的赋值。
  嵌套在其他表达式中的 match 块也可通过 `return`、`throw` 或循环跳转提前退出。
- b 字符串为 u8[]，只允许单字节字符和字节转义；f-string 使用 {expr}、
  {{/}}，不支持格式说明符。f16 为 IEEE binary16，f8 为 E4M3FN，按最近偶数
  舍入的有限范围检查；usize/isize 使用显式 32/64 位分析配置，缺省为宿主。

最小正反例、类型投影与 LSP/HIR 验证见覆盖矩阵的本轮验收记录。
