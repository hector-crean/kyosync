//! `#[derive(SchemaSync)]` for Bevy components in kyoso.
//!
//! ## Phases
//!
//! - **Phase A** (landed): scaffold + LWW happy path, `#[schema(name)]`.
//! - **Phase B** (landed): `#[crdt(skip)]`, `#[crdt(rename)]`,
//!   `#[crdt(default)]`.
//! - **Phase C** (landed): `#[crdt(or_set)]`, `#[crdt(counter)]`.
//! - **Phase D** (landed): `#[crdt(map)]`, `#[crdt(nested)]`.
//! - **Phase E** (landed): `#[crdt(with = "Type")]` escape hatch.
//! - **Phase F** (this): `#[crdt(sequence)]` for `String` / `Vec<T>`.
//!
//! ## Example
//!
//! ```ignore
//! #[derive(Component, Clone, Default, PartialEq, SchemaSync)]
//! #[schema(name = "Frame")]
//! pub struct Frame {
//!     pub name: String,
//!     pub visible: bool,
//!
//!     #[crdt(rename = "w")]
//!     pub width: f32,
//!
//!     #[crdt(or_set)]
//!     pub tags: Vec<String>,
//!
//!     #[crdt(counter)]
//!     pub edit_count: i64,
//!
//!     #[crdt(skip)]
//!     pub local_hover: HoverState,
//! }
//! ```
//!
//! Per-field CRDT kind drives schema-side type and the shape of the
//! generated `changes_against` / `write_back`:
//!
//! | Field attr | Schema-side type | `changes_against` semantics |
//! |---|---|---|
//! | (default) / `#[crdt(lww)]` | `LwwRegister<T>` | echo-guard against `Self::default()` |
//! | `#[crdt(or_set)]` | `OrSet<T>` (T from `Vec<T>` / `HashSet<T>`) | set-diff: `Add` for new, `Remove` for missing |
//! | `#[crdt(counter)]` | `PnCounter` (component is `i64`) | `Inc`/`Dec` by the signed diff |
//! | `#[crdt(map)]` | `CausalMap<LwwRegister<V>>` (component is `HashMap<String, V>`) | per-key `Apply` for changed values, `Remove` for absent keys |
//! | `#[crdt(nested)]` | `<T as SchemaSync>::Schema` | delegate to the inner type's own `changes_against` |
//! | `#[crdt(with = "Type")]` | the named `Type` (must impl [`kyoso_sync::SchemaField`](../kyoso_sync/trait.SchemaField.html)) | delegate to `Type`'s `SchemaField::changes_against` |
//! | `#[crdt(sequence)]` | `Sequence<char>` (component is `String`) or `Sequence<T>` (component is `Vec<T>`) | prefix-suffix diff via [`kyoso_sync::sequence_diff`] |
//!
//! See plan doc Part IX §IX.3 for the full default-CRDT-type rules.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{
    Data, DeriveInput, Expr, Fields, GenericArgument, Ident, LitStr, PathArguments, Type,
    parse_macro_input,
};

#[proc_macro_derive(SchemaSync, attributes(schema, crdt))]
pub fn derive_schema_sync(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let component_ident = &input.ident;
    let component_vis = &input.vis;
    let schema_ident = format_ident!("{}Schema", component_ident);
    let schema_mut_ident = format_ident!("{}SchemaMut", component_ident);

    let schema_name = container_schema_name(input)?
        .unwrap_or_else(|| component_ident.to_string());

    let raw_fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            Fields::Unit => {
                return Err(syn::Error::new_spanned(
                    component_ident,
                    "SchemaSync derive does not support unit structs (no fields to sync)",
                ));
            }
            Fields::Unnamed(unnamed) => {
                return Err(syn::Error::new_spanned(
                    unnamed,
                    "SchemaSync derive requires named fields. Tuple structs are not \
                     supported — give each field a name.",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new_spanned(
                component_ident,
                "SchemaSync derive requires a struct (not enum or union).",
            ));
        }
    };

    if raw_fields.is_empty() {
        return Err(syn::Error::new_spanned(
            component_ident,
            "SchemaSync derive requires at least one field.",
        ));
    }

    // Per-field analysis: skipped fields drop out, others are bucketed
    // by their CRDT kind for codegen.
    let mut synced: Vec<SyncedField> = Vec::new();
    for field in raw_fields {
        let component_ident = field
            .ident
            .as_ref()
            .expect("named fields enforced above")
            .clone();
        let opts = FieldOptions::parse(field)?;
        if opts.skip {
            continue;
        }
        let schema_ident = match &opts.rename {
            Some(name) => syn::parse_str::<Ident>(name).map_err(|_| {
                syn::Error::new_spanned(
                    field,
                    format!(
                        "#[crdt(rename = \"{name}\")] is not a valid Rust identifier",
                    ),
                )
            })?,
            None => component_ident.clone(),
        };
        let variant_ident = format_ident!("{}", to_pascal_case(&schema_ident.to_string()));
        let kind = opts.kind.unwrap_or(FieldCrdtKind::Lww);
        let default_expr: Expr = match opts.default_expr {
            Some(expr) => expr,
            None => syn::parse_quote!(default.#component_ident),
        };
        synced.push(SyncedField {
            component_ident,
            schema_ident,
            variant_ident,
            ty: field.ty.clone(),
            vis: field.vis.clone(),
            default_expr,
            kind,
        });
    }

    if synced.is_empty() {
        return Err(syn::Error::new_spanned(
            component_ident,
            "SchemaSync derive requires at least one synced field. \
             All fields are marked `#[crdt(skip)]`.",
        ));
    }

    // Generate per-field schema entries, changes_against arms, and
    // write_back arms. Each kind produces its own shape.
    let mut schema_struct_fields: Vec<TokenStream2> = Vec::new();
    let mut changes_arms: Vec<TokenStream2> = Vec::new();
    let mut write_back_arms: Vec<TokenStream2> = Vec::new();

    for field in &synced {
        let SyncedField {
            component_ident,
            schema_ident: field_schema_ident,
            variant_ident,
            ty,
            vis,
            default_expr,
            kind,
            ..
        } = field;
        match kind {
            FieldCrdtKind::Lww => {
                schema_struct_fields.push(quote! {
                    #vis #field_schema_ident:
                        ::kyoso_crdt::types::LwwRegister<#ty>,
                });
                changes_arms.push(quote! {
                    if *current.#field_schema_ident
                        .get()
                        .unwrap_or(&#default_expr)
                        != self.#component_ident
                    {
                        out.push(
                            #schema_mut_ident::#variant_ident(
                                ::kyoso_crdt::types::LwwMut::Set(
                                    ::core::clone::Clone::clone(
                                        &self.#component_ident,
                                    ),
                                ),
                            ),
                        );
                    }
                });
                write_back_arms.push(quote! {
                    if let ::core::option::Option::Some(value) =
                        schema.#field_schema_ident.get()
                    {
                        self.#component_ident = ::core::clone::Clone::clone(value);
                    }
                });
            }
            FieldCrdtKind::OrSet => {
                let inner = extract_set_inner_type(ty)?;
                schema_struct_fields.push(quote! {
                    #vis #field_schema_ident:
                        ::kyoso_crdt::types::OrSet<#inner>,
                });
                // Set-diff. Both sides yield `&T`, so comparisons via
                // `PartialEq for T` are direct (no deref gymnastics).
                changes_arms.push(quote! {
                    {
                        // Adds: elements present locally but absent from
                        // the doc-side membership.
                        for elem in self.#component_ident.iter() {
                            let in_doc = current.#field_schema_ident
                                .iter()
                                .any(|e| e == elem);
                            if !in_doc {
                                out.push(
                                    #schema_mut_ident::#variant_ident(
                                        ::kyoso_crdt::types::OrSetMut::Add(
                                            ::core::clone::Clone::clone(elem),
                                        ),
                                    ),
                                );
                            }
                        }
                        // Removes: elements present in the doc but no
                        // longer locally.
                        for doc_elem in current.#field_schema_ident.iter() {
                            let in_self = self.#component_ident
                                .iter()
                                .any(|e| e == doc_elem);
                            if !in_self {
                                out.push(
                                    #schema_mut_ident::#variant_ident(
                                        ::kyoso_crdt::types::OrSetMut::Remove(
                                            ::core::clone::Clone::clone(doc_elem),
                                        ),
                                    ),
                                );
                            }
                        }
                    }
                });
                write_back_arms.push(quote! {
                    self.#component_ident = schema.#field_schema_ident
                        .iter()
                        .cloned()
                        .collect();
                });
            }
            FieldCrdtKind::Counter => {
                // Validate the field type is a supported integer.
                require_counter_type(ty)?;
                schema_struct_fields.push(quote! {
                    #vis #field_schema_ident: ::kyoso_crdt::types::PnCounter,
                });
                // Diff against current.value(). Cast to i64 for safety;
                // the field type is constrained by require_counter_type.
                changes_arms.push(quote! {
                    {
                        let current_value: i64 =
                            current.#field_schema_ident.value();
                        let target_value: i64 = self.#component_ident as i64;
                        let diff: i64 = target_value - current_value;
                        if diff > 0 {
                            out.push(
                                #schema_mut_ident::#variant_ident(
                                    ::kyoso_crdt::types::PnMut::Inc(diff as u64),
                                ),
                            );
                        } else if diff < 0 {
                            out.push(
                                #schema_mut_ident::#variant_ident(
                                    ::kyoso_crdt::types::PnMut::Dec(
                                        diff.unsigned_abs(),
                                    ),
                                ),
                            );
                        }
                    }
                });
                write_back_arms.push(quote! {
                    {
                        // PnCounter::value() returns i64. Cast back to
                        // the component's declared field type. Using
                        // `as` here intentionally — the user opted into
                        // a counter on this type, so saturating /
                        // wrapping behavior is on them.
                        self.#component_ident =
                            schema.#field_schema_ident.value() as #ty;
                    }
                });
            }
            FieldCrdtKind::Map => {
                // Field must be `HashMap<String, V>` or
                // `BTreeMap<String, V>`. The schema-side type is
                // `CausalMap<LwwRegister<V>>` (CausalMap is fixed to
                // String keys; LWW per-value is the Phase D default —
                // future work could parameterise the inner CRDT).
                let value_ty = extract_map_value_type(ty)?;
                schema_struct_fields.push(quote! {
                    #vis #field_schema_ident:
                        ::kyoso_crdt::types::CausalMap<
                            ::kyoso_crdt::types::LwwRegister<#value_ty>,
                        >,
                });
                changes_arms.push(quote! {
                    {
                        // Per-key diff. Apply for keys whose component
                        // value differs from the doc; Remove for keys
                        // present in the doc but not in the component.
                        for (k, v) in self.#component_ident.iter() {
                            let key_str: &::core::primitive::str = k.as_ref();
                            let doc_value = current.#field_schema_ident
                                .get(key_str)
                                .and_then(|reg| reg.get());
                            let needs_apply = match doc_value {
                                ::core::option::Option::Some(cur) => cur != v,
                                ::core::option::Option::None => true,
                            };
                            if needs_apply {
                                out.push(
                                    #schema_mut_ident::#variant_ident(
                                        ::kyoso_crdt::types::MapMut::Apply {
                                            key: ::core::clone::Clone::clone(k),
                                            mutation: ::kyoso_crdt::types::LwwMut::Set(
                                                ::core::clone::Clone::clone(v),
                                            ),
                                        },
                                    ),
                                );
                            }
                        }
                        for (doc_key, _) in current.#field_schema_ident.iter() {
                            if !self.#component_ident.contains_key(doc_key.as_str()) {
                                out.push(
                                    #schema_mut_ident::#variant_ident(
                                        ::kyoso_crdt::types::MapMut::Remove {
                                            key: ::core::clone::Clone::clone(doc_key),
                                        },
                                    ),
                                );
                            }
                        }
                    }
                });
                write_back_arms.push(quote! {
                    {
                        // Rebuild the component's map from the
                        // server-confirmed schema state. Keys whose
                        // LwwRegister is bottom (no value yet) are
                        // skipped; this matches the semantics of an
                        // unobserved key in the original component.
                        self.#component_ident = schema.#field_schema_ident
                            .iter()
                            .filter_map(|(k, reg)| {
                                reg.get().map(|v| {
                                    (
                                        ::core::clone::Clone::clone(k),
                                        ::core::clone::Clone::clone(v),
                                    )
                                })
                            })
                            .collect();
                    }
                });
            }
            FieldCrdtKind::Nested => {
                // Field type must implement `SchemaSync`. The schema-
                // side type is the inner type's own schema struct.
                schema_struct_fields.push(quote! {
                    #vis #field_schema_ident:
                        <#ty as ::kyoso_graph_sync::SchemaSync>::Schema,
                });
                // Delegate to the inner type's own changes_against and
                // wrap each emitted mutation in this field's variant.
                changes_arms.push(quote! {
                    {
                        for inner_mut in
                            ::kyoso_graph_sync::SchemaSync::changes_against(
                                &self.#component_ident,
                                &current.#field_schema_ident,
                            )
                        {
                            out.push(
                                #schema_mut_ident::#variant_ident(inner_mut),
                            );
                        }
                    }
                });
                write_back_arms.push(quote! {
                    {
                        ::kyoso_graph_sync::SchemaSync::write_back(
                            &mut self.#component_ident,
                            &schema.#field_schema_ident,
                        );
                    }
                });
            }
            FieldCrdtKind::Sequence => {
                // `String` → `Sequence<char>`; `Vec<T>` → `Sequence<T>`.
                // Other field shapes are rejected at expansion time.
                let container = analyze_sequence_field(ty)?;
                let (elem_ty, component_iter, write_back_collect) = match container {
                    SequenceContainer::String => (
                        quote! { ::core::primitive::char },
                        quote! { self.#component_ident.chars() },
                        quote! {
                            schema.#field_schema_ident
                                .iter()
                                .into_iter()
                                .copied()
                                .collect::<::std::string::String>()
                        },
                    ),
                    SequenceContainer::Vec(elem) => (
                        quote! { #elem },
                        quote! { self.#component_ident.iter().cloned() },
                        quote! {
                            schema.#field_schema_ident
                                .iter()
                                .into_iter()
                                .cloned()
                                .collect::<::std::vec::Vec<#elem>>()
                        },
                    ),
                };
                schema_struct_fields.push(quote! {
                    #vis #field_schema_ident:
                        ::kyoso_crdt::types::Sequence<#elem_ty>,
                });
                changes_arms.push(quote! {
                    {
                        // Materialize doc-side as Vec<elem> for indexed
                        // diff. `Sequence::iter()` already returns
                        // `Vec<&T>`, so we just clone-collect.
                        let doc_view: ::std::vec::Vec<#elem_ty> =
                            current.#field_schema_ident
                                .iter()
                                .into_iter()
                                .cloned()
                                .collect();
                        let component_view = #component_iter;
                        for inner_mut in ::kyoso_sync::sequence_diff::<
                            #elem_ty, _, _,
                        >(doc_view, component_view) {
                            out.push(
                                #schema_mut_ident::#variant_ident(inner_mut),
                            );
                        }
                    }
                });
                write_back_arms.push(quote! {
                    self.#component_ident = #write_back_collect;
                });
            }
            FieldCrdtKind::With(with_ty) => {
                // User-named schema type. Must impl
                // `kyoso_sync::SchemaField<Component = <field-type>>`.
                // The `Component = #ty` constraint is enforced at the
                // call sites via type inference: `changes_against`
                // takes `&Self::Component` and `project_to` takes
                // `&mut Self::Component`, so a mismatch surfaces as a
                // compile-time type error.
                schema_struct_fields.push(quote! {
                    #vis #field_schema_ident: #with_ty,
                });
                changes_arms.push(quote! {
                    {
                        for inner_mut in
                            <#with_ty as ::kyoso_graph_sync::SchemaField>::changes_against(
                                &current.#field_schema_ident,
                                &self.#component_ident,
                            )
                        {
                            out.push(
                                #schema_mut_ident::#variant_ident(inner_mut),
                            );
                        }
                    }
                });
                write_back_arms.push(quote! {
                    {
                        <#with_ty as ::kyoso_graph_sync::SchemaField>::project_to(
                            &schema.#field_schema_ident,
                            &mut self.#component_ident,
                        );
                    }
                });
            }
        }
    }

    Ok(quote! {
        #[derive(
            ::core::clone::Clone,
            ::core::fmt::Debug,
            ::core::default::Default,
            ::core::cmp::PartialEq,
            ::kyoso_crdt::DeriveCrdt,
        )]
        #component_vis struct #schema_ident {
            #( #schema_struct_fields )*
        }

        impl ::kyoso_graph_sync::SchemaSync for #component_ident {
            type Schema = #schema_ident;
            const SCHEMA_NAME: &'static str = #schema_name;

            fn changes_against(
                &self,
                current: &Self::Schema,
            ) -> ::std::vec::Vec<
                <Self::Schema as ::kyoso_crdt::Crdt>::Mutation,
            > {
                // `default` is referenced by LWW field arms via the
                // `#[crdt(default = ...)]` fallback expression. May be
                // unused if every field is non-LWW or has an explicit
                // override.
                #[allow(unused_variables)]
                let default = <Self as ::core::default::Default>::default();
                let mut out = ::std::vec::Vec::new();
                #( #changes_arms )*
                out
            }

            fn write_back(&mut self, schema: &Self::Schema) {
                #( #write_back_arms )*
            }
        }
    })
}

struct SyncedField {
    component_ident: Ident,
    schema_ident: Ident,
    variant_ident: Ident,
    ty: Type,
    vis: syn::Visibility,
    default_expr: Expr,
    kind: FieldCrdtKind,
}

#[derive(Clone, Debug)]
enum FieldCrdtKind {
    Lww,
    OrSet,
    Counter,
    Map,
    Nested,
    Sequence,
    /// `#[crdt(with = "Type")]` — schema-side type is the user-named
    /// `Type`, which must implement
    /// [`kyoso_sync::SchemaField`](../kyoso_sync/trait.SchemaField.html)
    /// with `Component = <field-type>`.
    With(Type),
}

#[derive(Default)]
struct FieldOptions {
    skip: bool,
    rename: Option<String>,
    default_expr: Option<Expr>,
    kind: Option<FieldCrdtKind>,
}

impl FieldOptions {
    fn parse(field: &syn::Field) -> syn::Result<Self> {
        let mut opts = Self::default();
        for attr in &field.attrs {
            if !attr.path().is_ident("crdt") {
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("skip") {
                    opts.skip = true;
                    Ok(())
                } else if meta.path.is_ident("rename") {
                    let lit: LitStr = meta.value()?.parse()?;
                    if opts.rename.is_some() {
                        return Err(meta.error("duplicate `rename` in #[crdt(...)]"));
                    }
                    opts.rename = Some(lit.value());
                    Ok(())
                } else if meta.path.is_ident("default") {
                    let lit: LitStr = meta.value()?.parse()?;
                    if opts.default_expr.is_some() {
                        return Err(meta.error("duplicate `default` in #[crdt(...)]"));
                    }
                    let parsed: Expr = syn::parse_str(&lit.value()).map_err(|e| {
                        meta.error(format!(
                            "#[crdt(default = \"...\")] expression failed to parse: {e}",
                        ))
                    })?;
                    opts.default_expr = Some(parsed);
                    Ok(())
                } else if meta.path.is_ident("with") {
                    let lit: LitStr = meta.value()?.parse()?;
                    if opts.kind.is_some() {
                        return Err(meta.error(
                            "multiple CRDT-kind attributes on the same field are not allowed; \
                             pick exactly one of `lww`, `or_set`, `counter`, `map`, `nested`, \
                             `with`.",
                        ));
                    }
                    let parsed: Type = syn::parse_str(&lit.value()).map_err(|e| {
                        meta.error(format!(
                            "#[crdt(with = \"...\")] type path failed to parse: {e}",
                        ))
                    })?;
                    opts.kind = Some(FieldCrdtKind::With(parsed));
                    Ok(())
                } else if let Some(ident) = meta.path.get_ident() {
                    let name = ident.to_string();
                    let kind = match name.as_str() {
                        "lww" => Some(FieldCrdtKind::Lww),
                        "or_set" => Some(FieldCrdtKind::OrSet),
                        "counter" => Some(FieldCrdtKind::Counter),
                        "map" => Some(FieldCrdtKind::Map),
                        "nested" => Some(FieldCrdtKind::Nested),
                        "sequence" => Some(FieldCrdtKind::Sequence),
                        _ => None,
                    };
                    if let Some(k) = kind {
                        if opts.kind.is_some() {
                            return Err(meta.error(
                                "multiple CRDT-kind attributes on the same field are not allowed; \
                                 pick exactly one of `lww`, `or_set`, `counter`, `map`, `nested`, \
                                 `sequence`, `with`.",
                            ));
                        }
                        opts.kind = Some(k);
                        Ok(())
                    } else {
                        Err(meta.error(format!("unknown #[crdt(...)] key: `{name}`")))
                    }
                } else {
                    Err(meta.error("unknown #[crdt(...)] key"))
                }
            })?;
        }
        if opts.skip
            && (opts.rename.is_some() || opts.default_expr.is_some() || opts.kind.is_some())
        {
            return Err(syn::Error::new_spanned(
                field,
                "#[crdt(skip)] cannot be combined with `rename`, `default`, or a \
                 CRDT-kind attribute (the field doesn't appear in the schema, so the \
                 other settings have nothing to apply to)",
            ));
        }
        if let (Some(non_lww), Some(_)) = (&opts.kind, &opts.default_expr) {
            if !matches!(non_lww, FieldCrdtKind::Lww) {
                return Err(syn::Error::new_spanned(
                    field,
                    "#[crdt(default = ...)] is only meaningful for LWW fields; \
                     non-LWW kinds (or_set, counter, map, nested, with, sequence) \
                     use their own bottom semantics.",
                ));
            }
        }
        Ok(opts)
    }
}

/// Extract `T` from a field type expected to be `Vec<T>`, `HashSet<T>`,
/// or `BTreeSet<T>`. Used by `#[crdt(or_set)]` codegen.
fn extract_set_inner_type(ty: &Type) -> syn::Result<&Type> {
    let path = match ty {
        Type::Path(tp) => &tp.path,
        _ => {
            return Err(syn::Error::new_spanned(
                ty,
                "#[crdt(or_set)] requires a `Vec<T>` / `HashSet<T>` / `BTreeSet<T>` field",
            ));
        }
    };
    let last = path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(ty, "#[crdt(or_set)] field type has no path segments")
    })?;
    let segment_name = last.ident.to_string();
    let supported = matches!(segment_name.as_str(), "Vec" | "HashSet" | "BTreeSet");
    if !supported {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[crdt(or_set)] expects `Vec<T>`, `HashSet<T>`, or `BTreeSet<T>`; \
                 found `{segment_name}<...>`",
            ),
        ));
    }
    let args = match &last.arguments {
        PathArguments::AngleBracketed(a) => a,
        _ => {
            return Err(syn::Error::new_spanned(
                ty,
                "#[crdt(or_set)] field type must have a generic parameter",
            ));
        }
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let inner = type_args.next().ok_or_else(|| {
        syn::Error::new_spanned(ty, "#[crdt(or_set)] field type has no element type")
    })?;
    if type_args.next().is_some() {
        return Err(syn::Error::new_spanned(
            ty,
            "#[crdt(or_set)] field type has more than one generic — only single-element \
             collections are supported",
        ));
    }
    Ok(inner)
}

/// Extract `V` from a field type expected to be `HashMap<String, V>` or
/// `BTreeMap<String, V>`. The key type must be `String` because
/// `kyoso_crdt::types::CausalMap` is keyed by `String` only.
fn extract_map_value_type(ty: &Type) -> syn::Result<&Type> {
    let path = match ty {
        Type::Path(tp) => &tp.path,
        _ => {
            return Err(syn::Error::new_spanned(
                ty,
                "#[crdt(map)] requires a `HashMap<String, V>` or `BTreeMap<String, V>` field",
            ));
        }
    };
    let last = path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(ty, "#[crdt(map)] field type has no path segments")
    })?;
    let segment_name = last.ident.to_string();
    let supported = matches!(segment_name.as_str(), "HashMap" | "BTreeMap");
    if !supported {
        return Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[crdt(map)] expects `HashMap<String, V>` or `BTreeMap<String, V>`; \
                 found `{segment_name}<...>`",
            ),
        ));
    }
    let args = match &last.arguments {
        PathArguments::AngleBracketed(a) => a,
        _ => {
            return Err(syn::Error::new_spanned(
                ty,
                "#[crdt(map)] field type must have generic parameters",
            ));
        }
    };
    let mut type_args = args.args.iter().filter_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    });
    let key_ty = type_args.next().ok_or_else(|| {
        syn::Error::new_spanned(ty, "#[crdt(map)] field type has no key type")
    })?;
    let value_ty = type_args.next().ok_or_else(|| {
        syn::Error::new_spanned(ty, "#[crdt(map)] field type has no value type")
    })?;
    if type_args.next().is_some() {
        return Err(syn::Error::new_spanned(
            ty,
            "#[crdt(map)] field type has more than two generics",
        ));
    }
    // Require key to be `String`. CausalMap is fixed to String keys; if
    // we ever lift that, we can relax here too.
    let key_ok = matches!(
        key_ty,
        Type::Path(tp)
            if tp.path.segments.last().map(|s| s.ident.to_string()).as_deref()
                == Some("String"),
    );
    if !key_ok {
        return Err(syn::Error::new_spanned(
            key_ty,
            "#[crdt(map)] keys must be `String` (kyoso_crdt::types::CausalMap is \
             string-keyed). Use a manual schema if you need a different key type.",
        ));
    }
    Ok(value_ty)
}

/// Container shape detected for a `#[crdt(sequence)]` field.
enum SequenceContainer<'a> {
    /// `String` — schema element type is `char`; component iter is
    /// `chars()`; write-back collects to `String`.
    String,
    /// `Vec<T>` — schema element type is `T` borrowed from the field's
    /// generic; component iter is `iter().cloned()`; write-back
    /// collects to `Vec<T>`.
    Vec(&'a Type),
}

/// Inspect a `#[crdt(sequence)]` field type and classify it as `String`
/// or `Vec<T>`. Other shapes are rejected at expansion time.
fn analyze_sequence_field(ty: &Type) -> syn::Result<SequenceContainer<'_>> {
    let path = match ty {
        Type::Path(tp) => &tp.path,
        _ => {
            return Err(syn::Error::new_spanned(
                ty,
                "#[crdt(sequence)] requires a `String` or `Vec<T>` field",
            ));
        }
    };
    let last = path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(ty, "#[crdt(sequence)] field type has no path segments")
    })?;
    let segment_name = last.ident.to_string();
    match segment_name.as_str() {
        "String" => {
            if !matches!(last.arguments, PathArguments::None) {
                return Err(syn::Error::new_spanned(
                    ty,
                    "#[crdt(sequence)] String field type must have no generic parameters",
                ));
            }
            Ok(SequenceContainer::String)
        }
        "Vec" => {
            let args = match &last.arguments {
                PathArguments::AngleBracketed(a) => a,
                _ => {
                    return Err(syn::Error::new_spanned(
                        ty,
                        "#[crdt(sequence)] Vec field type must have a generic parameter",
                    ));
                }
            };
            let mut type_args = args.args.iter().filter_map(|a| match a {
                GenericArgument::Type(t) => Some(t),
                _ => None,
            });
            let inner = type_args.next().ok_or_else(|| {
                syn::Error::new_spanned(ty, "#[crdt(sequence)] Vec has no element type")
            })?;
            if type_args.next().is_some() {
                return Err(syn::Error::new_spanned(
                    ty,
                    "#[crdt(sequence)] Vec must have exactly one generic argument",
                ));
            }
            Ok(SequenceContainer::Vec(inner))
        }
        other => Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[crdt(sequence)] expects `String` or `Vec<T>`; found `{other}<...>`",
            ),
        )),
    }
}

/// Validate that a `#[crdt(counter)]` field type is one of the supported
/// integer types. The codegen casts both ways through `i64`.
fn require_counter_type(ty: &Type) -> syn::Result<()> {
    let segment_name = match ty {
        Type::Path(tp) => tp
            .path
            .segments
            .last()
            .map(|s| s.ident.to_string())
            .unwrap_or_default(),
        _ => String::new(),
    };
    match segment_name.as_str() {
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "isize" | "usize" => {
            Ok(())
        }
        _ => Err(syn::Error::new_spanned(
            ty,
            format!(
                "#[crdt(counter)] expects an integer field type \
                 (i8/i16/i32/i64/u8/u16/u32/u64/isize/usize); found `{segment_name}`",
            ),
        )),
    }
}

/// Read `#[schema(name = "...")]` from the container attributes. Returns
/// `Ok(None)` if no `schema` attribute is present.
fn container_schema_name(input: &DeriveInput) -> syn::Result<Option<String>> {
    let mut found: Option<String> = None;
    for attr in &input.attrs {
        if !attr.path().is_ident("schema") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let lit: LitStr = meta.value()?.parse()?;
                if found.is_some() {
                    return Err(meta.error("duplicate `name` in #[schema(...)]"));
                }
                found = Some(lit.value());
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unknown #[schema(...)] key: `{}`",
                    meta.path
                        .get_ident()
                        .map(ToString::to_string)
                        .unwrap_or_else(|| "<non-ident>".into()),
                )))
            }
        })?;
    }
    Ok(found)
}

fn to_pascal_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut next_upper = true;
    for ch in s.chars() {
        if ch == '_' {
            next_upper = true;
        } else if next_upper {
            out.extend(ch.to_uppercase());
            next_upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

