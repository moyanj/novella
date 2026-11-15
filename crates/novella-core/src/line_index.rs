//! 行索引：字节偏移 ⇄ (行号, 行内字节列)，全项目唯一实现。
//!
//! 编译器各阶段都用字节偏移（[`crate::Span`]）描述位置，但诊断展示、LSP 换算、
//! 编辑器定位都需要「第几行第几列」。此前 lexer、sema、LSP 各维护一份行表，
//! 容易在分行语义上漂移；本模块收编为单一事实源：
//!
//! - 分行语义：只按 `\n` 划分（`\r\n` 的 `\r` 归上一行），与 `str::lines()` 一致；
//! - 行号 **1-based**（对齐 ANTLR `getLine()`），列号 **0-based 字节偏移**
//!   （对齐 ANTLR `getCharPositionInLine()`）；
//! - LSP 需要的是 UTF-16 码元列，由 `novella-lsp` 在本类型之上附加换算，
//!   不在此处引入 LSP 语义。

/// 一次性扫描建立的行索引：字节偏移 → 行列，O(log n) 定位。
///
/// 相比「每个位置都从文件头重扫」的 O(n²) 做法，这里只在构造时扫描一次。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineIndex {
    /// 每行起始字节偏移（首行恒为 0），严格递增
    starts: Vec<usize>,
}

impl LineIndex {
    /// 扫描整份源码建立行起点表。
    pub fn build(input: &str) -> Self {
        let mut starts = vec![0];
        for (i, b) in input.bytes().enumerate() {
            if b == b'\n' {
                starts.push(i + 1);
            }
        }
        LineIndex { starts }
    }

    /// 行数（至少为 1：空文件也有一行起点）。
    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// 定位偏移所在的行列：`line` 为 1-based 行号，`column` 为行内字节偏移
    /// （0-based）。
    ///
    /// 偏移超出源码末尾时钳到末行末尾（不 panic），供 EOF 之类的边界位置使用。
    #[inline]
    pub fn locate(&self, offset: usize) -> (usize, usize) {
        // partition_point 依赖 starts 严格递增；starts 恒非空（首行 0），不会下溢
        let idx = self.starts.partition_point(|&s| s <= offset) - 1;
        (idx + 1, offset - self.starts[idx])
    }

    /// 行号（1-based）对应的行起始字节偏移；行号越界返回 `None`。
    #[inline]
    pub fn line_start(&self, line: usize) -> Option<usize> {
        line.checked_sub(1)
            .and_then(|idx| self.starts.get(idx).copied())
    }

    /// 行号（1-based）对应的源码切片（不含换行符）；越界返回 `None`。
    pub fn line_text<'a>(&self, source: &'a str, line: usize) -> Option<&'a str> {
        let start = self.line_start(line)?;
        let end = self
            .starts
            .get(line)
            .map_or(source.len(), |next| next.saturating_sub(1));
        source.get(start..end.max(start))
    }
}

#[cfg(test)]
mod tests {
    use super::LineIndex;

    #[test]
    fn 行列按换行划分且行号一基() {
        let src = "ab\ncd\n";
        let index = LineIndex::build(src);
        assert_eq!(index.line_count(), 3);
        assert_eq!(index.locate(0), (1, 0));
        assert_eq!(index.locate(2), (1, 2));
        assert_eq!(index.locate(3), (2, 0));
        assert_eq!(index.locate(5), (2, 2));
        assert_eq!(index.locate(6), (3, 0));
        // EOF 位置（源码长度）落在末行起点
        assert_eq!(index.locate(src.len()), (3, 0));
    }

    #[test]
    fn crlf_的_r_归上一行() {
        // 行 1 文本为 "ab\r"（"\r" 属上一行），行 2 从换行后的偏移 4 起
        let src = "ab\r\ncd";
        let index = LineIndex::build(src);
        assert_eq!(index.locate(3), (1, 3), "`\r` 仍在第 1 行内");
        assert_eq!(index.locate(4), (2, 0), "`\n` 之后即第 2 行起点");
        assert_eq!(index.line_text(src, 1), Some("ab\r"));
        assert_eq!(index.line_text(src, 2), Some("cd"));
    }

    #[test]
    fn 行越界与空文件() {
        let index = LineIndex::build("");
        assert_eq!(index.line_count(), 1);
        assert_eq!(index.locate(0), (1, 0));
        assert_eq!(index.line_start(0), None);
        assert_eq!(index.line_start(2), None);
        assert_eq!(index.line_text("", 2), None);
    }
}
