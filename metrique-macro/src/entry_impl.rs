// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

//! This module generates the implementation of the Entry trait for non-value structs and enums.
//! This gives us more control over the generated code and improves compile-time errors.

use proc_macro2::TokenStream as Ts2;
use quote::{format_ident, quote, quote_spanned};
use syn::Ident;

use crate::{NameStyle, Prefix, RootAttributes, inflect::metric_name};

pub(crate) use enum_impl::generate_enum_entry_impl;
pub(crate) use struct_impl::generate_struct_entry_impl;

fn make_ns(ns: NameStyle, span: proc_macro2::Span) -> Ts2 {
    match ns {
        NameStyle::PascalCase => quote_spanned! {span=> NS::PascalCase },
        NameStyle::SnakeCase => quote_spanned! {span=> NS::SnakeCase },
        NameStyle::KebabCase => quote_spanned! {span=> NS::KebabCase },
        NameStyle::Preserve => quote_spanned! {span=> NS },
    }
}

// Shared helpers for both struct and enum implementations

/// Generate a ConstStr struct with the given identifier and value.
/// Used to create compile-time constant strings for metric names and prefixes.
fn const_str(ident: &syn::Ident, value: &str) -> Ts2 {
    quote_spanned! {ident.span()=>
        #[allow(non_camel_case_types)]
        struct #ident;
        impl ::metrique::concat::ConstStr for #ident {
            const VAL: &'static str = #value;
        }
    }
}

/// Generate 4 ConstStr structs (one per naming style) and build an Inflect namespace type.
/// The `name_fn` callback computes the string value for each style.
/// Returns (extra_code, inflected_type).
fn make_inflect(
    ns: &Ts2,
    inflect_method: syn::Ident,
    base_name: &str,
    span: proc_macro2::Span,
    mut name_fn: impl FnMut(NameStyle) -> String,
) -> (Ts2, Ts2) {
    let name_ident = format_ident!(
        "{}{}",
        base_name,
        NameStyle::Preserve.to_word(),
        span = span
    );
    let name_kebab = format_ident!(
        "{}{}",
        base_name,
        NameStyle::KebabCase.to_word(),
        span = span
    );
    let name_pascal = format_ident!(
        "{}{}",
        base_name,
        NameStyle::PascalCase.to_word(),
        span = span
    );
    let name_snake = format_ident!(
        "{}{}",
        base_name,
        NameStyle::SnakeCase.to_word(),
        span = span
    );

    let extra_preserve = const_str(&name_ident, &name_fn(NameStyle::Preserve));
    let extra_kebab = const_str(&name_kebab, &name_fn(NameStyle::KebabCase));
    let extra_pascal = const_str(&name_pascal, &name_fn(NameStyle::PascalCase));
    let extra_snake = const_str(&name_snake, &name_fn(NameStyle::SnakeCase));

    let extra = quote!(
        #extra_preserve
        #extra_kebab
        #extra_pascal
        #extra_snake
    );

    let inflected_type = quote!(
        <#ns as ::metrique::NameStyle>::#inflect_method<#name_ident, #name_pascal, #name_snake, #name_kebab>
    );

    (extra, inflected_type)
}

/// Generate an inflectable prefix that adapts to the namespace style.
/// Creates 4 ConstStr structs (preserve, pascal, snake, kebab) and returns
/// a namespace type that selects the appropriate variant via InflectAffix.
/// Returns (extra_code, namespace_with_prefix).
fn make_inflect_prefix(
    ns: &Ts2,
    prefix: &str,
    base_name: &str,
    span: proc_macro2::Span,
) -> (Ts2, Ts2) {
    let (extra, inflected) = make_inflect(
        ns,
        format_ident!("InflectAffix", span = span),
        &format!("{}Prefix", base_name),
        span,
        |style| style.apply_prefix(prefix),
    );

    let ns_with_prefix = quote!(
        <#ns as ::metrique::NameStyle>::AppendPrefix<#inflected>
    );

    (extra, ns_with_prefix)
}

/// Generate an exact (non-inflectable) prefix that never changes.
/// Creates 1 ConstStr struct and returns a namespace type with the prefix applied.
/// Returns (extra_code, namespace_with_prefix).
fn make_exact_prefix(
    ns: &Ts2,
    exact_prefix: &str,
    base_name: &str,
    span: proc_macro2::Span,
) -> (Ts2, Ts2) {
    let prefix_ident = format_ident!("{}Preserve", base_name, span = span);
    let extra = const_str(&prefix_ident, exact_prefix);
    let ns_with_prefix = quote!(
        <#ns as ::metrique::NameStyle>::AppendPrefix<#prefix_ident>
    );
    (extra, ns_with_prefix)
}

mod struct_impl {
    use super::*;
    use crate::{MetricsField, MetricsFieldKind, value_impl::format_value};

    pub(crate) fn generate_struct_entry_impl(
        entry_name: &Ident,
        fields: &[MetricsField],
        root_attrs: &RootAttributes,
    ) -> Ts2 {
        let writes = generate_write_statements(fields, root_attrs);
        let sample_groups = generate_sample_group_statements(fields, root_attrs);
        // we generate one entry impl for each namestyle. This will then allow the parent to
        // transitively set the namestyle
        quote! {
            const _: () = {
                #[expect(deprecated)]
                impl<NS: ::metrique::NameStyle> ::metrique::InflectableEntry<NS> for #entry_name {
                    fn write<'a>(&'a self, writer: &mut impl ::metrique::writer::EntryWriter<'a>) {
                        #(#writes)*
                    }

                    fn sample_group(&self) -> impl ::std::iter::Iterator<Item = (::std::borrow::Cow<'static, str>, ::std::borrow::Cow<'static, str>)> {
                        #sample_groups
                    }
                }
            };
        }
    }

    fn generate_write_statements(fields: &[MetricsField], root_attrs: &RootAttributes) -> Vec<Ts2> {
        let mut writes = Vec::new();

        for field_ident in root_attrs.configuration_field_names() {
            writes.push(quote! {
                ::metrique::writer::Entry::write(&self.#field_ident, writer);
            });
        }

        for field in fields {
            let field_ident = &field.ident;
            let field_span = field.span;
            let ns = make_ns(root_attrs.rename_all, field_span);

            match &field.attrs.kind {
                MetricsFieldKind::Timestamp(span) => {
                    writes.push(quote_spanned! {*span=>
                        #[allow(clippy::useless_conversion)]
                        {
                            ::metrique::writer::EntryWriter::timestamp(writer, (self.#field_ident).into());
                        }
                    });
                }
                MetricsFieldKind::FlattenEntry(span) => {
                    writes.push(quote_spanned! {*span=>
                        ::metrique::writer::Entry::write(&self.#field_ident, writer);
                    });
                }
                MetricsFieldKind::Flatten { span, prefix } => {
                    let (extra, ns) = match prefix {
                        None => (quote!(), ns),
                        Some(Prefix::Inflectable { prefix }) => {
                            make_inflect_prefix(&ns, prefix, &field.ident.to_string(), field_span)
                        }
                        Some(Prefix::Exact(exact_prefix)) => make_exact_prefix(
                            &ns,
                            exact_prefix,
                            &field.ident.to_string(),
                            field_span,
                        ),
                    };
                    writes.push(quote_spanned! {*span=>
                        #extra
                        ::metrique::InflectableEntry::<#ns>::write(&self.#field_ident, writer);
                    });
                }
                MetricsFieldKind::Ignore(_) => {
                    continue;
                }
                MetricsFieldKind::Field { format, .. } => {
                    let (extra, name) = make_inflect_metric_name(root_attrs, field);
                    let value = format_value(format, field_span, quote! { &self.#field_ident });
                    writes.push(quote_spanned! {field_span=>
                        ::metrique::writer::EntryWriter::value(writer,
                            {
                                #extra
                                ::metrique::concat::const_str_value::<#name>()
                            }
                            , #value);
                    });
                }
            }
        }

        writes
    }

    fn make_inflect_metric_name(root_attrs: &RootAttributes, field: &MetricsField) -> (Ts2, Ts2) {
        make_inflect(
            &make_ns(root_attrs.rename_all, field.span),
            format_ident!("Inflect", span = field.span),
            &field.ident.to_string(),
            field.span,
            |style| metric_name(root_attrs, style, field),
        )
    }

    fn generate_sample_group_statements(
        fields: &[MetricsField],
        root_attrs: &RootAttributes,
    ) -> Ts2 {
        let mut sample_group_fields = Vec::new();

        for field in fields {
            if let MetricsFieldKind::Ignore(_) = field.attrs.kind {
                continue;
            }

            let field_ident = &field.ident;

            match &field.attrs.kind {
                MetricsFieldKind::Flatten { span, prefix: _ } => {
                    let ns = make_ns(root_attrs.rename_all, field.span);
                    sample_group_fields.push(quote_spanned! {*span=>
                        ::metrique::InflectableEntry::<#ns>::sample_group(&self.#field_ident)
                    });
                }
                MetricsFieldKind::FlattenEntry(span) => {
                    sample_group_fields.push(quote_spanned! {*span=>
                        ::metrique::writer::Entry::sample_group(&self.#field_ident)
                    });
                }
                MetricsFieldKind::Field {
                    sample_group: Some(span),
                    ..
                } => {
                    let (extra, name) = make_inflect_metric_name(root_attrs, field);
                    sample_group_fields.push(quote_spanned! {*span=>
                        {
                            #extra
                            ::std::iter::once((
                                ::metrique::concat::const_str_value::<#name>(),
                                ::metrique::writer::core::SampleGroup::as_sample_group(&self.#field_ident)
                            ))
                        }
                    });
                }
                // these don't have sample groups
                MetricsFieldKind::Field {
                    sample_group: None, ..
                }
                | MetricsFieldKind::Ignore { .. }
                | MetricsFieldKind::Timestamp { .. } => {}
            }
        }

        // If we have sample group fields, chain them together
        if !sample_group_fields.is_empty() {
            // Create a binary tree of chain calls to avoid deep nesting
            make_binary_tree_chain(sample_group_fields)
        } else {
            // Return empty iterator if no sample groups
            quote! { ::std::iter::empty() }
        }
    }

    /// Return an iterator that chains the iterators in `iterators`.
    ///
    /// This calls `chain` in a binary tree fashion to avoid problems with the recursion limit,
    /// e.g. `I1.chain(I2).chain(I3.chain(I4))`
    fn make_binary_tree_chain(iterators: Vec<Ts2>) -> Ts2 {
        if iterators.is_empty() {
            return quote! { ::std::iter::empty() };
        }

        if iterators.len() == 1 {
            return iterators[0].clone();
        }

        // Split the iterators in half and recursively build the tree
        let mid = iterators.len() / 2;
        let left = make_binary_tree_chain(iterators[..mid].to_vec());
        let right = make_binary_tree_chain(iterators[mid..].to_vec());

        quote! { #left.chain(#right) }
    }
}

pub(crate) mod enum_impl {
    use super::*;
    use crate::{
        MetricsFieldKind,
        enums::{MetricsVariant, VariantData},
    };

    pub(crate) fn generate_enum_entry_impl(
        entry_name: &Ident,
        variants: &[MetricsVariant],
        root_attrs: &RootAttributes,
    ) -> Ts2 {
        let write_arms = variants.iter().map(|variant| {
            let variant_ident = &variant.ident;

            match &variant.data {
                None => {
                    // Defensive guard: unit variants rejected earlier during parsing
                    quote::quote_spanned!(variant.ident.span()=>
                        #entry_name::#variant_ident => {
                            compile_error!("unit variants are not allowed in entry enums; use #[metrics(value(string))] for unit-only enums");
                        }
                    )
                }
                Some(VariantData::Tuple { kind, .. }) => {
                    match kind {
                        MetricsFieldKind::Flatten { span, prefix } => {
                            // Start with enum-level rename_all
                            let base_ns = make_ns(root_attrs.rename_all, *span);

                            // Apply enum-level prefix if present
                            let (enum_prefix_extra, ns_with_enum_prefix) = match &root_attrs.prefix {
                                None => (quote!(), base_ns.clone()),
                                Some(crate::Prefix::Inflectable { prefix: enum_prefix }) => {
                                    make_inflect_prefix(&base_ns, enum_prefix, &variant_ident.to_string(), variant.ident.span())
                                }
                                Some(crate::Prefix::Exact(exact_prefix)) => {
                                    make_exact_prefix(&base_ns, exact_prefix, &variant_ident.to_string(), variant.ident.span())
                                }
                            };

                            // Apply field-level prefix on top of enum-level prefix
                            let (field_prefix_extra, final_ns) = match prefix {
                                None => (quote!(), ns_with_enum_prefix),
                                Some(crate::Prefix::Inflectable { prefix: field_prefix }) => {
                                    make_inflect_prefix(&ns_with_enum_prefix, field_prefix, &variant_ident.to_string(), variant.ident.span())
                                }
                                Some(crate::Prefix::Exact(exact_prefix)) => {
                                    make_exact_prefix(&ns_with_enum_prefix, exact_prefix, &variant_ident.to_string(), variant.ident.span())
                                }
                            };

                            quote::quote_spanned!(*span=>
                                #entry_name::#variant_ident(v) => {
                                    #enum_prefix_extra
                                    #field_prefix_extra
                                    ::metrique::InflectableEntry::<#final_ns>::write(v, writer);
                                }
                            )
                        }
                        MetricsFieldKind::FlattenEntry(span) => {
                            quote::quote_spanned!(*span=>
                                #entry_name::#variant_ident(v) => {
                                    ::metrique::writer::Entry::write(v, writer);
                                }
                            )
                        }
                        _ => {
                            // Defensive guard: invalid tuple field attributes rejected earlier during parsing
                            quote::quote_spanned!(variant.ident.span()=>
                                #entry_name::#variant_ident(_) => {
                                    compile_error!("tuple variant fields must use #[metrics(flatten)] or #[metrics(flatten_entry)]");
                                }
                            )
                        }
                    }
                }
                Some(VariantData::Struct(fields)) => {
                    // Generate write statements for each field
                    let field_writes = fields.iter().map(|field| {
                        let field_ident = &field.ident;
                        let field_span = field.span;
                        let ns = make_ns(root_attrs.rename_all, field_span);

                        match &field.attrs.kind {
                            MetricsFieldKind::Timestamp(span) => {
                                quote::quote_spanned!(*span=>
                                    #[allow(clippy::useless_conversion)]
                                    {
                                        ::metrique::writer::EntryWriter::timestamp(writer, (*#field_ident).into());
                                    }
                                )
                            }
                            MetricsFieldKind::FlattenEntry(span) => {
                                quote::quote_spanned!(*span=>
                                    ::metrique::writer::Entry::write(#field_ident, writer);
                                )
                            }
                            MetricsFieldKind::Flatten { span, prefix } => {
                                let (extra, ns) = match prefix {
                                    None => (quote!(), ns),
                                    Some(crate::Prefix::Inflectable { prefix }) => {
                                        make_inflect_prefix(&ns, prefix, &field.ident.to_string(), field_span)
                                    }
                                    Some(crate::Prefix::Exact(exact_prefix)) => {
                                        make_exact_prefix(&ns, exact_prefix, &field.ident.to_string(), field_span)
                                    }
                                };
                                quote::quote_spanned!(*span=>
                                    #extra
                                    ::metrique::InflectableEntry::<#ns>::write(#field_ident, writer);
                                )
                            }
                            MetricsFieldKind::Ignore(_) => {
                                quote!()
                            }
                            MetricsFieldKind::Field { format, .. } => {
                                let (extra, name) = make_inflect(
                                    &ns,
                                    format_ident!("Inflect", span = field_span),
                                    &field.ident.to_string(),
                                    field_span,
                                    |style| crate::inflect::metric_name(root_attrs, style, field),
                                );
                                let value = crate::value_impl::format_value(format, field_span, quote! { #field_ident });
                                quote::quote_spanned!(field_span=>
                                    ::metrique::writer::EntryWriter::value(writer,
                                        {
                                            #extra
                                            ::metrique::concat::const_str_value::<#name>()
                                        }
                                        , #value);
                                )
                            }
                        }
                    });

                    let field_names: Vec<_> = fields.iter().map(|f| &f.ident).collect();
                    quote::quote_spanned!(variant.ident.span()=>
                        #entry_name::#variant_ident { #(#field_names),* } => {
                            #(#field_writes)*
                        }
                    )
                }
            }
        });

        quote! {
            const _: () = {
                #[expect(deprecated)]
                impl<NS: ::metrique::NameStyle> ::metrique::InflectableEntry<NS> for #entry_name {
                    fn write<'a>(&'a self, writer: &mut impl ::metrique::writer::EntryWriter<'a>) {
                        #[allow(deprecated)]
                        match self {
                            #(#write_arms)*
                        }
                    }

                    fn sample_group(&self) -> impl ::std::iter::Iterator<Item = (::std::borrow::Cow<'static, str>, ::std::borrow::Cow<'static, str>)> {
                        ::std::iter::empty()
                    }
                }
            };
        }
    }
}
