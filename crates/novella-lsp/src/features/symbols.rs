//! `textDocument/documentSymbol`：把符号表还原成嵌套树。

use crate::document::SourceDoc;
use crate::outline::{Outline, OutlineKind};

/// `textDocument/documentSymbol`：把 [`Outline`] 的扁平符号表还原成树。
///
/// LSP 允许返回嵌套（`DocumentSymbol` 树）或扁平（`SymbolInformation`）两种形态；
/// 嵌套形态能让编辑器正确显示「类 → 方法」层级，因此优先返回树。
pub fn document_symbols(doc: &SourceDoc, outline: &Outline) -> Vec<lsp_types::DocumentSymbol> {
    // 先建所有节点的空壳，再按 parent 下标回填孩子
    let mut nodes: Vec<lsp_types::DocumentSymbol> = outline
        .symbols
        .iter()
        .map(|symbol| lsp_types::DocumentSymbol {
            name: symbol.name.clone(),
            detail: symbol.detail.clone(),
            kind: symbol_kind(symbol.kind),
            tags: None,
            #[allow(deprecated)] // lsp-types 0.97 已弃用该字段（改用 tags），但结构体仍需填充
            deprecated: None,
            range: doc.index.range(symbol.span),
            selection_range: doc.index.range(symbol.name_span),
            children: None,
        })
        .collect();

    let mut children: Vec<Vec<usize>> = vec![Vec::new(); nodes.len()];
    let mut roots = Vec::new();
    for (index, symbol) in outline.symbols.iter().enumerate() {
        match symbol.parent {
            Some(parent) if parent < nodes.len() => children[parent].push(index),
            // 父下标越界属于内部错误，按顶层处理（不静默丢弃符号）
            _ => roots.push(index),
        }
    }
    // 逆序回填：孩子下标恒大于父下标，回填父节点时孩子已组装完毕
    for index in (0..nodes.len()).rev() {
        if children[index].is_empty() {
            continue;
        }
        let collected = children[index]
            .iter()
            .map(|&kid| nodes[kid].clone())
            .collect::<Vec<_>>();
        nodes[index].children = Some(collected);
    }

    roots
        .into_iter()
        .map(|index| nodes[index].clone())
        .collect()
}

fn symbol_kind(kind: OutlineKind) -> lsp_types::SymbolKind {
    match kind {
        OutlineKind::Function => lsp_types::SymbolKind::FUNCTION,
        OutlineKind::Method => lsp_types::SymbolKind::METHOD,
        OutlineKind::Class => lsp_types::SymbolKind::CLASS,
        OutlineKind::Struct => lsp_types::SymbolKind::STRUCT,
        OutlineKind::Trait => lsp_types::SymbolKind::INTERFACE,
        OutlineKind::TypeAlias => lsp_types::SymbolKind::TYPE_PARAMETER,
        OutlineKind::TypeParameter => lsp_types::SymbolKind::TYPE_PARAMETER,
        OutlineKind::Parameter => lsp_types::SymbolKind::VARIABLE,
        OutlineKind::Field => lsp_types::SymbolKind::FIELD,
        OutlineKind::Variable => lsp_types::SymbolKind::VARIABLE,
        OutlineKind::Import => lsp_types::SymbolKind::MODULE,
    }
}
