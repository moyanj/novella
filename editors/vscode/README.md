# Novella for VS Code

novella 语言的 VS Code 支持：**词法级语法高亮**（semantic tokens）。

高亮规则不在本插件内——插件只负责启动 `novella-lsp` 语言服务器，
由服务器复用编译器自身的词法器（`novella-lexer`）产出语义 token。
因此染色结果与编译器对源码的切分**永远一致**，不存在两套词法规则漂移的问题。

## 染色范围

| 类别 | 覆盖内容 |
|------|----------|
| `comment` | 行注释 `// ...` |
| `keyword` | 全部关键字，以及 `null` / `true` / `false` |
| `number` | 十进制、十六进制 `0x`、八进制 `0o`、二进制 `0b` 字面量 |
| `string` | 普通 / 格式化 `f"..."` / 原始 `r"..."` / 字节 `b"..."` 字符串 |
| `operator` | 算术、比较、逻辑、位运算、赋值、箭头等运算符 |

标识符与括号、分号等分隔符**不参与染色**，保持主题默认色：
它们的语义角色（变量 / 类型 / 函数）需要 parser 与语义分析才能判定，
词法阶段强行着色反而会误导主题配色。

## 安装

### 1. 安装语言服务器

服务器是 Rust 二进制，先构建并放进 `PATH`：

```bash
cargo build --release -p novella-lsp
install -Dm755 target/release/novella-lsp ~/.local/bin/novella-lsp
```

确认可用：

```bash
novella-lsp --help 2>&1 | head -1   # 无参数时等待 stdio 输入属正常现象
```

### 2. 安装本插件

```bash
cd editors/vscode
pnpm install
pnpm run package                # esbuild 打成自包含单文件 + 生成 vsix
code --install-extension novella-language-0.1.0.vsix
```

`pnpm run package` 会先用 esbuild 把 `src/extension.js` 与
`vscode-languageclient` 一起打包进 `dist/extension.js`，
因此 vsix 无需随包分发 `node_modules`。

开发调试时可在本目录按 `F5` 启动「扩展开发宿主」窗口，
打开任意 `.nov` 文件即可看到高亮效果（首次需先执行 `pnpm install`）。

## 配置

| 配置项 | 默认值 | 说明 |
|--------|--------|------|
| `novella.lsp.path` | `novella-lsp` | 服务器可执行文件路径；未安装到 `PATH` 时指向 `target/debug/novella-lsp` 即可 |
| `novella.lsp.args` | `[]` | 追加的启动参数 |

配置变更后插件会自动重启服务器，无需重载窗口。也可以在命令面板运行 **Novella: 重启语言服务器**，例如构建了新版本的 `novella-lsp` 后，无需重载 VS Code 窗口即可让插件重新启动服务器进程。

## 故障排查

**没有高亮**

1. 检查输出面板：菜单「查看 → 输出」，右上角下拉选择 `Novella Language Server`；
2. 确认 `editor.semanticHighlighting.enabled` 未被设为 `false`
   （设置项 `editor.semanticHighlighting.enabled`，默认 `true`）；
3. 若日志提示找不到可执行文件，用 `novella.lsp.path` 指定绝对路径。

**服务器启动即退出**

在终端直接运行 `novella-lsp` 并观察 stderr 输出——服务器日志全部写在 stderr，
stdout 保留给 LSP 协议报文。

## 协议能力

服务器只实现了语言服务的最小闭环：

- `initialize` / `initialized` / `shutdown` / `exit`
- `textDocument/didOpen` / `didChange`（全量同步）/ `didClose`
- `textDocument/semanticTokens/full`

未实现的方法（如 `textDocument/hover`）按 JSON-RPC 规范返回 `-32601`：
诊断、补全、跳转等能力需等 parser 与语义分析落地后再扩展。
