# ANTLR4 语法定义文件

`NovellaLexer.g4` / `NovellaParser.g4` 描述 Novella 语言**当前**的词法与语法。

对齐方向是单向的：**代码是基准，文法跟随代码**（不是拿文法去要求代码）。

| 文件 | 事实源 |
|------|--------|
| `NovellaLexer.g4` | `novella-core` 的 `TokenKind`（105 个词法变体）+ `novella-lexer` 的 `RawToken` 规则 |
| `NovellaParser.g4` | `novella-ast` 的 AST 节点定义，以及 `novella-parser` 已实现的分支行为 |

## 约定

- token 规则名与 `TokenKind` 变体名一一对应，顺序与 logos 声明顺序一致
  （logos 与 ANTLR 同为「最长匹配优先，同长取声明在前者」）。
- 文法里不保留代码中不存在的结构：AST 没有对应节点，文法就没有对应产生式。
- 值语义（字符串转义、数字进制、基础类型归类等）不在文法层表达，统一延后到语义分析阶段。
- 行注释 `LineComment` 是正式 token（供高亮消费），语法层透明：调用方在喂给 parser 前过滤。

## 非 token 规则

以下规则不产生正式 token，故在 `TokenKind` 中没有对应变体：

- `WhiteSpaces` / `LineTerminator`：`-> skip`，对应 logos 的 `#[logos(skip ...)]`；
- `UnterminatedString` / `UnexpectedCharacter`：`ERROR` 通道，对应 `LexError` 的
  未闭合字符串 / 非法字符两类错误（代码中错误 token 只进 `errors` 列表、不进 token 流）；
- `TokenKind::Eof` 对应 ANTLR 内建的 `EOF`，`TokenKind::Error` 对应 `ERROR` 通道。

## 校验

```bash
# 文法编译（预期无错误、无告警）
antlr4 -o target/antlr-align/gen grammar/NovellaLexer.g4
antlr4 -lib target/antlr-align/gen/grammar -o target/antlr-align/gen grammar/NovellaParser.g4
```

已做过的实机交叉验证（一次性台架，产物在 `target/` 下，不入库）：

- **词法**：样例覆盖全部 105 种 token（含边界：`0xg`、`007`、`1.5e-3`、`f"…"`、Unicode 标识符、
  `/*` 被切成 `Divide`+`Multiply`、各类空白），ANTLR 词法器与 `novella-lexer` 的
  token 流（种类 + 原文，含 `Eof`）逐条一致，词法错误数量与错误 token 文本一致；
- **语法**：覆盖 AST 全结构（导入 / 类型别名 / 类 / trait / 函数 / 装饰器 / 控制流 / match /
  lambda / try / with / 各类字面量与运算符）的样例通过 `NovellaParser.program()`，0 syntax error；
  已砍结构（交叉类型 `&`、`objectType`、`macro` / `async` / `stream` / `continue` 语句、
  字面量形态的预定义类型、块注释）均被正确拒绝。

## 已知的代码侧落差点（文法已按代码收紧，代码是否需要补齐由代码侧决定）

- 词法仍保留 `Stream` / `Macro` 关键字，但 AST 与文法都没有对应结构
  （`Macro` 只有调用侧 `Expression::MacroCall`）；若确认不再支持，应连同词法与高亮映射一并移除。
- 标识符是**硬保留字**：`novella-parser` 只接受 `TokenKind::Identifier`
  （`except_identifier` 及各处校验），因此 `func new()`、`obj.type` 这类写法当前不可解析。
  若需要软关键字，应先放宽 parser 侧校验，再放宽文法的 `identifier` 规则。
- for-init 段不带 `let` / `const`（AST 规定统一按 `let` 语义承载）；`for (i = 0; ...)` 既可读作
  声明也可读作赋值，文法按备选顺序取「声明」，与 `ForInit` 的注释一致。
- 返回类型注解沿用 AST 文档所记片段 `typeAnnotation? ReturnArrow? typeExpression?`，
  即 `func f(): T` 与 `func f() -> T` 两种写法都被接受（解析结果同落在单个可选字段上）。
