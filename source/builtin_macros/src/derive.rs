use proc_macro2::Span;
use quote::{quote, quote_spanned, ToTokens};
use syn::{parse_macro_input, spanned::Spanned, Field, Ident, Member, Visibility};
pub(crate) struct CmpSpecCrate;

impl ToTokens for CmpSpecCrate {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        if crate::cfg_verify_core() {
            tokens.extend(quote! { crate::std_specs::cmp });
        } else {
            tokens.extend(quote! { ::vstd::std_specs::cmp });
        }
    }
}

fn field_name(field: &Field, index: u32, span: Span) -> Member {
    let field_name = match &field.ident {
        Some(name) => Member::Named(name.clone()),
        None => Member::Unnamed(syn::Index { index, span }),
    };

    field_name
}

// gen_spec_fun(span, is_closed, fields);
pub(crate) fn spec_trait_expand_for_struct<Crate: ToTokens>(
    input: proc_macro::TokenStream,
    crat: Crate,
    trait_name: &str,
    gen_spec_fun: fn(Span, &dyn ToTokens, Vec<&dyn ToTokens>) -> proc_macro2::TokenStream,
) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as syn::DeriveInput);
    let span = input.span();
    let name = input.ident;
    let generics = input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let spec_trait = Ident::new(trait_name, span);
    let s = match input.data {
        syn::Data::Struct(s) => s,
        _ => panic!("SpecTrait derive macro only support struct"),
    };
    let mut fields = vec![];
    let mut closed = false;
    for (i, field) in s.fields.iter().enumerate() {
        fields.push(field_name(&field, i as u32, field.span()));
        if matches!(&field.vis, Visibility::Restricted(_) | Visibility::Inherited) {
            closed = true;
        }
    }
    let closed_or_open = if closed {
        quote_spanned! {span => closed}
    } else {
        quote_spanned! {span => open}
    };
    let spec_func_def =
        gen_spec_fun(span, &closed_or_open, fields.iter().map(|v| v as &dyn ToTokens).collect());
    let expand = quote_spanned! { span =>
        verus!{
            impl #impl_generics #crat::#spec_trait<#name #ty_generics> for #name #ty_generics #where_clause {
                #spec_func_def
            }
        }
    };
    proc_macro::TokenStream::from(expand)
}

// When usig derived(PartialEq)
// spec_partial_eq  == builtin::spec_eq
pub(crate) fn spec_partial_eq_expand(
    span: Span,
    closed_or_open: &dyn ToTokens,
    fields: Vec<&dyn ToTokens>,
) -> proc_macro2::TokenStream {
    let ret = quote_spanned! {span =>
        #closed_or_open spec fn spec_partial_eq(&self, rhs: &Self) -> bool
        {
            true #(&& self.#fields.spec_partial_eq(&rhs.#fields))*
        }
    };
    ret
}

/// It will produce a lexicographic ordering based on the top-to-bottom declaration order of the struct’s members
pub(crate) fn spec_partial_ord_expand(
    span: Span,
    closed_or_open: &dyn ToTokens,
    fields: Vec<&dyn ToTokens>,
) -> proc_macro2::TokenStream {
    quote_spanned! {span =>
        #closed_or_open spec fn spec_partial_cmp(&self, rhs: &Self) -> Option<core::cmp::Ordering>
        {
            if false {
                None
            }
            #(
            else if self.#fields.spec_partial_cmp(&rhs.#fields) != Some(core::cmp::Ordering::Equal) {
                self.#fields.spec_partial_cmp(&rhs.#fields)
            }
            )*
            else {
                Some(core::cmp::Ordering::Equal)
            }
        }
    }
}
