//! Internal derive for a registry section contract and Parquet codec.
//!
//! `#[derive(Section)]` accepts a named-field struct with one
//! `#[section(id = ..., name = ..., semantics = ..., sort_key(...),
//! identity(...))]` attribute. Every field needs `#[column(class)]`; a column
//! may also declare a collection gate and row-specific gate overrides.
//!
//! The generated sealed `kronika_registry::Section` implementation exposes
//! one `TypeContract`, encodes at most `MAX_SECTION_ROWS`, decodes only a
//! CRC-verified section, and derives its timestamp range from a field named
//! `ts`. Registry linting validates references and semantic invariants across
//! the complete contract set.
//!
//! Supported field spellings are the registry primitive integer and float
//! widths, `bool`, `Ts`, `StrId`, `Vec<i32>`, and `Option<T>` for nullable
//! scalars. Types are matched by their written identifiers because a proc
//! macro receives tokens, not resolved Rust types. Unsupported shapes and
//! attributes fail at compile time with a span-local error.
//!
//! This proc macro is an implementation detail of `kronika-registry`; it is
//! not the extension point for downstream section types.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use syn::spanned::Spanned;
use syn::{Data, DeriveInput, Fields, Ident, LitInt, LitStr, Token, Type, parse_macro_input};

/// Derive the section contract and Parquet codec for a typed struct.
///
/// See the crate docs for the attribute grammar.
#[proc_macro_derive(Section, attributes(section, column))]
pub fn derive_section(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Collected `#[section(..)]` header values.
struct Header {
    id: LitInt,
    name: LitStr,
    semantics: Ident,
    sort_key: Vec<LitStr>,
    identity: Vec<LitStr>,
}

/// One resolved column: its field, on-disk shape, and class.
struct ColumnDef {
    field: Ident,
    name: String,
    /// `ColumnType` variant ident, e.g. `I64`, `Ts`.
    column_type: Ident,
    /// `ColumnClass` variant ident, e.g. `Cumulative`.
    column_class: Ident,
    /// Arrow primitive type token for the shared helpers, or `None` for
    /// `bool` (which uses the dedicated boolean helpers).
    arrow_type: Option<Ident>,
    /// Wrapper over the Arrow native value (`Ts`, `StrId`), or `None` when the
    /// field already is the native type. Encode reads `.0`; decode wraps the
    /// decoded value back into it.
    wrapper: Option<Ident>,
    nullable: bool,
    gate: Option<GateDef>,
}

struct GateDef {
    default: (String, String),
    overrides: Vec<GateOverrideDef>,
}

struct GateOverrideDef {
    column: String,
    value: String,
    gate: (String, String),
}

fn expand(input: &DeriveInput) -> syn::Result<TokenStream2> {
    let header = parse_header(input)?;
    let fields = match &input.data {
        Data::Struct(data) => match &data.fields {
            Fields::Named(named) => &named.named,
            _ => {
                return Err(syn::Error::new(
                    Span::call_site(),
                    "Section requires a struct with named fields",
                ));
            }
        },
        _ => {
            return Err(syn::Error::new(
                Span::call_site(),
                "Section can only be derived for a struct",
            ));
        }
    };

    let columns: Vec<ColumnDef> = fields
        .iter()
        .map(parse_column)
        .collect::<syn::Result<_>>()?;

    let struct_name = &input.ident;
    let contract = build_contract(&header, &columns);
    let encode = build_encode(&columns);
    let decode = build_decode(struct_name, &columns);
    let ts_range = build_ts_range(&columns);

    Ok(quote! {
        impl ::kronika_registry::sealed::Sealed for #struct_name {}

        impl ::kronika_registry::Section for #struct_name {
            #contract

            fn encode(rows: &[Self]) -> ::core::result::Result<
                ::std::vec::Vec<u8>,
                ::kronika_registry::CodecError,
            > {
                // Reject over-cap input before building Arrow arrays.
                ::kronika_registry::check_row_cap(rows.len())?;
                let columns = #encode;
                ::kronika_registry::encode_section(&Self::CONTRACT, columns)
            }

            fn decode(section: ::kronika_registry::VerifiedSection) -> ::core::result::Result<
                ::std::vec::Vec<Self>,
                ::kronika_registry::CodecError,
            > {
                #decode
            }

            #ts_range
        }
    })
}

fn parse_header(input: &DeriveInput) -> syn::Result<Header> {
    let attr = input
        .attrs
        .iter()
        .find(|a| a.path().is_ident("section"))
        .ok_or_else(|| {
            syn::Error::new(
                Span::call_site(),
                "Section requires a #[section(..)] header",
            )
        })?;

    let mut id = None;
    let mut name = None;
    let mut semantics = None;
    let mut sort_key = Vec::new();
    let mut identity = Vec::new();

    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("id") {
            id = Some(meta.value()?.parse::<LitInt>()?);
        } else if meta.path.is_ident("name") {
            name = Some(meta.value()?.parse::<LitStr>()?);
        } else if meta.path.is_ident("semantics") {
            semantics = Some(meta.value()?.parse::<Ident>()?);
        } else if meta.path.is_ident("sort_key") {
            let content;
            syn::parenthesized!(content in meta.input);
            let keys = content.parse_terminated(<LitStr as syn::parse::Parse>::parse, Token![,])?;
            sort_key = keys.into_iter().collect();
        } else if meta.path.is_ident("identity") {
            let content;
            syn::parenthesized!(content in meta.input);
            let keys = content.parse_terminated(<LitStr as syn::parse::Parse>::parse, Token![,])?;
            identity = keys.into_iter().collect();
        } else {
            return Err(meta.error("unknown #[section(..)] key"));
        }
        Ok(())
    })?;

    Ok(Header {
        id: id.ok_or_else(|| syn::Error::new(attr.span(), "#[section(..)] needs `id`"))?,
        name: name.ok_or_else(|| syn::Error::new(attr.span(), "#[section(..)] needs `name`"))?,
        semantics: semantics
            .ok_or_else(|| syn::Error::new(attr.span(), "#[section(..)] needs `semantics`"))?,
        sort_key,
        identity,
    })
}

fn parse_column(field: &syn::Field) -> syn::Result<ColumnDef> {
    let field_ident = field
        .ident
        .clone()
        .ok_or_else(|| syn::Error::new(field.span(), "field needs a name"))?;

    let class_attr = field
        .attrs
        .iter()
        .find(|a| a.path().is_ident("column"))
        .ok_or_else(|| syn::Error::new(field.span(), "field needs a #[column(class)] attribute"))?;
    let args: ColumnArgs = class_attr.parse_args()?;
    let column_class = column_class(&args.class)?;
    let gate = args.gate;

    let (inner, nullable) = unwrap_option(&field.ty);

    // `Vec<i32>` is not a bare ident, so it has its own branch: a list column is
    // never NULL (an empty vec is an empty list) and needs no Arrow scalar type
    // or wrapper.
    if is_vec_i32(inner) {
        return Ok(ColumnDef {
            name: field_ident.to_string(),
            field: field_ident,
            column_type: Ident::new("ListI32", Span::call_site()),
            column_class,
            arrow_type: None,
            wrapper: None,
            nullable: false,
            gate,
        });
    }

    let inner_ident = type_ident(inner)?;
    let (column_type, arrow_type, wrapper) = map_type(&inner_ident, &column_class)?;

    Ok(ColumnDef {
        name: field_ident.to_string(),
        field: field_ident,
        column_type,
        column_class,
        arrow_type,
        wrapper,
        nullable,
        gate,
    })
}

/// Arguments of `#[column(...)]`.
struct ColumnArgs {
    class: Ident,
    gate: Option<GateDef>,
}

impl syn::parse::Parse for ColumnArgs {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let class: Ident = input.parse()?;
        let mut default = None;
        let mut overrides = Vec::new();
        while input.peek(syn::Token![,]) {
            input.parse::<syn::Token![,]>()?;
            let key: Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            let value: LitStr = input.parse()?;
            if key == "gated_by" {
                if default.is_some() {
                    return Err(syn::Error::new(key.span(), "duplicate `gated_by`"));
                }
                default = Some(parse_section_ref(&value, "gated_by")?);
            } else if key == "gate_override" {
                overrides.push(parse_gate_override(&value)?);
            } else {
                return Err(syn::Error::new(
                    key.span(),
                    "expected `gated_by` or `gate_override`",
                ));
            }
        }
        let gate = match default {
            Some(default) => Some(GateDef { default, overrides }),
            None if overrides.is_empty() => None,
            None => {
                return Err(syn::Error::new(
                    class.span(),
                    "`gate_override` requires `gated_by`",
                ));
            }
        };
        Ok(Self { class, gate })
    }
}

fn parse_section_ref(value: &LitStr, key: &str) -> syn::Result<(String, String)> {
    let raw = value.value();
    let Some((section, column)) = raw.split_once('.') else {
        return Err(syn::Error::new(
            value.span(),
            format!("{key} must be \"section.column\""),
        ));
    };
    if section.is_empty() || column.is_empty() || column.contains('.') {
        return Err(syn::Error::new(
            value.span(),
            format!("{key} must be \"section.column\""),
        ));
    }
    Ok((section.to_owned(), column.to_owned()))
}

fn parse_gate_override(value: &LitStr) -> syn::Result<GateOverrideDef> {
    let raw = value.value();
    let Some((selector, gate)) = raw.split_once("=>") else {
        return Err(syn::Error::new(
            value.span(),
            "gate_override must be \"column=value=>section.column\"",
        ));
    };
    let Some((column, expected)) = selector.split_once('=') else {
        return Err(syn::Error::new(
            value.span(),
            "gate_override must be \"column=value=>section.column\"",
        ));
    };
    if column.is_empty() || expected.is_empty() || column.contains('.') {
        return Err(syn::Error::new(
            value.span(),
            "gate_override must be \"column=value=>section.column\"",
        ));
    }
    let gate_lit = LitStr::new(gate, value.span());
    Ok(GateOverrideDef {
        column: column.to_owned(),
        value: expected.to_owned(),
        gate: parse_section_ref(&gate_lit, "gate_override")?,
    })
}

fn column_class(ident: &Ident) -> syn::Result<Ident> {
    let variant = match ident.to_string().as_str() {
        "c" => "Cumulative",
        "g" => "Gauge",
        "l" => "Label",
        "t" => "Timestamp",
        _ => {
            return Err(syn::Error::new(
                ident.span(),
                "column class must be one of c (cumulative), g (gauge), l (label), t (timestamp)",
            ));
        }
    };
    Ok(Ident::new(variant, ident.span()))
}

/// Map a field's base-type ident and class to its `ColumnType`, Arrow type
/// token, and optional wrapper (`Ts` or `StrId`).
fn map_type(ident: &Ident, class: &Ident) -> syn::Result<(Ident, Option<Ident>, Option<Ident>)> {
    let span = ident.span();
    let is_timestamp = class == "Timestamp";

    let (column_type, arrow_type, wrapper): (&str, Option<&str>, Option<&str>) = match ident
        .to_string()
        .as_str()
    {
        "i8" => ("I8", Some("Int8Type"), None),
        "i16" => ("I16", Some("Int16Type"), None),
        "i32" => ("I32", Some("Int32Type"), None),
        "i64" => ("I64", Some("Int64Type"), None),
        "u8" => ("U8", Some("UInt8Type"), None),
        "u16" => ("U16", Some("UInt16Type"), None),
        "u32" => ("U32", Some("UInt32Type"), None),
        "u64" => ("U64", Some("UInt64Type"), None),
        "f32" => ("F32", Some("Float32Type"), None),
        "f64" => ("F64", Some("Float64Type"), None),
        "bool" => ("Bool", None, None),
        "Ts" => ("Ts", Some("Int64Type"), Some("Ts")),
        "StrId" => ("StrId", Some("UInt64Type"), Some("StrId")),
        other => {
            return Err(syn::Error::new(
                span,
                format!(
                    "unsupported column type `{other}`; expected a base type like i64, u32, f64, bool, Ts, StrId"
                ),
            ));
        }
    };

    if is_timestamp && column_type != "Ts" {
        return Err(syn::Error::new(
            span,
            "a column of class t (timestamp) must be a `Ts`",
        ));
    }

    Ok((
        Ident::new(column_type, span),
        arrow_type.map(|at| Ident::new(at, span)),
        wrapper.map(|w| Ident::new(w, span)),
    ))
}

/// Split `Option<T>` into `(T, true)`; a non-option type into `(ty, false)`.
fn unwrap_option(ty: &Type) -> (&Type, bool) {
    if let Type::Path(path) = ty
        && path.qself.is_none()
        && let Some(segment) = path.path.segments.last()
        && segment.ident == "Option"
        && let syn::PathArguments::AngleBracketed(args) = &segment.arguments
        && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
    {
        return (inner, true);
    }
    (ty, false)
}

/// True for a `Vec<i32>` field type.
fn is_vec_i32(ty: &Type) -> bool {
    let Type::Path(path) = ty else { return false };
    let Some(segment) = path.path.segments.last() else {
        return false;
    };
    if segment.ident != "Vec" {
        return false;
    }
    let syn::PathArguments::AngleBracketed(args) = &segment.arguments else {
        return false;
    };
    matches!(
        args.args.first(),
        Some(syn::GenericArgument::Type(inner))
            if type_ident(inner).is_ok_and(|ident| ident == "i32")
    )
}

/// The single path-segment ident of a simple type like `i64`.
fn type_ident(ty: &Type) -> syn::Result<Ident> {
    if let Type::Path(path) = ty
        && path.qself.is_none()
        && let Some(segment) = path.path.segments.last()
        && segment.arguments.is_empty()
    {
        return Ok(segment.ident.clone());
    }
    Err(syn::Error::new(ty.span(), "expected a simple base type"))
}

fn build_contract(header: &Header, columns: &[ColumnDef]) -> TokenStream2 {
    let id = &header.id;
    let name = &header.name;
    let semantics_variant = semantics_variant(&header.semantics);
    let sort_key = &header.sort_key;
    let identity = &header.identity;

    let column_entries = columns.iter().map(|c| {
        let name = &c.name;
        let ty = &c.column_type;
        let class = &c.column_class;
        let nullable = c.nullable;
        let gated_by = c.gate.as_ref().map_or_else(
            || quote! { ::core::option::Option::None },
            |gate| {
                let (section, column) = &gate.default;
                let overrides = gate.overrides.iter().map(|rule| {
                    let selector = &rule.column;
                    let value = &rule.value;
                    let (gate_section, gate_column) = &rule.gate;
                    quote! {
                        ::kronika_registry::RowGateOverride {
                            column: #selector,
                            value: #value,
                            gate: ::kronika_registry::SectionColumnRef {
                                section: #gate_section,
                                column: #gate_column,
                            },
                        }
                    }
                });
                quote! {
                    ::core::option::Option::Some(::kronika_registry::CollectionGate {
                        default: ::kronika_registry::SectionColumnRef {
                            section: #section,
                            column: #column,
                        },
                        overrides: &[ #( #overrides ),* ],
                    })
                }
            },
        );
        quote! {
            ::kronika_registry::Column {
                name: #name,
                ty: ::kronika_registry::ColumnType::#ty,
                class: ::kronika_registry::ColumnClass::#class,
                nullable: #nullable,
                gated_by: #gated_by,
            }
        }
    });

    // `TypeId::new` runs in const context, so an invalid id fails compilation.
    quote! {
        const CONTRACT: ::kronika_registry::TypeContract = ::kronika_registry::TypeContract {
            type_id: match ::kronika_registry::TypeId::new(#id) {
                ::core::option::Option::Some(id) => id,
                ::core::option::Option::None => ::core::panic!(
                    "section type_id is invalid: unknown class, or a zero source or version"
                ),
            },
            name: #name,
            semantics: ::kronika_registry::Semantics::#semantics_variant,
            columns: &[ #( #column_entries ),* ],
            sort_key: &[ #( #sort_key ),* ],
            identity: &[ #( #identity ),* ],
            deprecated: false,
        };
    }
}

fn semantics_variant(ident: &Ident) -> Ident {
    let name = ident.to_string();
    let variant = match name.as_str() {
        "snapshot_full" => "SnapshotFull",
        "conditional_full" => "ConditionalFull",
        "event_stream" => "EventStream",
        "changed" => "Changed",
        "on_change" => "OnChange",
        // Leave an unknown name as-is so rustc's error points at the enum.
        other => other,
    };
    Ident::new(variant, ident.span())
}

fn build_encode(columns: &[ColumnDef]) -> TokenStream2 {
    // One builder per column. Collapsing these passes would require columnar
    // input; keep the row-slice API until benchmarks say otherwise.
    let builders = columns.iter().map(|c| {
        let field = &c.field;
        let name = &c.name;
        if c.column_type == "ListI32" {
            return quote! {
                ::kronika_registry::write_list_i32(#name, rows.iter().map(|r| r.#field.clone()))?
            };
        }
        let values = match (&c.wrapper, c.nullable) {
            (None, _) => quote! { rows.iter().map(|r| r.#field) },
            (Some(_), false) => quote! { rows.iter().map(|r| r.#field.0) },
            (Some(_), true) => quote! { rows.iter().map(|r| r.#field.map(|v| v.0)) },
        };
        match (&c.arrow_type, c.nullable) {
            (Some(at), false) => quote! {
                ::kronika_registry::write_required::<::kronika_registry::#at>(#values)
            },
            (Some(at), true) => quote! {
                ::kronika_registry::write_nullable::<::kronika_registry::#at>(#values)
            },
            (None, false) => quote! { ::kronika_registry::write_bool(#values) },
            (None, true) => quote! { ::kronika_registry::write_bool_nullable(#values) },
        }
    });
    quote! { ::std::vec![ #( #builders ),* ] }
}

/// Generate `Section::ts_range` from the non-nullable `#[column(t)]` field.
fn build_ts_range(columns: &[ColumnDef]) -> TokenStream2 {
    columns
        .iter()
        .find(|column| column.column_class == "Timestamp" && !column.nullable)
        .map_or_else(
            || {
                quote! {
                    fn ts_range(_rows: &[Self]) -> ::core::option::Option<(i64, i64)> {
                        ::core::option::Option::None
                    }
                }
            },
            |column| {
                let field = &column.field;
                quote! {
                    fn ts_range(rows: &[Self]) -> ::core::option::Option<(i64, i64)> {
                        let mut values = rows.iter().map(|row| row.#field.0);
                        let first = values.next()?;
                        ::core::option::Option::Some(
                            values.fold((first, first), |(lo, hi), v| (lo.min(v), hi.max(v))),
                        )
                    }
                }
            },
        )
}

fn build_decode(struct_name: &Ident, columns: &[ColumnDef]) -> TokenStream2 {
    // Mixed-site idents avoid collisions with user fields and in-scope tuple
    // structs such as `Ts` and `StrId`.
    let batch = Ident::new("batch", Span::mixed_site());
    let out = Ident::new("out", Span::mixed_site());
    let idx = Ident::new("i", Span::mixed_site());
    let cols: Vec<Ident> = (0..columns.len())
        .map(|n| Ident::new(&format!("col{n}"), Span::mixed_site()))
        .collect();

    let bindings = columns.iter().zip(&cols).map(|(c, col)| {
        let name = &c.name;
        if c.column_type == "ListI32" {
            return quote! { let #col = ::kronika_registry::read_list_i32(#batch, #name)?; };
        }
        match (&c.arrow_type, c.nullable) {
            // Required primitive: rebind to the values slice, so the row loop
            // gathers by `slice[i]` (one bounds-check the optimizer can hoist)
            // instead of `PrimitiveArray::value(i)` per cell.
            (Some(at), false) => quote! {
                let #col = ::kronika_registry::required_column::<::kronika_registry::#at>(#batch, #name)?;
                let #col = #col.values();
            },
            // Nullable arrays stay intact so `opt_primitive` can check nulls.
            (Some(at), true) => quote! {
                let #col = ::kronika_registry::nullable_column::<::kronika_registry::#at>(#batch, #name)?;
            },
            (None, false) => quote! {
                let #col = ::kronika_registry::required_bool(#batch, #name)?;
            },
            (None, true) => quote! {
                let #col = ::kronika_registry::nullable_bool(#batch, #name)?;
            },
        }
    });

    let cells = columns.iter().zip(&cols).map(|(c, col)| {
        let field = &c.field;
        if c.column_type == "ListI32" {
            return quote! { #field: #col.value(#idx) };
        }
        let value = match (&c.wrapper, &c.arrow_type, c.nullable) {
            (Some(w), _, false) => quote! { ::kronika_registry::#w(#col[#idx]) },
            (Some(w), _, true) => quote! {
                ::kronika_registry::opt_primitive(#col, #idx).map(::kronika_registry::#w)
            },
            (None, Some(_), false) => quote! { #col[#idx] },
            (None, None, false) => quote! { #col.value(#idx) },
            (None, Some(_), true) => quote! { ::kronika_registry::opt_primitive(#col, #idx) },
            (None, None, true) => quote! { ::kronika_registry::opt_bool(#col, #idx) },
        };
        quote! { #field: #value }
    });

    quote! {
        ::kronika_registry::decode_section(&Self::CONTRACT, section, |#batch, #out| {
            #( #bindings )*
            for #idx in 0..#batch.num_rows() {
                #out.push(#struct_name { #( #cells ),* });
            }
            ::core::result::Result::Ok(())
        })
    }
}
