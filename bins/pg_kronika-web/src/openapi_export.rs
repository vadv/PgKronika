//! Deterministic multi-file export of the runtime OpenAPI document.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs;
use std::io;
use std::path::Path;

use serde_json::{Map, Value};
use utoipa::openapi::OpenApi;

const DOMAINS: &[&str] = &["core", "sections", "analytics", "timeline", "ui"];
const HTTP_METHODS: &[&str] = &[
    "get", "put", "post", "delete", "options", "head", "patch", "trace",
];
const GENERATED_README: &str = "# Generated OpenAPI

This directory is generated from the Rust handlers and response schemas used by
`pg_kronika-web`.

Do not edit these files manually. Run `make openapi` from the repository root
to regenerate and round-trip-check the tree. The entry point is
`openapi.yaml`; path and schema fragments use standard relative `$ref` values.
Run `make openapi-bundle` when a tool requires one self-contained YAML file.
";

type DynError = Box<dyn Error + Send + Sync + 'static>;

/// Export a tool-neutral YAML tree and verify that it reconstructs the runtime
/// document exactly before replacing the destination directory.
pub fn export_multifile(document: &OpenApi, destination: &Path) -> Result<(), DynError> {
    let parent = destination.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("openapi");
    let temporary = parent.join(format!(".{name}.tmp-{}", std::process::id()));
    if temporary.exists() {
        fs::remove_dir_all(&temporary)?;
    }

    let original = serde_json::to_value(document)?;
    let result = export_into(&original, &temporary)
        .and_then(|()| reconstruct(&temporary))
        .and_then(|reconstructed| {
            if reconstructed == original {
                Ok(())
            } else {
                Err(invalid_data("multi-file OpenAPI round-trip changed the document").into())
            }
        });
    if let Err(error) = result {
        drop(fs::remove_dir_all(&temporary));
        return Err(error);
    }

    let backup = parent.join(format!(".{name}.backup-{}", std::process::id()));
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    if destination.exists() {
        fs::rename(destination, &backup)?;
    }
    if let Err(error) = fs::rename(&temporary, destination) {
        if backup.exists() {
            fs::rename(&backup, destination)?;
        }
        drop(fs::remove_dir_all(&temporary));
        return Err(error.into());
    }
    if backup.exists() {
        fs::remove_dir_all(backup)?;
    }
    Ok(())
}

fn export_into(original: &Value, destination: &Path) -> Result<(), DynError> {
    validate_source_contract(original)?;
    let paths = object_at(original, &["paths"])?;
    let schemas = object_at(original, &["components", "schemas"])?;
    let mut paths_by_domain: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    let mut owners: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();

    for (path, item) in paths {
        let domain = operation_domain(item)?;
        paths_by_domain
            .entry(domain.clone())
            .or_default()
            .insert(path.clone(), item.clone());
        mark_schema_owners(item, schemas, &domain, &mut owners)?;
    }
    for schema in schemas.keys() {
        owners.entry(schema.clone()).or_default();
    }

    let schema_group = owners
        .into_iter()
        .map(|(name, domains)| {
            let group = if domains.len() == 1 {
                domains
                    .into_iter()
                    .next()
                    .expect("one schema owner after length check")
            } else {
                "common".to_owned()
            };
            (name, group)
        })
        .collect::<BTreeMap<_, _>>();

    fs::create_dir_all(destination.join("paths"))?;
    fs::create_dir_all(destination.join("schemas"))?;

    for domain in DOMAINS {
        let mut document = Value::Object(paths_by_domain.remove(*domain).unwrap_or_default());
        rewrite_schema_refs(&mut document, |name| {
            let group = schema_group
                .get(name)
                .expect("referenced schemas were validated");
            format!("../schemas/{group}.yaml#/{}", pointer_token(name))
        });
        write_yaml(
            &destination.join("paths").join(format!("{domain}.yaml")),
            &document,
        )?;
    }

    let mut schemas_by_group: BTreeMap<String, Map<String, Value>> = BTreeMap::new();
    for (name, schema) in schemas {
        let group = schema_group
            .get(name)
            .expect("every component schema has an assigned group");
        schemas_by_group
            .entry(group.clone())
            .or_default()
            .insert(name.clone(), schema.clone());
    }
    for group in DOMAINS.iter().copied().chain(std::iter::once("common")) {
        let mut document = Value::Object(schemas_by_group.remove(group).unwrap_or_default());
        rewrite_schema_refs(&mut document, |name| {
            let target = schema_group
                .get(name)
                .expect("referenced schemas were validated");
            if target == group {
                format!("#/{}", pointer_token(name))
            } else {
                format!("{target}.yaml#/{}", pointer_token(name))
            }
        });
        write_yaml(
            &destination.join("schemas").join(format!("{group}.yaml")),
            &document,
        )?;
    }

    let root = root_document(original, paths, schemas, &schema_group)?;
    write_yaml(&destination.join("openapi.yaml"), &root)?;
    fs::write(destination.join("README.md"), GENERATED_README)?;
    Ok(())
}

#[allow(
    clippy::too_many_lines,
    reason = "the exporter validates one complete OpenAPI operation and reachability contract"
)]
fn validate_source_contract(document: &Value) -> Result<(), DynError> {
    let paths = object_at(document, &["paths"])?;
    let schemas = object_at(document, &["components", "schemas"])?;
    let mut operation_ids = BTreeSet::new();

    if paths.contains_key("/v1/sources") {
        return Err(invalid_data("removed HTTP source route is present: /v1/sources").into());
    }
    if document.to_string().contains("unknown_source") {
        return Err(invalid_data("removed HTTP source error is present: unknown_source").into());
    }

    for (path, path_item) in paths {
        let operations = path_item
            .as_object()
            .ok_or_else(|| invalid_data(format!("OpenAPI path item is not an object: {path}")))?;
        let mut operation_count = 0_usize;
        for (method, operation_value) in operations {
            if !HTTP_METHODS.contains(&method.as_str()) {
                continue;
            }
            operation_count += 1;
            let operation = operation_value.as_object().ok_or_else(|| {
                invalid_data(format!(
                    "OpenAPI operation is not an object: {method} {path}"
                ))
            })?;
            let operation_id = operation
                .get("operationId")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    invalid_data(format!(
                        "OpenAPI operation has no operationId: {method} {path}"
                    ))
                })?;
            if !operation_ids.insert(operation_id.to_owned()) {
                return Err(
                    invalid_data(format!("duplicate OpenAPI operationId: {operation_id}")).into(),
                );
            }
            if operation
                .get("parameters")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .any(|parameter| {
                    parameter.get("in").and_then(Value::as_str) == Some("query")
                        && parameter.get("name").and_then(Value::as_str) == Some("source")
                })
            {
                return Err(invalid_data(format!(
                    "removed HTTP source query parameter is present: {operation_id}"
                ))
                .into());
            }

            let tags = operation
                .get("tags")
                .and_then(Value::as_array)
                .ok_or_else(|| {
                    invalid_data(format!("{operation_id} must have exactly one domain tag"))
                })?;
            let [tag] = tags.as_slice() else {
                return Err(invalid_data(format!(
                    "{operation_id} must have exactly one domain tag"
                ))
                .into());
            };
            let tag = tag.as_str().ok_or_else(|| {
                invalid_data(format!("{operation_id} domain tag must be a string"))
            })?;
            if !DOMAINS.contains(&tag) {
                return Err(invalid_data(format!(
                    "unsupported OpenAPI domain tag on {operation_id}: {tag}"
                ))
                .into());
            }

            let success_ref = operation_value
                .pointer("/responses/200/content/application~1json/schema/$ref")
                .and_then(Value::as_str)
                .and_then(|reference| reference.strip_prefix("#/components/schemas/"))
                .map(pointer_unescape)
                .ok_or_else(|| {
                    invalid_data(format!("{operation_id} must have a named JSON 200 schema"))
                })?;
            if !schemas.contains_key(&success_ref) {
                return Err(
                    invalid_data(format!("missing referenced schema: {success_ref}")).into(),
                );
            }
        }
        if operation_count == 0 {
            return Err(invalid_data(format!("OpenAPI path has no operation: {path}")).into());
        }
    }

    for name in schema_refs(document) {
        if !schemas.contains_key(&name) {
            return Err(invalid_data(format!("missing referenced schema: {name}")).into());
        }
    }

    let mut pending = paths
        .values()
        .flat_map(schema_refs)
        .collect::<BTreeSet<_>>();
    let mut reachable = BTreeSet::new();
    while let Some(name) = pending.pop_first() {
        if !reachable.insert(name.clone()) {
            continue;
        }
        pending.extend(schema_refs(
            schemas
                .get(&name)
                .expect("all schema references were validated"),
        ));
    }
    if let Some(unused) = schemas.keys().find(|name| !reachable.contains(*name)) {
        return Err(invalid_data(format!("unreachable component schema: {unused}")).into());
    }
    Ok(())
}

fn root_document(
    original: &Value,
    paths: &Map<String, Value>,
    schemas: &Map<String, Value>,
    schema_group: &BTreeMap<String, String>,
) -> Result<Value, DynError> {
    let mut root = original.clone();
    let root_object = root
        .as_object_mut()
        .ok_or_else(|| invalid_data("OpenAPI root must be an object"))?;
    root_object.insert(
        "paths".to_owned(),
        Value::Object(
            paths
                .iter()
                .map(|(path, item)| {
                    let domain = operation_domain(item)?;
                    Ok((
                        path.clone(),
                        serde_json::json!({
                            "$ref": format!("paths/{domain}.yaml#/{}", pointer_token(path))
                        }),
                    ))
                })
                .collect::<Result<_, DynError>>()?,
        ),
    );
    let components = root_object
        .get_mut("components")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| invalid_data("OpenAPI components must be an object"))?;
    components.insert(
        "schemas".to_owned(),
        Value::Object(
            schemas
                .keys()
                .map(|name| {
                    let group = schema_group
                        .get(name)
                        .expect("every component schema has an assigned group");
                    (
                        name.clone(),
                        serde_json::json!({
                            "$ref": format!("schemas/{group}.yaml#/{}", pointer_token(name))
                        }),
                    )
                })
                .collect(),
        ),
    );
    Ok(root)
}

fn operation_domain(path_item: &Value) -> Result<String, DynError> {
    let operations = path_item
        .as_object()
        .ok_or_else(|| invalid_data("OpenAPI path item must be an object"))?;
    let mut tags = operations
        .values()
        .filter_map(|operation| operation.get("tags"))
        .filter_map(Value::as_array)
        .filter_map(|tags| tags.first())
        .filter_map(Value::as_str);
    let domain = tags
        .next()
        .ok_or_else(|| invalid_data("every exported path must have one domain tag"))?;
    if !DOMAINS.contains(&domain) {
        return Err(invalid_data(format!("unsupported OpenAPI domain tag: {domain}")).into());
    }
    if tags.any(|tag| tag != domain) {
        return Err(invalid_data("all operations in one path must use the same domain tag").into());
    }
    Ok(domain.to_owned())
}

fn mark_schema_owners(
    value: &Value,
    schemas: &Map<String, Value>,
    domain: &str,
    owners: &mut BTreeMap<String, BTreeSet<String>>,
) -> Result<(), DynError> {
    let mut pending = schema_refs(value);
    let mut visited = BTreeSet::new();
    while let Some(name) = pending.pop_first() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let schema = schemas
            .get(&name)
            .ok_or_else(|| invalid_data(format!("missing referenced schema: {name}")))?;
        owners.entry(name).or_default().insert(domain.to_owned());
        pending.extend(schema_refs(schema));
    }
    Ok(())
}

fn schema_refs(value: &Value) -> BTreeSet<String> {
    let mut refs = BTreeSet::new();
    visit(value, &mut |candidate| {
        if let Some(name) = candidate.strip_prefix("#/components/schemas/") {
            refs.insert(pointer_unescape(name));
        }
    });
    refs
}

fn rewrite_schema_refs(value: &mut Value, reference: impl Fn(&str) -> String + Copy) {
    match value {
        Value::Array(items) => {
            for item in items {
                rewrite_schema_refs(item, reference);
            }
        }
        Value::Object(object) => {
            if let Some(Value::String(current)) = object.get_mut("$ref")
                && let Some(name) = current.strip_prefix("#/components/schemas/")
            {
                *current = reference(&pointer_unescape(name));
            }
            for child in object.values_mut() {
                rewrite_schema_refs(child, reference);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn restore_schema_refs(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                restore_schema_refs(item);
            }
        }
        Value::Object(object) => {
            if let Some(Value::String(current)) = object.get_mut("$ref")
                && let Some(fragment) = current.rsplit('#').next()
                && let Some(name) = fragment.strip_prefix('/')
            {
                *current = format!("#/components/schemas/{}", pointer_unescape(name));
            }
            for child in object.values_mut() {
                restore_schema_refs(child);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn reconstruct(directory: &Path) -> Result<Value, DynError> {
    let mut root = read_yaml(&directory.join("openapi.yaml"))?;
    let path_refs = object_at(&root, &["paths"])?
        .iter()
        .map(|(path, reference)| Ok((path.clone(), ref_text(reference)?.to_owned())))
        .collect::<Result<Vec<_>, DynError>>()?;
    let mut restored_paths = Map::new();
    for (path, reference) in path_refs {
        let mut item = resolve_top_level_ref(directory, &reference)?;
        restore_schema_refs(&mut item);
        restored_paths.insert(path, item);
    }
    root.as_object_mut()
        .expect("validated root object")
        .insert("paths".to_owned(), Value::Object(restored_paths));

    let schema_refs = object_at(&root, &["components", "schemas"])?
        .iter()
        .map(|(name, reference)| Ok((name.clone(), ref_text(reference)?.to_owned())))
        .collect::<Result<Vec<_>, DynError>>()?;
    let mut restored_schemas = Map::new();
    for (name, reference) in schema_refs {
        let mut schema = resolve_top_level_ref(directory, &reference)?;
        restore_schema_refs(&mut schema);
        restored_schemas.insert(name, schema);
    }
    root.pointer_mut("/components/schemas")
        .ok_or_else(|| invalid_data("generated root is missing components.schemas"))?
        .clone_from(&Value::Object(restored_schemas));
    Ok(root)
}

fn resolve_top_level_ref(directory: &Path, reference: &str) -> Result<Value, DynError> {
    let (relative, fragment) = reference
        .split_once('#')
        .ok_or_else(|| invalid_data(format!("external ref has no fragment: {reference}")))?;
    let key = fragment
        .strip_prefix('/')
        .ok_or_else(|| {
            invalid_data(format!(
                "external ref is not a top-level pointer: {reference}"
            ))
        })
        .map(pointer_unescape)?;
    let document = read_yaml(&directory.join(relative))?;
    object_at(&document, &[])?
        .get(&key)
        .cloned()
        .ok_or_else(|| invalid_data(format!("external ref target is missing: {reference}")).into())
}

fn ref_text(value: &Value) -> Result<&str, DynError> {
    value
        .get("$ref")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_data("generated root entry must contain $ref").into())
}

fn object_at<'a>(value: &'a Value, path: &[&str]) -> Result<&'a Map<String, Value>, DynError> {
    let mut current = value;
    for segment in path {
        current = current
            .get(*segment)
            .ok_or_else(|| invalid_data(format!("missing OpenAPI field: {}", path.join("."))))?;
    }
    current.as_object().ok_or_else(|| {
        invalid_data(format!(
            "OpenAPI field is not an object: {}",
            path.join(".")
        ))
        .into()
    })
}

fn visit(value: &Value, visitor: &mut impl FnMut(&str)) {
    match value {
        Value::Array(items) => {
            for item in items {
                visit(item, visitor);
            }
        }
        Value::Object(object) => {
            if let Some(reference) = object.get("$ref").and_then(Value::as_str) {
                visitor(reference);
            }
            for child in object.values() {
                visit(child, visitor);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn write_yaml(path: &Path, value: &Value) -> Result<(), DynError> {
    let mut yaml = serde_norway::to_string(value)?;
    if !yaml.ends_with('\n') {
        yaml.push('\n');
    }
    fs::write(path, yaml)?;
    Ok(())
}

fn read_yaml(path: &Path) -> Result<Value, DynError> {
    Ok(serde_norway::from_str(&fs::read_to_string(path)?)?)
}

fn pointer_token(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn pointer_unescape(value: &str) -> String {
    value.replace("~1", "/").replace("~0", "~")
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[cfg(test)]
mod tests {
    use super::{export_multifile, validate_source_contract};

    #[test]
    fn generated_tree_round_trips_and_uses_relative_refs() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let destination = temporary.path().join("openapi");

        export_multifile(&crate::openapi_document(), &destination).expect("export OpenAPI");

        for relative in [
            "openapi.yaml",
            "paths/core.yaml",
            "paths/sections.yaml",
            "paths/analytics.yaml",
            "paths/timeline.yaml",
            "paths/ui.yaml",
            "schemas/common.yaml",
            "schemas/core.yaml",
            "schemas/sections.yaml",
            "schemas/analytics.yaml",
            "schemas/timeline.yaml",
            "schemas/ui.yaml",
            "README.md",
        ] {
            assert!(destination.join(relative).is_file(), "missing {relative}");
        }
        let root = std::fs::read_to_string(destination.join("openapi.yaml")).expect("read root");
        assert!(
            root.contains("paths/core.yaml#/~1v1~1version"),
            "root uses a standard escaped JSON Pointer"
        );
        let paths =
            std::fs::read_to_string(destination.join("paths/sections.yaml")).expect("read paths");
        assert!(
            paths.contains("../schemas/"),
            "path files use relative schema refs"
        );
        let readme = std::fs::read_to_string(destination.join("README.md")).expect("read README");
        assert!(readme.contains("Do not edit"));
        assert!(readme.contains("make openapi"));
    }

    #[test]
    fn source_contract_rejects_ambiguous_or_incomplete_operations() {
        let mut document = minimal_document();
        document["paths"]["/v1/example"]["get"]["tags"] = serde_json::json!(["core", "sections"]);
        assert!(
            validate_source_contract(&document)
                .expect_err("multiple tags must fail")
                .to_string()
                .contains("exactly one domain tag")
        );

        let mut document = minimal_document();
        document["paths"]["/v1/duplicate"] = document["paths"]["/v1/example"].clone();
        assert!(
            validate_source_contract(&document)
                .expect_err("duplicate operationId must fail")
                .to_string()
                .contains("duplicate OpenAPI operationId")
        );

        let mut document = minimal_document();
        document["paths"]["/v1/example"]["get"]["responses"]["200"]["content"]["application/json"]
            .as_object_mut()
            .expect("media type object")
            .remove("schema");
        assert!(
            validate_source_contract(&document)
                .expect_err("missing success schema must fail")
                .to_string()
                .contains("named JSON 200 schema")
        );

        let mut document = minimal_document();
        document["paths"]["/v1/example"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"] = serde_json::json!("#/components/schemas/Missing");
        assert!(
            validate_source_contract(&document)
                .expect_err("unresolved schema must fail")
                .to_string()
                .contains("missing referenced schema")
        );

        let mut document = minimal_document();
        document["components"]["schemas"]["Unused"] = serde_json::json!({"type": "string"});
        assert!(
            validate_source_contract(&document)
                .expect_err("unused schema must fail")
                .to_string()
                .contains("unreachable component schema")
        );

        let mut document = minimal_document();
        document["paths"]["/v1/sources"] = document["paths"]["/v1/example"].clone();
        document["paths"]["/v1/sources"]["get"]["operationId"] = serde_json::json!("sources");
        assert!(
            validate_source_contract(&document)
                .expect_err("removed source route must fail")
                .to_string()
                .contains("removed HTTP source route")
        );

        let mut document = minimal_document();
        document["paths"]["/v1/example"]["get"]["parameters"] = serde_json::json!([{
            "name": "source",
            "in": "query",
            "schema": { "type": "string" }
        }]);
        assert!(
            validate_source_contract(&document)
                .expect_err("removed source parameter must fail")
                .to_string()
                .contains("removed HTTP source query parameter")
        );

        let mut document = minimal_document();
        document["components"]["schemas"]["Example"]["description"] =
            serde_json::json!("unknown_source");
        assert!(
            validate_source_contract(&document)
                .expect_err("removed source error must fail")
                .to_string()
                .contains("removed HTTP source error")
        );
    }

    #[test]
    fn failed_export_preserves_the_existing_tree() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let destination = temporary.path().join("openapi");
        std::fs::create_dir(&destination).expect("create existing tree");
        std::fs::write(destination.join("sentinel"), "keep").expect("write sentinel");
        let mut value = minimal_document();
        value["paths"]["/v1/example"]["get"]["tags"] = serde_json::json!(["core", "sections"]);
        let document = serde_json::from_value(value).expect("deserialize OpenAPI fixture");

        export_multifile(&document, &destination).expect_err("invalid export must fail");

        assert_eq!(
            std::fs::read_to_string(destination.join("sentinel")).expect("read sentinel"),
            "keep"
        );
    }

    fn minimal_document() -> serde_json::Value {
        serde_json::json!({
            "openapi": "3.1.0",
            "info": {
                "title": "fixture",
                "version": "1"
            },
            "paths": {
                "/v1/example": {
                    "get": {
                        "operationId": "example",
                        "tags": ["core"],
                        "responses": {
                            "200": {
                                "description": "OK",
                                "content": {
                                    "application/json": {
                                        "schema": {
                                            "$ref": "#/components/schemas/Example"
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            },
            "components": {
                "schemas": {
                    "Example": {
                        "type": "object"
                    }
                }
            }
        })
    }
}
