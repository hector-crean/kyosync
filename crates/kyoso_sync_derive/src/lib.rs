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
//! - **Phase F** (landed): `#[crdt(sequence)]` for `String` / `Vec<T>`.
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
//! ## Codegen shape
//!
//! Per-field CRDT kind drives only the *schema-side type*; the
//! `diff` / `write_back` bodies are uniform — one delegation
//! to that type's [`kyoso_sync::SchemaField`] impl per field. The
//! per-kind diff/projection logic lives in those impls, not here.
//!
//! | Field attr | Schema-side type | `SchemaField` impl behaviour |
//! |---|---|---|
//! | (default) / `#[crdt(lww)]` | `LwwRegister<T>` | echo-guard against the field's default |
//! | `#[crdt(or_set)]` | `OrSet<T>` (T from `Vec<T>` / `HashSet<T>` / `BTreeSet<T>`) | set-diff: `Add` for new, `Remove` for missing |
//! | `#[crdt(counter)]` | `PnCounter` (component is an integer) | `Inc`/`Dec` by the signed diff |
//! | `#[crdt(map)]` | `CausalMap<LwwRegister<V>>` (component is `HashMap<String, V>`) | per-key `Apply` for changed values, `Remove` for absent keys |
//! | `#[crdt(nested)]` | `<T as SchemaSync>::Schema` | delegate to the inner type's own `diff` |
//! | `#[crdt(with = "Type")]` | the named `Type` (must impl [`kyoso_sync::SchemaField`]) | delegate to `Type`'s `SchemaField` impl |
//! | `#[crdt(sequence)]` | `Sequence<char>` (component is `String`) or `Sequence<T>` (component is `Vec<T>`) | prefix-suffix diff via `kyoso_sync::sequence_diff` |
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

    // Each field contributes one schema-struct field plus one uniform
    // `diff` / `write_back` arm. The CRDT kind only picks the
    // schema-side type; the diff/projection logic lives in that type's
    // `SchemaField` impl, which both arms delegate to.
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
        } = field;

        // The schema-side CRDT type for this field. This is the *only*
        // thing the CRDT kind influences; everything below is uniform.
        let schema_field_ty: TokenStream2 = match kind {
            FieldCrdtKind::Lww => quote! {
                ::kyoso_crdt::types::LwwRegister<#ty>
            },
            FieldCrdtKind::OrSet => {
                let inner = extract_set_inner_type(ty)?;
                quote! { ::kyoso_crdt::types::OrSet<#inner> }
            }
            FieldCrdtKind::Counter => {
                require_counter_type(ty)?;
                quote! { ::kyoso_crdt::types::PnCounter }
            }
            FieldCrdtKind::Map => {
                // CausalMap is fixed to String keys; LWW per-value is the
                // Phase D default — future work could parameterise the
                // inner CRDT.
                let value_ty = extract_map_value_type(ty)?;
                quote! {
                    ::kyoso_crdt::types::CausalMap<
                        ::kyoso_crdt::types::LwwRegister<#value_ty>,
                    >
                }
            }
            FieldCrdtKind::Nested => {
                // Field type must itself implement `SchemaSync`; its
                // schema struct is the schema-side type. `derive(SchemaSync)`
                // emits `impl SchemaField<T> for <T as SchemaSync>::Schema`,
                // so the delegation below resolves like any other field.
                quote! { <#ty as ::kyoso_sync::SchemaSync>::Schema }
            }
            FieldCrdtKind::Sequence => {
                // `String` → `Sequence<char>`; `Vec<T>` → `Sequence<T>`.
                let elem_ty = match analyze_sequence_field(ty)? {
                    SequenceContainer::String => quote! { ::core::primitive::char },
                    SequenceContainer::Vec(elem) => quote! { #elem },
                };
                quote! { ::kyoso_crdt::types::Sequence<#elem_ty> }
            }
            FieldCrdtKind::With(with_ty) => {
                // User-named schema type. Must impl
                // `kyoso_sync::SchemaField<#ty>`. A `Component`
                // mismatch surfaces as a compile-time type error at the
                // delegation call sites below.
                quote! { #with_ty }
            }
        };

        schema_struct_fields.push(quote! {
            #vis #field_schema_ident: #schema_field_ty,
        });
        // Outbound: diff the component field against the doc-side state.
        // `default_expr` is the echo-guard baseline — used by LWW,
        // ignored by additive CRDTs.
        changes_arms.push(quote! {
            out.extend(
                <#schema_field_ty as ::kyoso_sync::SchemaField<#ty>>::diff(
                    &doc.#field_schema_ident,
                    &self.#component_ident,
                    &#default_expr,
                )
                .into_iter()
                .map(#schema_mut_ident::#variant_ident),
            );
        });
        // Inbound: project the doc-side state back onto the component.
        write_back_arms.push(quote! {
            <#schema_field_ty as ::kyoso_sync::SchemaField<#ty>>::project_to(
                &schema.#field_schema_ident,
                &mut self.#component_ident,
            );
        });
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

        impl ::kyoso_sync::SchemaSync for #component_ident {
            type Schema = #schema_ident;
            const SCHEMA_NAME: &'static str = #schema_name;

            fn diff(
                &self,
                doc: &Self::Schema,
            ) -> ::kyoso_sync::SchemaMutations<Self> {
                // `default` is referenced by each field's echo-guard
                // baseline (`&default.<field>`). May be unused if every
                // field carries an explicit `#[crdt(default = ...)]`.
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

        // Lets `#component_ident` be embedded as a `#[crdt(nested)]` field
        // of another `SchemaSync` component: the parent delegates through
        // `SchemaField` exactly as it does for primitive CRDT fields.
        impl ::kyoso_sync::SchemaField<#component_ident> for #schema_ident {
            fn diff(
                &self,
                component: &#component_ident,
                _baseline: &#component_ident,
            ) -> ::std::vec::Vec<<Self as ::kyoso_crdt::Crdt>::Mutation> {
                <#component_ident as ::kyoso_sync::SchemaSync>::diff(
                    component, self,
                )
            }

            fn project_to(&self, component: &mut #component_ident) {
                <#component_ident as ::kyoso_sync::SchemaSync>::write_back(
                    component, self,
                );
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
    /// [`kyoso_sync::SchemaField`] over the component field type.
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
    /// `String` — schema element type is `char`.
    String,
    /// `Vec<T>` — schema element type is `T` borrowed from the field's
    /// generic.
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
