//! 源码位置换算：字节偏移 ⇄ (行, UTF-16 列)，以及 LSP `Range` 的构造。
//!
//! novella 内部（`Span` / `Token` / `Diagnostic`）一律使用**字节偏移**，
//! 而 LSP 规定位置单位为**行（0-based）+ 行内 UTF-16 码元列（0-based）**。
//! 本项目只在 [`SemanticIndex`] 这一处维护换算表：
//! - [`crate::semantic`] 用它输出语义 token；
//! - [`crate::features`] 用它实现悬停 / 补全 / 签名帮助；
//! - [`crate::server`] 用它把诊断换算成 LSP 诊断区间。
//!
//! 分行语义由 [`novella_core::line_index::LineIndex`] 唯一裁定（只按 `\n` 分行，
//! `\r\n` 的 `\r` 归上一行）——本模块只在其上附加「行内 UTF-16 列」换算，
//! 不再维护第二份行表。

use lsp_types::{Position as LspPosition, Range};
use novella_core::Span;
use novella_core::line_index::LineIndex;

/// 行号（0-based）。
pub type Line = u32;
/// 行内 UTF-16 码元列（0-based）。
pub type Column = u32;
/// 内部使用的 `(行, 列)` 二元组，避免与 `lsp_types::Position` 混淆。
pub type Position = (Line, Column);

/// 单个源码文件的换算索引。
///
/// 构造后只读，随文档缓存一起保存（见 [`crate::server::SourceDoc`]），
/// 避免每个请求都重建行表。
#[derive(Debug, Clone)]
pub struct SemanticIndex {
    /// 该索引对应的整份源码：所有偏移先钳到源码范围内再换算，
    /// 越界偏移不会被误判成「下一行的行首」
    source: String,
    /// 行起点表（core 唯一实现）
    lines: LineIndex,
    /// 逐行的 UTF-16 码元前缀和：为某行构造一次后按字节偏移查列
    inner: Vec<InnerIndex>,
}

/// 单行的 UTF-16 前缀和表。
#[derive(Debug, Clone)]
struct InnerIndex {
    /// 第 `i` 个码点起点的字节偏移（升序）
    byte_offsets: Vec<usize>,
    /// 与 `byte_offsets` 对应的 UTF-16 列
    utf16_offsets: Vec<u32>,
}

impl SemanticIndex {
    /// 为整份源码建立换算表。
    pub fn new(source: &str) -> Self {
        let mut index = SemanticIndex {
            source: source.to_owned(),
            lines: LineIndex::build(source),
            inner: Vec::new(),
        };
        index.inner = (0..index.line_count())
            .map(|line| InnerIndex::new(index.line_text(source, line)))
            .collect();
        index
    }

    /// 行数（末尾换行不算新行）。
    pub fn line_count(&self) -> usize {
        self.lines.line_count()
    }

    /// 第 `line` 行的完整源码切片（不含行尾换行）。
    pub fn line_text<'a>(&self, source: &'a str, line: usize) -> &'a str {
        self.lines.line_text(source, line + 1).unwrap_or("")
    }

    /// 字节偏移 → (0-based 行, UTF-16 列)。
    ///
    /// 越界偏移被夹到最后一行行尾；落在多字节字符中间时进位到该字符之后，
    /// 列号始终是合法的 UTF-16 位置。
    pub fn position(&self, offset: usize) -> Position {
        // 先钳到源码末尾：否则 offset 略超末行时会被判成「末行之后」的列偏移，
        // 越过行尾落到下一行
        let offset = offset.min(self.source.len());
        let line = (self.lines.locate(offset).0 - 1).min(self.inner.len().saturating_sub(1));
        let line_start = self.lines.line_start(line + 1).unwrap_or(0);
        let column = self.inner[line].utf16_of(offset.saturating_sub(line_start));
        (line as u32, column)
    }

    /// (0-based 行, UTF-16 列) → 字节偏移。
    ///
    /// 行号越界时夹到最后一行；列越行尾时夹到行尾（行尾即换行符之前）。
    pub fn offset(&self, line: Line, column: Column) -> usize {
        let line = (line as usize).min(self.line_count().saturating_sub(1));
        let base = self.lines.line_start(line + 1).unwrap_or(0);
        base + self.inner[line].byte_of(column)
    }

    /// LSP `Position` → 字节偏移。
    pub fn offset_of(&self, position: LspPosition) -> usize {
        self.offset(position.line, position.character)
    }

    /// 字节偏移 → LSP `Position`。
    pub fn lsp_position(&self, offset: usize) -> LspPosition {
        let (line, character) = self.position(offset);
        LspPosition { line, character }
    }

    /// 字节区间 → LSP `Range`（起点为 `span.start`，终点为 `span.end`）。
    ///
    /// 与 [`SemanticIndex::segments`] 不同，这里**不按行拆分**：诊断 / 悬停 / 跳转
    /// 都允许跨行区间，由客户端自行渲染。
    pub fn range(&self, span: Span) -> Range {
        Range {
            start: self.lsp_position(span.start),
            end: self.lsp_position(span.end),
        }
    }

    /// 字节区间 → LSP `(行, 起列, UTF-16 长度)` 的多段切分。
    ///
    /// 跨行区间按行拆开（LSP 的单个语义 token / 诊断区间不跨行时更易被客户端着色），
    /// 空段（长度为 0）被跳过。
    pub fn segments(&self, source: &str, span: Span) -> Vec<(Line, Column, u32)> {
        let mut segments = Vec::new();
        let mut offset = span.start.min(source.len());
        let end = span.end.min(source.len());

        while offset < end {
            let line = self.line_index_of(offset);
            let line_start = self.lines.line_start(line + 1).unwrap_or(0);
            let line_end = self
                .lines
                .line_start(line + 2)
                .map_or(source.len(), |next| next - 1)
                .max(line_start);
            // 本段止于行尾（不含换行符）或区间结束，二者取小
            let segment_end = end.min(line_end);
            let text = &source[line_start..line_end];
            let segment_start = offset - line_start;
            let segment_stop = segment_end - line_start;

            let column = self.inner[line].utf16_of(segment_start);
            let length = utf16_len(&text[segment_start..segment_stop]);
            if length > 0 {
                segments.push((line as u32, column, length as u32));
            }

            // 段落在行尾、而区间仍延伸到下一行时，跳过一个换行符继续
            offset = if segment_end == line_end {
                line_end + 1
            } else {
                segment_end
            };
        }

        segments
    }

    /// 定位字节偏移所在的行号（0-based）。
    ///
    /// 行起点严格递增且首元素为 0，`LineIndex::locate` 的行号减一不会下溢。
    fn line_index_of(&self, offset: usize) -> usize {
        self.lines.locate(offset).0 - 1
    }
}

impl InnerIndex {
    /// 为一行文本建立「字节偏移 → UTF-16 列」的对照表。
    fn new(text: &str) -> Self {
        let mut byte_offsets = Vec::with_capacity(text.len() + 1);
        let mut utf16_offsets = Vec::with_capacity(text.len() + 1);
        let mut utf16 = 0u32;
        for (offset, ch) in text.char_indices() {
            byte_offsets.push(offset);
            utf16_offsets.push(utf16);
            utf16 += ch.len_utf16() as u32;
        }
        // 行尾哨兵：允许定位到行末（如空文档、光标贴行尾）
        byte_offsets.push(text.len());
        utf16_offsets.push(utf16);
        InnerIndex {
            byte_offsets,
            utf16_offsets,
        }
    }

    /// 行内字节偏移 → UTF-16 列（落在多字节字符中间时进位到字符结束）。
    ///
    /// 用「首个 `> byte` 的下标」减一得到该列；字节偏移恰好落在某码点起点时，
    /// 该码点对应的列即为结果；落在字符中间时 `partition_point` 指向下一个码点，
    /// 同样给出「字符之后」的列，与 LSP 对非法位置的容忍行为一致。
    fn utf16_of(&self, byte: usize) -> Column {
        let index = self
            .byte_offsets
            .partition_point(|&b| b <= byte)
            .saturating_sub(1);
        self.utf16_offsets[index.min(self.utf16_offsets.len() - 1)]
    }

    /// UTF-16 列 → 行内字节偏移（超过行末时夹到行尾）。
    fn byte_of(&self, column: Column) -> usize {
        // partition_point 返回「首个列 > column」的下标，减一即命中的码点，不能直接索引
        let index = self
            .utf16_offsets
            .partition_point(|&c| c <= column)
            .saturating_sub(1);
        let last = self.byte_offsets.len() - 1;
        self.byte_offsets[index.min(last)]
    }
}

/// 字符串的 UTF-16 码元长度（BMP 内字符计 1，超出 BMP 计 2）。
pub fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 中文与 emoji 的列号必须按 UTF-16 码元计。
    #[test]
    fn position_uses_utf16_units() {
        let source = "// 注释\nlet x = \"🐋\";\n";
        let index = SemanticIndex::new(source);
        // 第二行的 emoji 位于字节偏移之后，但 UTF-16 列按码元计
        let key = source.find("let").expect("存在 let");
        assert_eq!(index.position(key), (1, 0));
        let quote = source.find('"').expect("存在引号");
        assert_eq!(
            index.position(quote),
            (1, 8),
            "position(18) 实际={:?}",
            index.position(quote)
        );
        // 圆整闭环：列 → 偏移 → 列
        let offset = index.offset(1, 8);
        assert_eq!(
            offset, quote,
            "offset(1,8) 偏移实际字节={offset} 引号={quote}"
        );
    }

    /// 跨行区间应按行拆分，且换行符不计入任何一段。
    #[test]
    fn cross_line_span_is_split() {
        let source = "a\nbb\n";
        let index = SemanticIndex::new(source);
        let segments = index.segments(source, Span { start: 0, end: 4 });
        assert_eq!(segments, vec![(0, 0, 1), (1, 0, 2)]);
    }

    /// 行尾 / 超出行尾的光标位置应夹到行末而不是越界。
    ///
    /// `source.len() == 22`：第 0 行 `let a = 1;` 的 10 个字节后是换行；
    /// 最后一行 `let b = 2;` 从 11 开始，行内 10 个字节，其后是第 21 个字节的换行。
    #[test]
    fn offset_clamps_to_line_end() {
        let source = "let a = 1;\nlet b = 2;\n";
        let index = SemanticIndex::new(source);
        assert_eq!(index.offset(0, 999), 10, "第 0 行行尾夹到换行前");
        assert_eq!(index.offset(99, 0), 22, "行号越界夹到最后一行行首");
        assert_eq!(
            index.offset(1, 999),
            21,
            "最后一行行末夹到换行前（行内第 10 字节）"
        );
    }
}
