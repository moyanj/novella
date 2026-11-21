# sema 设计对照待办备忘

> 更新：2026-11-15。S00–S14 的旧验收仅证明当时规则清单，不等同整个语言设计完成。
> 本文记录本轮纠偏、实现边界和后续交接；架构问题见 [architecture-review.md](architecture-review.md)。

## 本轮需求与完成情况

| 编号 | 工作 | 当前状态 |
| --- | --- | --- |
| TD01 | std 强制检查 | 已修复：删除 External；std_root 下的真实模块参与名称、可见性、类型、实参数量/类型、返回及泛型检查；缺声明报错，未编造标准库 API；Exception 只保留编译器静态名义根 |
| TD02 | 指针 ++/-- | 已修复：可写 T* 在 unsafe 内保留指针类型并允许后缀自增减；安全上下文、void*、不可写目标拒绝；HIR 按地址定位、读取旧值、指针运算、写回降级 |
| TD03 | 块体 lambda 返回检查 | 已修复：无注解块体合并自身可达返回与落尾 void，不兼容报错；嵌套 lambda 独立；不落尾 finally 覆盖之前返回；返回类型保留到调用点和 HIR |
| TD04 | @ 条件编译 | 已实现 @cfg：声明收集前筛选函数、class/struct/trait、字段、方法及装饰块；错误条件报错；CLI/LSP 使用同一配置；未定义指令继续报错 |
| TD05 | lambda this | 已修复：继承词法 this 类型上下文；lambda 仍重置循环、异常、unsafe、async 等执行上下文；捕获环境的运行时生成仍属 HIR/后端边界 |
| TD06 | 删除宏残留 | 已清理：Macro 关键字、MacroCall AST、后缀解析、ANTLR 文法及 lexer/HIR/LSP/语法生成器消费者同步移除；macro 成为普通标识符，f!(...) 语法拒绝；Rust proc-macro 不受影响 |
| TD07 | 设计与完成声明 | 已同步：特殊方法允许直接调用，仍检查签名/接收者/可见性；废止 std 不透明策略；历史计划不再作为“全部 sema 完成”的证明 |
| AR01–08 | 架构审查契约 | 已修复对应问题：统一诊断、多入口 LSP、版本快照、声明依赖定型、声明索引、独立定义表、错误类型状态、后台分析与过期任务处理；边界详见架构审查 |

## 条件编译与标准库配置

```novella
@cfg(all(target_os = "linux", not(feature = "minimal")))
func platform() -> i32 { return 1; }
```

cfg 支持 true/false/unix/windows，target_os/target_arch/target_family/feature 字符串比较，
递归 all/any/not（not 恰有一个参数）。多个 cfg 按 and 合并，所有条件均解析；
关闭节点仍要求词法/语法合法。独立 @cfg(...); 没有修饰目标，报错。
不根据示例自行实现 @packed/@align/@extern/@deprecated 等指令。

```sh
cargo run -p novella -- compile main.nov -o result.json \
  --std-root /project/std --feature debug \
  --target-os linux --target-arch x86_64 --target-family unix
```

公开 API：analyze_with_options(loader, entry, AnalysisOptions)，包含 target、cfg、std_root、module_only。
默认目标为宿主，feature 为空；module_only 默认 false，CLI 仍检查 main。
LSP 的 initializationOptions 支持 stdRoot、features、targetOs、targetArch、targetFamily 和绝对路径 entryPaths；
未配置 entryPaths 的打开文档按普通模块检查，不以最近编辑文件猜程序入口。

std:io 映射到 std_root/io.nov，子目录同理；拒绝 . / .. 路径段。
std:exception 仅允许 Exception 的具名/命名空间类型身份，不允许未知导出、未知类型或任意成员放行。
当前保留该静态根以支持已确认异常检查，不表示实现了标准库异常模块或运行时。

## 仍需后续完成的工作

- 标准库真实源码、签名及运行时实现。当前已有 [stdlib-design.md](stdlib-design.md) 草案，
  标为待评审、未实现；它不是可加载的 .nov 声明。String 非内建迁移、链接清单等属于该草案后续工作，
  本轮未用草案示例虚构内建 API，也未隐式改变既有 string 类型。
- 未定义的其他 @ 指令须先有正式语义；当前只实现已明确的条件编译，未知指令报错。
- 无注解普通形参的上下文推导、yield 完整语义等原设计边界；不能把 Unknown 当作具体类型交给后端。
- HIR/后端仍需闭包捕获环境、动态派发、泛型单态化、异常展开、异步状态机、ARC/weak 等运行时工作。
  sema 的静态检查完成不等于这些运行时已经可用。
- LSP 仍按整个导入闭包分析，不是增量编译器；取消在模块/阶段边界生效，不中断单次 parser。
  文档语法快照构建仍同步；未提供磁盘监视通知，磁盘依赖在下一次文档同步触发分析时重读。

宏展开和“禁止直接调用特殊方法”均已删除，不再列为待实现任务。

## 兼容性

无数据迁移，源码/API 直接替换：Macro/MacroCall 序列化变体删除；
AnalyzedModule.syntax_diagnostics 删除，统一从 DiagnosticReport.modules[id] 的阶段切片读取；
TypeView 新增 Error；BindingTable 新增独立 definitions；SemaResult 新增 DeclarationIndex。
所有 ModuleId/Span/DeclarationId 只对同一次分析及其源码快照有效，不可跨版本缓存使用。

## 本轮验收记录

- 临时正反例矩阵：46 项全部符合预期，覆盖 std 缺声明/签名/返回/可见性、指针、
  块体 lambda、嵌套与 finally、词法 this、特殊方法显式调用、宏移除及 cfg 条件错误；
  另含声明顺序互换、std 泛型检查及重定义错误恢复，避免定位精确化引入下标越界。
- 公开消费者验证：纯语法错误 has_errors 为真且 HIR 拒绝；Error 投影独立拒绝；
  指针 ++/-- 与块体 lambda 可降级；跨模块默认参数、字段初值和默认表达式 lambda 的定义模块归属正确。
- LSP 实际 stdio JSON-RPC：12 项通过，覆盖多入口、无引用声明及形参定位、依赖诊断刷新、
  关闭未保存文档、倒序版本、诊断去重、cfg/std 配置、分析期间响应请求、过期任务取消，
  以及共享模块跨入口的引用/重命名合并。5000 函数输入中，协议请求先于该版本语义诊断返回；
  本次观测约 200ms，仅为非阻塞路径验证，不是性能保证。
- 工作区 Clippy（all-targets，-D warnings）、cargo test --workspace（106 项，零失败）、
  cargo doc --workspace --no-deps（无警告）、cargo fmt --all --check、git diff --check 均通过。
  HIR smoke 成功；TextMate 语法已重新生成且 --check 通过。既有测试不能替代上述设计正反例。

临时验证报告位于 /tmp/novella-design-audit-QPZnkb，仅作本次运行证据，不是仓库长期测试资源。
CLI 错误仍以 JSON diagnostics 判断，不以进程退出码代替。
