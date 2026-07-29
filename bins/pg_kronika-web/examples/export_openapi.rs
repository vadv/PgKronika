//! Write the generated `OpenAPI` document as YAML.
#![allow(
    unused_crate_dependencies,
    reason = "the exporter consumes the web library; package dependencies belong to that library"
)]

use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let Some(path) = env::args_os().nth(1) else {
        eprintln!("usage: export_openapi <swagger.yaml>");
        return ExitCode::FAILURE;
    };
    let document = pg_kronika_web::openapi_document();
    let yaml = match document.to_yaml() {
        Ok(yaml) => yaml,
        Err(error) => {
            eprintln!("failed to serialize OpenAPI: {error}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = fs::write(&path, yaml) {
        eprintln!("failed to write {}: {error}", path.to_string_lossy());
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
