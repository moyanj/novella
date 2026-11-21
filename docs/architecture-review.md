# 架构审查记录

- 审查日期：2026-11-15
- 源码基线：`main@ca976da3c332be9e478f78bbf3973e297931564e`
- 范围：编译阶段边界、语义产物契约、声明身份、LSP 工程状态与分析调度。
- 历史方法：静态阅读主分支源码与调用关系，当时未运行复现、测试或性能测量。
- 历史审查不含当时未提交修改；本次工作区修复和运行验证单列如下。

## 2026-11-15 工作区修复复查

用户已授权连同实现修复。上面的 commit 是**历史审查基线**，下文原始发现保留供追溯，
不再代表当前工作区。当前改动及验证范围如下；“已修复”针对列出的契约缺陷，
不意味着已完成增量编译或所有语言运行时。

| 项 | 当前状态 | 实现与约束 |
| --- | --- | --- |
| 1 统一诊断 | 已修复 | 全部诊断唯一存储于 DiagnosticReport；has_errors 覆盖词法/语法/语义/加载；ModuleDiagnostics 提供阶段切片及 ordered；CLI/LSP/HIR 同步消费 |
| 2 工程入口 | 已修复 | LSP 保留多个分析根与导入闭包；initialize.initializationOptions.entryPaths 显式指定可执行入口；其他打开文档按 module_only 检查，不猜测最近编辑文件为 main 入口；更新后重发全部打开文档诊断 |
| 3 快照生命周期 | 已修复 | 复用 SourceDoc 的 ParsedSource；缓存仅在源码完全相同时复用，cfg 筛选副本；版本倒退不接受；关闭文档移除 overlay 并重建；所有语义 span 使用分析时 source 换算 |
| 4 声明定型 | 已修复 | 函数体之前完成字段和顶层绑定的依赖定型，显式诊断循环依赖；实现见 check/fields.rs、check/globals.rs |
| 5 声明身份 | 已修复 | DeclarationId=(module,名字 span)，仅在单次分析内有效；DeclarationIndex 定位参数默认值和类型字段初值；HIR 不再扫描全部 AST 或解码函数键，lambda 以 source_module/is_lambda 表达归属 |
| 6 定义索引 | 已修复 | BindingTable.definitions 独立覆盖全局/局部/形参/字段/方法；无引用的声明也可定位；函数/方法/捕获异常名字范围精确化；导航不再从 references 反推定义 |
| 7 类型状态 | 已修复 | 删除 std External 放行；TypeView::Error 与 Unknown 分离；HIR 分别拒绝已诊断错误和未定型输入 |
| 8 分析调度 | 已修复 | 协议接收、文档状态、分析工作线程分离；最新任务覆盖排队任务，在模块/阶段边界协作取消旧任务；仅版本一致的结果可提交；等待定型时导航返回空结果；语义计算不阻塞后续协议请求 |

配置入口示例（客户端 initialize 参数）：

```json
{"initializationOptions":{"entryPaths":["/project/main.nov"],"stdRoot":"/project/std","features":["debug"],"targetOs":"linux","targetArch":"x86_64","targetFamily":"unix"}}
```

没有 entryPaths 时，LSP 只进行模块检查；CLI compile 默认仍要求 main。
目前按完整导入闭包重建，不承诺模块级增量缓存。取消是协作式，不能中断正在执行的单次 parser 调用；
语法快照构建仍在文档状态线程，后台处理的是完整语义图。导航在新版本分析完成前不复用旧快照。
磁盘依赖在下一次打开/修改/关闭触发分析时重读；尚未提供磁盘文件监视通知处理。

公开接口变更采取直接替换：移除 AnalyzedModule.syntax_diagnostics，改用
DiagnosticReport.modules[id].syntax_diagnostics()；新增 TypeView::Error、DeclarationIndex 和
BindingTable.definitions。server::run 接收拥有所有权且 Send + 'static 的 reader；HIR 函数新增
source_module/is_lambda。跨快照不允许复用 ModuleId、Span 或 DeclarationId。

### 当前验证

详见 [sema-design-todo.md](sema-design-todo.md) 的验收记录。历史静态审查与本次运行验证须分开引用。

## 历史审查：总体判断

当前主要问题是阶段间的数据契约不完整：诊断需要消费方重新合并，声明需要下游重新搜索，语义分析结果与源码版本没有明确关联。部分正确性依赖调用顺序与调用方补救。

crate 数量、文件长度以及显式的枚举映射本身不构成架构缺陷。建议优先解决影响分析正确性的契约问题，再进行局部去重和文件拆分。

## 历史发现 1. 诊断缺少完整、统一的查询入口

**优先级：高。状态：源码可直接确认。**

依据：

- `crates/novella-sema/src/lib.rs`：`DiagnosticReport::has_errors` 只检查 `global` 和 `diagnostics.modules`。
- 同文件 `analyze_with` 组装结果时，将词法、语法诊断移到 `AnalyzedModule.syntax_diagnostics`，`ModuleDiagnostics` 只保留语义诊断。
- `crates/novella-hir/src/build/module.rs`：`build_hir` 在调用 `has_errors` 后，还要单独检查模块语法与加载状态。
- `crates/novella-cli/src/main.rs`：CLI 自行合并两处模块诊断并排序。

影响：只有词法或语法错误、没有全局或语义错误时，`DiagnosticReport::has_errors()` 可能返回 `false`。HIR 当前另有检查，因此不能据此认定错误程序会进入后端；问题在于公开错误判定接口不完整，调用方容易误用。

建议：为整个分析结果提供完整的诊断查询入口，统一模块关联、遍历、排序与错误判定。阶段分类可以保留，但消费方不应需要了解内部存储拆分。修改时同步核对 CLI、LSP、HIR 的所有消费路径。

## 历史发现 2. LSP 把最近编辑文件当作工程入口

**优先级：高。状态：源码可直接确认，具体交互尚未运行复现。**

依据：

- `crates/novella-lsp/src/server/mod.rs`：`Server.analysis` 只有一个 `Option<SemaResult>`。
- `crates/novella-lsp/src/server/sync.rs`：`publish` 以当前文档路径调用 `analyze_with`，随后覆盖 `self.analysis`，仅推送当前文档诊断。
- `crates/novella-sema/src/check/decl.rs`：编号为零的入口模块执行 `main` 入口检查。

影响：

- 分别打开或编辑两个独立入口，后一次分析会替换前一次入口的语义结果；前一个入口可能不在新结果的导入闭包内。
- 编辑辅助模块也会触发程序入口检查，工程入口与普通模块的语义没有分开。
- 修改被导入模块后，依赖文件的诊断没有统一重新发布，客户端可能保留旧诊断。

建议：明确工程、入口与模块三者的关系。按工程维护分析快照与模块归属，根据依赖关系更新受影响模块的诊断；普通模块检查不应隐式等同于可执行程序入口检查。具体入口选择规则需要结合工程配置设计确定。

## 历史发现 3. 文档解析与语义分析是两套缺少版本关联的快照

**优先级：高。状态：重复解析和生命周期差异可确认，错位场景尚未运行复现。**

依据：

- `crates/novella-lsp/src/document.rs`：`SourceDoc::new` 运行词法与语法分析，并建立大纲和位置索引。
- `crates/novella-sema/src/module/loader.rs`：语义分析加载相同文档时再次运行词法与语法分析。
- `crates/novella-lsp/src/server/sync.rs`：`did_close` 移除文档缓存并清空诊断，但没有重建或失效化 `analysis`；诊断发布版本传入 `None`。
- `crates/novella-lsp/src/server/query.rs`：`module_view` 使用当前打开文档或重新读取磁盘文本，`location_of` 用该文本换算分析结果中的 span。

影响：大纲等功能和语义导航分别依赖不同解析产物；语义结果没有明确绑定文档版本。关闭含未保存修改的文档后，旧语义 span 可能被应用到磁盘文本上。重复解析也增加编辑开销，但本次未量化其成本。

建议：共享带版本的不可变源码与解析快照，语义结果显式关联其输入快照。位置换算使用分析时的源码；文档关闭、修改或磁盘更新时，明确处理快照失效与结果版本。

## 历史发现 4. 声明类型尚未完成时已经开始检查消费者

**优先级：高。状态：阶段顺序可确认，具体漏检场景待验证。**

依据：

- `crates/novella-sema/src/check/type_defs.rs`：`build_field` 将无标注、有初值字段注册为 `Ty::Unknown`，等待后续推导。
- `crates/novella-sema/src/check/decl.rs`：`check_module` 按 AST 顺序检查函数体和类型成员。
- `crates/novella-sema/src/typecheck/mod.rs`：`check_field_initializer` 在检查字段初值时回填字段类型。
- `crates/novella-sema/src/ty.rs`：`is_opaque` 将 `Unknown` 等类型用于抑制后续检查。

影响：函数或方法读取字段时，字段可能仍未完成类型推导。若此时按不透明类型放行，后续字段回填不会自动重检此前消费者，存在结果依赖声明或模块顺序的风险。不能仅凭静态阅读断言所有前向引用都会漏检。

建议：将声明类型完成与函数体检查分成明确阶段。先解决字段、顶层绑定等声明类型，再检查依赖它们的主体；推导存在依赖时显式记录并处理循环。不要以简单调整遍历顺序代替依赖处理。

## 历史发现 5. 语义产物没有提供稳定的声明查询契约

**优先级：中高。状态：源码可直接确认。**

依据：

- `crates/novella-hir/src/build/call.rs`：`append_default_arguments` 扫描模块 AST，通过重新拼装函数键定位默认参数声明。
- `crates/novella-hir/src/build/aggregate.rs`：`build_struct_init` 按模块、类型名搜索原始 struct 声明，再获取字段初值。
- `crates/novella-hir/src/build/module.rs`：lambda 归属依赖字符串键前缀识别。

影响：已解析的定义身份没有完整传递到下游，HIR 需要知道 AST 组织方式和字符串键编码规则。新增声明形态、修改键格式或扩展默认值机制时，需要同步修改多个搜索路径。

建议：建立稳定的定义身份与声明索引，显式提供调用目标、默认表达式及其定义环境。HIR 继续消费 AST 是合理的，但应通过已解析的声明引用访问，而不是重新扫描并匹配字符串。定义身份的作用域应明确为单次分析或跨快照，避免承诺未经设计的持久稳定性。

## 历史发现 6. 引用表兼任定义索引

**优先级：中高。状态：源码可直接确认。**

依据：

- `crates/novella-sema/src/lib.rs`：`BindingTable` 提供引用到 `Definition` 的映射，没有独立的完整定义表。
- `crates/novella-lsp/src/nav.rs`：`definition_at` 在声明处查询时扫描 `bindings.references.values()`。
- 绑定表契约明确说明实例成员等位置尚未完整进入表。

影响：没有引用的声明缺少独立查询来源；导航能力取决于声明是否恰好作为引用目标出现在表中。字段、方法等定义信息不完整，也限制了统一的引用查询和重命名。

建议：维护独立定义表与引用关系。定义表保存种类、属主、精确名字位置与类型；引用只保存目标定义身份。定义表应覆盖声明本身，不依赖是否发生引用。与上一项声明索引统一设计，避免再建立一套并行身份系统。

## 历史发现 7. 容错类型视图同时承担编译输入契约

**优先级：中。状态：有损转换可直接确认。**

依据：

- `crates/novella-sema/src/ty.rs`：`Ty::view` 将 `Unknown`、`External`、`Error` 都投影为 `TypeView::Unknown`。
- `crates/novella-sema/src/lib.rs`：表达式、注解与声明类型表对外均提供该投影。
- `crates/novella-hir/src/ty.rs`：HIR 从 `TypeView` 转换类型，无法降级时返回错误。

影响：HIR 能够拒绝不完整类型，但无法从投影中区分外部符号、已诊断错误和未完成推导，错误解释与阶段完整性验证因此受限。这不是要求所有外部类型都能生成代码，而是要求保留拒绝的具体原因。

建议：区分容错查询结果与已经完成定型的降级输入，保留类型状态和来源。可通过显式校验后产出降级视图实现，无需公开 sema 内部可变状态，也不必机械复制整套类型模型。

## 历史发现 8. 分析计算与 LSP 消息处理同步耦合

**优先级：中。状态：执行路径可直接确认，延迟成本未测量。**

依据：

- `crates/novella-lsp/src/server/run.rs`：主循环串行读取并处理报文。
- `crates/novella-lsp/src/server/sync.rs`：文档变更在处理链内同步运行整个导入闭包的分析。
- `crates/novella-lsp/src/server/dispatch.rs`：当前没有分析任务取消与过期结果管理路径。

影响：分析期间不能及时处理后续编辑或导航请求；连续编辑会依次触发分析，缺少合并变更与淘汰旧任务的机制。stdio 传输串行不意味着分析计算必须阻塞收消息。

建议：先分离消息接收与分析任务，明确任务输入版本、取消策略和结果提交条件；合并连续变更，丢弃过期结果。是否进一步引入模块级增量分析，应结合测量决定。

## 建议实施顺序

1. 统一完整诊断查询，消除 `has_errors` 契约漏洞。
2. 定义 LSP 工程入口、快照与版本生命周期，同时解决多入口结果覆盖和诊断失效问题。
3. 完成声明类型推导阶段，验证前向引用与跨模块依赖不受遍历顺序影响。
4. 统一定义身份、定义表与声明索引，改造导航和 HIR 声明查询。
5. 明确定型输入契约，再拆分分析调度，按实际成本决定增量化范围。

历史建议涉及跨模块结构调整；用户随后已明确授权一次性修复，当前实现状态以上表为准。

## 验证边界

实施时应优先验证：纯语法错误的全局错误判定、两个独立入口交替编辑、辅助模块入口语义、依赖模块诊断刷新、关闭未保存文档后的导航、声明顺序互换、无引用声明定位，以及默认参数跨模块展开。

上述历史审查当时未修改实现、未运行验证。本次工作区修复的证据见前述复查表和独立备忘，不用历史静态判断代替运行结果。
