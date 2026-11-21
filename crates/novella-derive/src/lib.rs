//! AST 节点 derive 宏集。
//!
//! 目前提供 `#[derive(Spanned)]`：为携带 `span: novella_core::Span 字段的节点
//! 自动生成 `pub fn span(&self) -> novella_core::Span` 方法，避免手工样板。

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Data, DeriveInput, Fields};

/// 生成 struct 的 span 访问体：要求存在名为 `span` 的字段。
fn struct_span(fields: &Fields) -> syn::Result<TokenStream2> {
    match fields {
        Fields::Named(named)
            if named
                .named
                .iter()
                .any(|f| f.ident.as_ref().is_some_and(|id| id == "span")) =>
        {
            Ok(quote! { self.span })
        }
        _ => Err(syn::Error::new_spanned(
            fields,
            "派生 `Spanned` 的 struct 必须包含名为 `span` 的字段",
        )),
    }
}

/// 生成 enum 的 `span` 访问体（match 表达式）。
///
/// 变体规则：
/// - 具名字段变体且含 `span` 字段：直接取 `*span`；
/// - 单字段 newtype 变体：委托内部节点的 `span()`（内部节点需同样实现 `Spanned`）。
fn enum_span(
    variants: &syn::punctuated::Punctuated<syn::Variant, syn::token::Comma>,
) -> syn::Result<TokenStream2> {
    let mut arms = Vec::new();
    for variant in variants {
        let ident = &variant.ident;
        match &variant.fields {
            Fields::Named(named)
                if named
                    .named
                    .iter()
                    .any(|f| f.ident.as_ref().is_some_and(|id| id == "span")) =>
            {
                arms.push(quote! { Self::#ident { span, .. } => *span });
            }
            Fields::Unnamed(unnamed) if unnamed.unnamed.len() == 1 => {
                // newtype 变体：委托给内部节点的 span()（内部节点也必须实现）。
                arms.push(quote! { Self::#ident(inner) => inner.span() });
            }
            _ => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "变体需要包含名为 `span` 的字段，或为单字段 newtype 变体（内部实现 `span()`）",
                ));
            }
        }
    }
    Ok(quote! { match self { #(#arms),* } })
}

/// 派生 `span()` 方法。
///
/// 适用：所有携带 `span: novella_core::Span` 字段的 AST 节点；
/// 支持 struct 与 enum（含混合 newtype 变体），生成无参固有方法
/// `pub fn span(&self) -> ::novella_core::Span`。
#[proc_macro_derive(Spanned)]
pub fn derive_spanned(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();
    let body = match &input.data {
        Data::Struct(data) => struct_span(&data.fields),
        Data::Enum(data) => enum_span(&data.variants),
        Data::Union(_) => Err(syn::Error::new_spanned(
            &input.ident,
            "`union` 不支持派生 `Spanned`",
        )),
    };
    match body {
        Ok(body) => quote! {
            impl #impl_generics #name #ty_generics #where_clause {
                /// 返回节点覆盖的源码区间（供诊断定位）。
                pub fn span(&self) -> ::novella_core::Span {
                    #body
                }
            }
        }
        .into(),
        Err(err) => err.to_compile_error().into(),
    }
}

/// AST 递归节点专用 Clone：每层进入既有分段栈守卫，不改变字段克隆语义。
#[proc_macro_derive(GuardedClone)]
pub fn derive_guarded_clone(input: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let Data::Enum(data) = &input.data else {
        return syn::Error::new_spanned(name, "GuardedClone 仅用于 AST 递归枚举")
            .to_compile_error()
            .into();
    };
    let arms = data.variants.iter().map(|variant| {
        let ident = &variant.ident;
        match &variant.fields {
            Fields::Named(fields) => {
                let names: Vec<_> = fields.named.iter().map(|f| f.ident.as_ref().expect("具名字段")).collect();
                quote! { Self::#ident { #(#names),* } => Self::#ident { #(#names: ::core::clone::Clone::clone(#names)),* } }
            }
            Fields::Unnamed(fields) => {
                let names: Vec<_> = (0..fields.unnamed.len()).map(|i| quote::format_ident!("field_{i}")).collect();
                quote! { Self::#ident(#(#names),*) => Self::#ident(#(::core::clone::Clone::clone(#names)),*) }
            }
            Fields::Unit => quote! { Self::#ident => Self::#ident },
        }
    });
    quote! {
        impl ::core::clone::Clone for #name {
            fn clone(&self) -> Self {
                crate::drop_guard::with_stack(|| match self { #(#arms),* })
            }
        }
    }
    .into()
}
