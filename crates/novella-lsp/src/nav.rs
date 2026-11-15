//! 跨模块导航：定义跳转、引用查找、重命名、类型层级。
//!
//! 数据全部来自一次 [`SemaResult`]（引用绑定表 + 类型摘要），
//! 本模块只做「光标偏移 → 定义/引用集合」的纯查询；位置换算与
//! URI 拼装由 server 层完成。
//!
//! 光标解析的共同策略：
//! - 先查引用表——光标落在某个引用 span 内时取该引用的定义；
//! - 否则看光标是否落在某个**定义的声明 span** 内（此时定义即自身）；
//! - 引用 span 可能嵌套（如 `new Base()` 的名字 span 含于整个表达式
//!   span），取最小包含区间以保证光标定位精确。

use std::collections::HashSet;

use novella_core::Span;
use novella_sema::{Definition, ModuleId, SemaResult, TypeSummary};

/// 光标处的目标定义（B1/B2 共用的第一步）。
pub fn definition_at(result: &SemaResult, module: ModuleId, offset: usize) -> Option<Definition> {
    // 1) 光标在引用内：取最小包含区间对应的定义
    let by_reference = result
        .bindings
        .references
        .iter()
        .filter(|((m, span), _)| *m == module && span.start <= offset && offset < span.end)
        .min_by_key(|((_, span), _)| span.end - span.start)
        .map(|(_, definition)| definition.clone());
    if by_reference.is_some() {
        return by_reference;
    }
    // 2) 光标在声明上：定义即自身
    result
        .bindings
        .references
        .values()
        .filter(|d| {
            d.module == module
                && d.span.start < d.span.end // 零长定义（占位节点）不参与命中
                && d.span.start <= offset
                && offset < d.span.end
        })
        .min_by_key(|d| d.span.end - d.span.start)
        .cloned()
}

/// 目标定义的全部引用位置（B2）。
///
/// `include_declaration` 时首条为声明本身（LSP `references` 的约定）；
/// 结果按 (模块, span) 排序，保证输出稳定。
pub fn references_of(
    result: &SemaResult,
    target: &Definition,
    include_declaration: bool,
) -> Vec<(ModuleId, Span)> {
    let mut locations: Vec<(ModuleId, Span)> = result
        .bindings
        .references
        .iter()
        .filter(|(_, definition)| *definition == target)
        .map(|((module, span), _)| (*module, *span))
        .collect();
    locations.sort_by_key(|(module, span)| (*module, span.start, span.end));
    if include_declaration {
        let mut all = vec![(target.module, target.span)];
        all.extend(locations);
        return all;
    }
    locations
}

/// 重命名集合：声明 + 全部引用（B2 rename）。
///
/// 与 [`references_of`] 的差别仅在于声明总是包含（rename 必须改声明处）。
pub fn rename_edits(result: &SemaResult, target: &Definition) -> Vec<(ModuleId, Span)> {
    references_of(result, target, true)
}

/// 光标处的类型摘要（B3 prepare）。
#[allow(
    dead_code,
    reason = "供尚未实现的 LSP 能力（prepareTypeHierarchy / typeHierarchy）调用"
)]
pub fn type_at(result: &SemaResult, module: ModuleId, offset: usize) -> Option<&TypeSummary> {
    result
        .types
        .summaries
        .iter()
        .filter(|t| t.module == module && t.span.start <= offset && offset < t.span.end)
        .min_by_key(|t| t.span.end - t.span.start)
}

/// 父类型链（B3 supertypes）：从直接父类一路向上，环路防御用 visited。
#[allow(
    dead_code,
    reason = "供尚未实现的 LSP 能力（prepareTypeHierarchy / typeHierarchy）调用"
)]
pub fn supertypes_of<'a>(
    result: &'a SemaResult,
    module: ModuleId,
    name: &str,
) -> Vec<&'a TypeSummary> {
    let mut chain = Vec::new();
    let mut visited = HashSet::new();
    let mut current = result
        .types
        .summaries
        .iter()
        .find(|t| t.module == module && t.name == name);
    while let Some(summary) = current {
        let Some((parent_module, parent_name)) = summary.parent.clone() else {
            break;
        };
        if !visited.insert((parent_module, parent_name.clone())) {
            break; // 继承环：sema 已报诊断，这里静默截断
        }
        let parent = result
            .types
            .summaries
            .iter()
            .find(|t| t.module == parent_module && t.name == parent_name);
        match parent {
            Some(summary) => {
                chain.push(summary);
                current = Some(summary);
            }
            None => break,
        }
    }
    chain
}

/// 直接子类型（B3 subtypes）。
#[allow(
    dead_code,
    reason = "供尚未实现的 LSP 能力（prepareTypeHierarchy / typeHierarchy）调用"
)]
pub fn subtypes_of<'a>(
    result: &'a SemaResult,
    module: ModuleId,
    name: &str,
) -> Vec<&'a TypeSummary> {
    result
        .types
        .summaries
        .iter()
        .filter(|t| t.parent.as_ref() == Some(&(module, name.to_owned())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use novella_sema::{OverlayLoader, analyze_with};
    use std::collections::HashMap;
    use std::path::PathBuf;

    /// 两模块内存工程：helper 定义函数，main 里引用；另有 Base/Shape 继承。
    fn fixture() -> (SemaResult, ModuleId, ModuleId) {
        let dir = PathBuf::from("/novella-nav-test");
        let main = dir.join("main.nov");
        let helper = dir.join("helper.nov");
        let mut overlay = HashMap::new();
        overlay.insert(
            helper,
            "pub func f() -> int {\n    return 1;\n}\n".to_owned(),
        );
        overlay.insert(
            main.clone(),
            "import \"helper\";\nclass Base {}\nclass Shape(Base) {}\nfunc main() {\n    let x = helper.f();\n}\n"
                .to_owned(),
        );
        let mut loader = OverlayLoader::new(overlay);
        let result = analyze_with(&mut loader, &main);
        let main_id = result
            .diagnostics
            .modules
            .iter()
            .position(|m| m.path.ends_with("main.nov"))
            .expect("main 模块");
        let helper_id = result
            .diagnostics
            .modules
            .iter()
            .position(|m| m.path.ends_with("helper.nov"))
            .expect("helper 模块");
        (result, main_id, helper_id)
    }

    /// 光标在引用上 → 跳到 helper 里的 `f` 定义。
    #[test]
    fn definition_of_cross_module_call() {
        let (result, main_id, helper_id) = fixture();
        // `helper.f()` 的 f 在 main.nov 文本中的偏移
        let offset = "import \"helper\";\nclass Base {}\nclass Shape(Base) {}\nfunc main() {\n    let x = helper."
            .len();
        let definition = definition_at(&result, main_id, offset).expect("应解析到定义");
        assert_eq!(definition.name, "f");
        assert_eq!(definition.module, helper_id, "定义在 helper 模块");
    }

    /// references 含声明处；rename 集合与 references(include) 一致。
    #[test]
    fn references_include_declaration() {
        let (result, main_id, _) = fixture();
        let offset = "import \"helper\";\nclass Base {}\nclass Shape(Base) {}\nfunc main() {\n    let x = helper."
            .len();
        let target = definition_at(&result, main_id, offset).expect("目标定义");
        let without = references_of(&result, &target, false);
        assert_eq!(without.len(), 1, "只有一处调用");
        let with = references_of(&result, &target, true);
        assert_eq!(with.len(), 2, "声明 + 一处调用");
        assert_eq!(rename_edits(&result, &target), with);
    }

    /// 类型层级：Shape 的父链是 [Base]，Base 的子类含 Shape。
    #[test]
    fn type_hierarchy_walks_both_ways() {
        let (result, main_id, _) = fixture();
        let source = "import \"helper\";\nclass Base {}\nclass Shape(Base) {}\nfunc main() {\n    let x = helper.f();\n}\n";
        let offset = source.find("Shape(Base)").expect("Shape 声明") + 1;
        let shape = type_at(&result, main_id, offset).expect("Shape 摘要");
        assert_eq!(shape.name, "Shape");

        let supers = supertypes_of(&result, main_id, "Shape");
        assert_eq!(supers.len(), 1);
        assert_eq!(supers[0].name, "Base");

        let subs = subtypes_of(&result, main_id, "Base");
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].name, "Shape");
        assert!(subtypes_of(&result, main_id, "Shape").is_empty());
    }
}
