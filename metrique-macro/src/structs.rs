// Copyright Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0

use darling::FromField;
use proc_macro2::{Span, TokenStream as Ts2};
use quote::{format_ident, quote, quote_spanned};
use syn::{
    Attribute, DeriveInput, FieldsNamed, FieldsUnnamed, Generics, Ident, Result, Type, Visibility,
    spanned::Spanned,
};

use crate::{
    MetricMode, MetricsFieldAttrs, MetricsFieldKind, OwnershipKind, RawMetricsFieldAttrs,
    RootAttributes, clean_attrs, entry_impl, generate_close_value_impls, generate_on_drop_wrapper,
    value_impl,
};

pub(crate) struct MetricsField {
    pub(crate) vis: Visibility,
    pub(crate) ident: Ts2,
    pub(crate) name: Option<String>,
    pub(crate) span: Span,
    pub(crate) ty: Type,
    pub(crate) external_attrs: Vec<Attribute>,
    pub(crate) attrs: MetricsFieldAttrs,
}

impl MetricsField {
    fn core_field(&self, is_named: bool) -> Ts2 {
        let MetricsField {
            ref external_attrs,
            ref ident,
            ref ty,
            ref vis,
            ..
        } = *self;
        let field = if is_named {
            quote! { #ident: #ty }
        } else {
            quote! { #ty }
        };
        quote! { #(#external_attrs)* #vis #field }
    }

    fn entry_field(&self, named: bool) -> Option<Ts2> {
        if let MetricsFieldKind::Ignore(_span) = self.attrs.kind {
            return None;
        }
        let MetricsField {
            ident, ty, span, ..
        } = self;
        let mut base_type = if self.attrs.close {
            quote_spanned! { *span=> <#ty as metrique::CloseValue>::Closed }
        } else {
            quote_spanned! { *span=>#ty }
        };
        if let Some(expr) = self.unit() {
            base_type = quote_spanned! { expr.span()=>
                <#base_type as ::metrique::unit::AttachUnit>::Output<#expr>
            }
        }
        let inner = if named {
            quote! { #ident: #base_type }
        } else {
            quote! { #base_type }
        };
        Some(quote_spanned! { *span=>
                #[deprecated(note = "these fields will become private in a future release. To introspect an entry, use `metrique::writer::test_util::test_entry`")]
                #[doc(hidden)]
                #inner
        })
    }

    fn unit(&self) -> Option<&syn::Path> {
        match &self.attrs.kind {
            MetricsFieldKind::Field { unit, .. } => unit.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn close_value(&self, ownership_kind: OwnershipKind) -> Ts2 {
        let ident = &self.ident;
        let span = self.span;
        let field_expr = match ownership_kind {
            OwnershipKind::ByValue => quote_spanned! {span=> self.#ident },
            OwnershipKind::ByRef => quote_spanned! {span=> &self.#ident },
        };
        let base = if self.attrs.close {
            quote_spanned! {span=> metrique::CloseValue::close(#field_expr) }
        } else {
            field_expr
        };

        let base = if let Some(unit) = self.unit() {
            quote_spanned! { unit.span() =>
                #base.into()
            }
        } else {
            base
        };

        quote! { #ident: #base }
    }
}

pub(crate) fn parse_struct_fields(
    fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>,
) -> Result<Vec<MetricsField>> {
    let mut parsed_fields = vec![];
    let mut errors = darling::Error::accumulator();

    for (i, field) in fields.iter().enumerate() {
        let i = syn::Index::from(i);
        let (ident, name, span) = match &field.ident {
            Some(ident) => (quote! { #ident }, Some(ident.to_string()), ident.span()),
            None => (quote! { #i }, None, field.ty.span()),
        };

        let attrs = match errors
            .handle(RawMetricsFieldAttrs::from_field(field).and_then(|attr| attr.validate()))
        {
            Some(attrs) => attrs,
            None => {
                continue;
            }
        };

        parsed_fields.push(MetricsField {
            ident,
            name,
            span,
            ty: field.ty.clone(),
            vis: field.vis.clone(),
            external_attrs: clean_attrs(&field.attrs),
            attrs,
        });
    }

    errors.finish()?;

    Ok(parsed_fields)
}

pub(crate) fn generate_metrics_for_struct(
    root_attributes: RootAttributes,
    input: &DeriveInput,
    fields: &syn::punctuated::Punctuated<syn::Field, syn::token::Comma>,
) -> Result<Ts2> {
    let struct_name = &input.ident;
    let entry_name = if root_attributes.mode == MetricMode::Value {
        format_ident!("{}Value", struct_name)
    } else {
        format_ident!("{}Entry", struct_name)
    };
    let guard_name = format_ident!("{}Guard", struct_name);
    let handle_name = format_ident!("{}Handle", struct_name);

    let parsed_fields = parse_struct_fields(fields)?;

    let base_struct = generate_base_struct(
        struct_name,
        &input.vis,
        &input.generics,
        &input.attrs,
        &parsed_fields,
    )?;
    let warnings = root_attributes.warnings();

    let entry_struct = generate_entry_struct(
        &entry_name,
        &input.generics,
        &parsed_fields,
        &root_attributes,
    )?;

    let inner_impl = match root_attributes.mode {
        MetricMode::Value => {
            value_impl::validate_value_impl_for_struct(
                &root_attributes,
                &entry_name,
                &parsed_fields,
            )?;
            value_impl::generate_value_impl_for_struct(
                &root_attributes,
                &entry_name,
                &parsed_fields,
            )?
        }
        _ => entry_impl::generate_entry_impl(&entry_name, &parsed_fields, &root_attributes),
    };

    let close_value_impl = generate_close_value_impls_for_struct(
        struct_name,
        &entry_name,
        &parsed_fields,
        &root_attributes,
    );
    let vis = &input.vis;

    let root_entry_specifics = match root_attributes.mode {
        MetricMode::RootEntry => {
            let on_drop_wrapper =
                generate_on_drop_wrapper(vis, &guard_name, struct_name, &entry_name, &handle_name);
            quote! {
                #on_drop_wrapper
            }
        }
        MetricMode::Subfield
        | MetricMode::SubfieldOwned
        | MetricMode::ValueString
        | MetricMode::Value => {
            quote! {}
        }
    };

    Ok(quote! {
        #base_struct
        #warnings
        #entry_struct
        #inner_impl
        #close_value_impl
        #root_entry_specifics
    })
}

fn generate_base_struct(
    name: &Ident,
    vis: &Visibility,
    generics: &Generics,
    attrs: &[Attribute],
    fields: &[MetricsField],
) -> Result<Ts2> {
    let has_named_fields = fields.iter().any(|f| f.name.is_some());
    let fields = fields.iter().map(|f| f.core_field(has_named_fields));
    let body = wrap_fields_into_struct_decl(has_named_fields, fields);

    Ok(quote! {
        #(#attrs)*
        #vis struct #name #generics #body
    })
}

fn wrap_fields_into_struct_decl(has_named_fields: bool, fields: impl Iterator<Item = Ts2>) -> Ts2 {
    if has_named_fields {
        quote! { { #(#fields,)* } }
    } else {
        quote! { ( #(#fields,)* ); }
    }
}

fn generate_entry_struct(
    name: &Ident,
    _generics: &Generics,
    fields: &[MetricsField],
    root_attrs: &RootAttributes,
) -> Result<Ts2> {
    let has_named_fields = fields.iter().any(|f| f.name.is_some());
    let config = root_attrs.configuration_fields();

    let fields = fields.iter().flat_map(|f| f.entry_field(has_named_fields));
    let body = wrap_fields_into_struct_decl(has_named_fields, config.into_iter().chain(fields));
    Ok(quote!(
        #[doc(hidden)]
        pub struct #name #body
    ))
}

fn generate_close_value_impls_for_struct(
    metrics_struct: &Ident,
    entry: &Ident,
    fields: &[MetricsField],
    root_attrs: &RootAttributes,
) -> Ts2 {
    let fields = fields
        .iter()
        .filter(|f| !matches!(f.attrs.kind, MetricsFieldKind::Ignore(_)))
        .map(|f| f.close_value(root_attrs.ownership_kind()));
    let config: Vec<Ts2> = root_attrs.create_configuration();
    generate_close_value_impls(
        root_attrs,
        metrics_struct,
        entry,
        quote! {
            #[allow(deprecated)]
            #entry {
                #(#config,)*
                #(#fields,)*
            }
        },
    )
}

pub(crate) fn clean_base_struct(
    vis: &syn::Visibility,
    struct_name: &syn::Ident,
    generics: &syn::Generics,
    filtered_attrs: Vec<Attribute>,
    fields: &FieldsNamed,
) -> Ts2 {
    // Strip out `metrics` attribute
    let clean_fields = fields.named.iter().map(|field| {
        let field_name = field.ident.as_ref().unwrap();
        let field_type = &field.ty;
        let field_vis = &field.vis;

        // Filter out metrics attributes
        let field_attrs = clean_attrs(&field.attrs);

        quote! {
            #(#field_attrs)*
            #field_vis #field_name: #field_type
        }
    });

    let expanded = quote! {
        #(#filtered_attrs)*
        #vis struct #struct_name #generics {
            #(#clean_fields),*
        }
    };

    expanded
}

pub(crate) fn clean_base_unnamed_struct(
    vis: &syn::Visibility,
    struct_name: &syn::Ident,
    generics: &syn::Generics,
    filtered_attrs: Vec<Attribute>,
    fields: &FieldsUnnamed,
) -> Ts2 {
    // Strip out `metrics` attribute
    let clean_fields = fields.unnamed.iter().map(|field| {
        let field_type = &field.ty;
        let field_vis = &field.vis;

        // Filter out metrics attributes
        let field_attrs = clean_attrs(&field.attrs);

        quote! {
            #(#field_attrs)*
            #field_vis #field_type
        }
    });

    let expanded = quote! {
        #(#filtered_attrs)*
        #vis struct #struct_name #generics (
            #(#clean_fields),*
        );
    };

    expanded
}
