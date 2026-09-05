use serde::{Deserialize, Serialize};

use novella_core::Spanned;

use crate::{
    stmt::{Block, Statement},
    types::{FunctionType, Param, TypeExpr, TypeParameter},
};

/// 程序根节点：顶层 item 列表（携带源码位置）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Program {
    pub items: Vec<Spanned<Item>>,
}

/// 对象可见性
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Scope {
    Pubilc,
    Private,
}

/// 导入对象
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportSpecifier {
    /// 本地符号名，None 表示与 imported 同名
    pub local: Option<String>,
    /// 实际符号名
    pub imported: String,
}

/// 导入的具体形式
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ImportKind {
    /// 具名导入：from "mod" import a, b as c
    Named { specifiers: Vec<ImportSpecifier> },
    /// 命名空间导入：import "mod"  将整个模块绑定到 infered_name
    Namespace {
        /// 本地绑定的符号名
        local_alias: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportItem {
    /// 原始路径字面量
    pub raw_path: String,
    /// 前缀（如 "std"）
    pub prefix: Option<String>,
    /// 逻辑路径（去除协议）
    pub path: String,
    /// 导入变体
    pub kind: ImportKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeAliasItem {
    /// 别名名称
    pub name: String,
    /// 泛型参数列表（`type A<T> = ...` 中的 `<T>`）
    pub type_parameters: Vec<TypeParameter>,
    /// 被别名的类型表达式（`=` 右侧）
    pub alias: TypeExpr,
}

// TODO: 实现接口的AST节点
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InterfaceItem {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClassMember {
    Field(Param),
    Method(FunctionItem),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ClassItem {
    pub name: String,
    pub type_parameters: Vec<TypeParameter>,
    pub members: Vec<ClassMember>,
    pub parent: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FunctionItem {
    pub name: String,
    pub r#type: FunctionType,
    pub block: Option<Block>,
    pub scope: Scope,
    pub is_extern: bool,
}

/// 顶层 item（对应文法 `statement | importStatement | typeAliasStatement | classDeclaration | functionDeclaration`）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Item {
    Import(ImportItem),
    TypeAlias(TypeAliasItem),
    Interface(InterfaceItem),
    Class(ClassItem),
    Function(FunctionItem),
    Statement(Statement),
}
