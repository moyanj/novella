//! HIR 构造错误及定位。

use novella_core::Span;
use novella_sema::ModuleId;

/// 构造期错误。
#[derive(Debug, Clone)]
pub struct BuildError {
    pub kind: BuildErrorKind,
    /// 出错模块（`None` 表示与模块无关的构造失败）
    pub module: Option<ModuleId>,
    /// 出错的源码区间（`None` 表示没有具体来源）
    pub span: Option<Span>,
}

impl BuildError {
    /// 与源码位置无关的失败。
    pub fn at(kind: BuildErrorKind) -> Self {
        BuildError {
            kind,
            module: None,
            span: None,
        }
    }

    /// 补上模块与源码位置（沿 `?` 向上传播时逐层补全，只填第一个非空值）。
    pub fn with_site(mut self, module: ModuleId, span: Span) -> Self {
        if self.module.is_none() {
            self.module = Some(module);
        }
        if self.span.is_none() {
            self.span = Some(span);
        }
        self
    }
}

impl std::fmt::Display for BuildError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HIR 降级失败：{}", self.kind)
    }
}

impl std::error::Error for BuildError {}

/// 构造期错误种类。
///
/// 与「诊断」的区别：诊断是给用户看的语义错误；这里只发生在
/// **语义已通过、但后端暂不支持该构造**（或 sema 与 HIR 的类型集合不一致）的情形。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildErrorKind {
    /// 输入分析存在错误，不能跳过坏模块生成部分程序。
    InvalidInput,
    /// 纯分析 API 的平台整数位宽与实际发射目标不一致。
    TargetWidthMismatch {
        analysis_bits: u32,
        target_bits: u32,
    },
    /// 类型被投影为 `Unknown` / `External` / `Error`
    OpaqueType,
    /// 未定型字面量 / `null` 漏到需要具体类型的位置
    UntypedLiteral { what: &'static str },
    /// 类型名无法识别（sema 与本 crate 的类型名集合不一致，属内部错误）
    UnknownTypeName { name: &'static str },
    /// 联合类型暂不支持（值表示未定）
    UnionUnsupported,
    /// 泛型形参未消解
    GenericUnsupported,
    /// 尚未降级的 AST 构造
    Unimplemented { node: &'static str },
    /// 类型注解缺失且无法从上下文补全
    MissingType { what: &'static str },
    /// 表达式在 sema 类型表中查不到（语义阶段未检查到该表达式）
    MissingExpressionType,
    /// 标量之间的隐式转换超出了本期实现
    ImplicitCast {
        from: &'static str,
        to: &'static str,
    },
}

impl std::fmt::Display for BuildErrorKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BuildErrorKind::InvalidInput => f.write_str("语义 / 语法 / 加载错误尚未解决"),
            BuildErrorKind::TargetWidthMismatch {
                analysis_bits,
                target_bits,
            } => write!(
                f,
                "静态分析指针位宽为 {analysis_bits} 位，与发射目标的 {target_bits} 位不一致；请使用匹配目标位宽的分析结果"
            ),
            BuildErrorKind::OpaqueType => f.write_str("类型未知（Unknown / External / Error）"),
            BuildErrorKind::UntypedLiteral { what } => {
                write!(f, "未定型字面量 `{what}` 需要具体目标类型")
            }
            BuildErrorKind::UnknownTypeName { name } => write!(f, "无法识别的类型名 `{name}`"),
            BuildErrorKind::UnionUnsupported => f.write_str("联合类型的值表示尚未实现"),
            BuildErrorKind::GenericUnsupported => f.write_str("泛型形参尚未单态化"),
            BuildErrorKind::Unimplemented { node } => write!(f, "暂不支持降级语法节点 `{node}`"),
            BuildErrorKind::MissingType { what } => write!(f, "缺少类型注解：{what}"),
            BuildErrorKind::MissingExpressionType => {
                f.write_str("表达式类型未登记（语义阶段未检查到该表达式）")
            }
            BuildErrorKind::ImplicitCast { from, to } => {
                write!(f, "暂不支持 `{from}` → `{to}` 的转换")
            }
        }
    }
}
