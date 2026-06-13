//! `#[derive(Section)]`: generate a section codec from a typed struct.
//!
//! The struct is the single source of truth for one `type_id`. The derive
//! reads each field's Rust type (the on-disk type and nullability) and a
//! `#[column(..)]` class, plus a `#[section(..)]` header, and generates the
//! registry contract and the Parquet encode/decode — every per-type piece
//! `kronika-registry` used to hand-write. The framing and the memory bounds
//! live once in `kronika_registry::codec`; the generated code only supplies
//! one column builder/reader per field.
//!
//! ```ignore
//! #[derive(Section)]
//! #[section(id = 1_006_001, name = "pg_stat_bgwriter", semantics = snapshot_full, sort_key("ts"))]
//! struct BgwriterCheckpointer {
//!     #[column(t)] ts: i64,
//!     #[column(c)] checkpoints_timed: i64,
//!     #[column(c)] buffers_backend: Option<i64>,
//! }
//! ```

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
}

/// One resolved column: its field, on-disk shape, and class.
struct ColumnDef {
    field: Ident,
    name: String,
    /// `ColumnType` variant ident, e.g. `I64`, `Ts`.
    column_type: Ident,
    /// `ColumnClass` variant ident, e.g. `Cumulative`.
    column_class: Ident,
    /// Arrow primitive type token for the runtime helpers, or `None` for
    /// `bool` (which uses the dedicated boolean helpers).
    arrow_type: Option<Ident>,
    nullable: bool,
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

    Ok(quote! {
        impl #struct_name {
            #contract

            #[doc = "Encode rows into a Parquet section body."]
            ///
            /// # Errors
            ///
            /// A `kronika_registry::CodecError` if a row cap is exceeded or
            /// Parquet writing fails.
            pub fn encode(rows: &[Self]) -> ::core::result::Result<
                ::std::vec::Vec<u8>,
                ::kronika_registry::CodecError,
            > {
                let columns = #encode;
                ::kronika_registry::encode_section(&Self::CONTRACT, rows.len(), columns)
            }

            #[doc = "Decode a Parquet section body back into rows."]
            ///
            /// # Errors
            ///
            /// A `kronika_registry::CodecError` if a memory bound is exceeded,
            /// the Parquet is malformed, or the file does not match the
            /// contract.
            pub fn decode(bytes: &[u8]) -> ::core::result::Result<
                ::std::vec::Vec<Self>,
                ::kronika_registry::CodecError,
            > {
                #decode
            }
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
    let class_ident: Ident = class_attr.parse_args()?;
    let column_class = column_class(&class_ident)?;

    let (inner, nullable) = unwrap_option(&field.ty);
    let inner_ident = type_ident(inner)?;
    let (column_type, arrow_type) = map_type(&inner_ident, &column_class)?;

    Ok(ColumnDef {
        name: field_ident.to_string(),
        field: field_ident,
        column_type,
        column_class,
        arrow_type,
        nullable,
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

/// Map a base-type ident and class to a `ColumnType` variant and the Arrow
/// primitive type token (or `None` for `bool`).
fn map_type(ident: &Ident, class: &Ident) -> syn::Result<(Ident, Option<Ident>)> {
    let span = ident.span();
    let is_timestamp = class == "Timestamp";

    let (column_type, arrow_type): (&str, Option<&str>) = match ident.to_string().as_str() {
        "i8" => ("I8", Some("Int8Type")),
        "i16" => ("I16", Some("Int16Type")),
        "i32" => ("I32", Some("Int32Type")),
        "i64" if is_timestamp => ("Ts", Some("Int64Type")),
        "i64" => ("I64", Some("Int64Type")),
        "u8" => ("U8", Some("UInt8Type")),
        "u16" => ("U16", Some("UInt16Type")),
        "u32" => ("U32", Some("UInt32Type")),
        "u64" => ("U64", Some("UInt64Type")),
        "f32" => ("F32", Some("Float32Type")),
        "f64" => ("F64", Some("Float64Type")),
        "bool" => ("Bool", None),
        other => {
            return Err(syn::Error::new(
                span,
                format!(
                    "unsupported column type `{other}`; expected a base type like i64, u32, f64, bool"
                ),
            ));
        }
    };

    if is_timestamp && column_type != "Ts" {
        return Err(syn::Error::new(
            span,
            "a column of class t (timestamp) must be an i64",
        ));
    }

    Ok((
        Ident::new(column_type, span),
        arrow_type.map(|at| Ident::new(at, span)),
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

    let column_entries = columns.iter().map(|c| {
        let name = &c.name;
        let ty = &c.column_type;
        let class = &c.column_class;
        let nullable = c.nullable;
        quote! {
            ::kronika_registry::Column {
                name: #name,
                ty: ::kronika_registry::ColumnType::#ty,
                class: ::kronika_registry::ColumnClass::#class,
                nullable: #nullable,
            }
        }
    });

    quote! {
        #[doc = "The registry contract for this type."]
        pub const CONTRACT: ::kronika_registry::TypeContract = ::kronika_registry::TypeContract {
            type_id: ::kronika_registry::TypeId::declared(#id),
            name: #name,
            semantics: ::kronika_registry::Semantics::#semantics_variant,
            columns: &[ #( #column_entries ),* ],
            sort_key: &[ #( #sort_key ),* ],
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
    let builders = columns.iter().map(|c| {
        let field = &c.field;
        let values = quote! { rows.iter().map(|r| r.#field) };
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

fn build_decode(struct_name: &Ident, columns: &[ColumnDef]) -> TokenStream2 {
    // The closure params and loop variable use mixed-site hygiene so a field
    // named `batch`, `out`, or `i` cannot collide with them: a derive's
    // call-site identifiers would otherwise be the *same* identifier as the
    // user's field of that name.
    let batch = Ident::new("batch", Span::mixed_site());
    let out = Ident::new("out", Span::mixed_site());
    let idx = Ident::new("i", Span::mixed_site());

    let bindings = columns.iter().map(|c| {
        let field = &c.field;
        let name = &c.name;
        match (&c.arrow_type, c.nullable) {
            (Some(at), false) => quote! {
                let #field = ::kronika_registry::required_column::<::kronika_registry::#at>(#batch, #name)?;
            },
            (Some(at), true) => quote! {
                let #field = ::kronika_registry::nullable_column::<::kronika_registry::#at>(#batch, #name)?;
            },
            (None, false) => quote! {
                let #field = ::kronika_registry::required_bool(#batch, #name)?;
            },
            (None, true) => quote! {
                let #field = ::kronika_registry::nullable_bool(#batch, #name)?;
            },
        }
    });

    let cells = columns.iter().map(|c| {
        let field = &c.field;
        let value = match (&c.arrow_type, c.nullable) {
            (Some(_) | None, false) => quote! { #field.value(#idx) },
            (Some(_), true) => quote! { ::kronika_registry::opt_primitive(#field, #idx) },
            (None, true) => quote! { ::kronika_registry::opt_bool(#field, #idx) },
        };
        quote! { #field: #value }
    });

    quote! {
        ::kronika_registry::decode_section(bytes, |#batch, #out| {
            #( #bindings )*
            for #idx in 0..#batch.num_rows() {
                #out.push(#struct_name { #( #cells ),* });
            }
            ::core::result::Result::Ok(())
        })
    }
}
