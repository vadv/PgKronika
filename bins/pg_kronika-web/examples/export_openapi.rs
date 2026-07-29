//! Write the generated `OpenAPI` document as a verified multi-file YAML tree.
#![allow(
    unused_crate_dependencies,
    reason = "the exporter consumes the web library; package dependencies belong to that library"
)]

use std::env;
use std::fs;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(first) = arguments.next() else {
        eprintln!("usage: export_openapi <output-directory> | --bundle <openapi.yaml>");
        return ExitCode::FAILURE;
    };
    let document = pg_kronika_web::openapi_document();
    if first == "--bundle" {
        let Some(path) = arguments.next() else {
            eprintln!("usage: export_openapi --bundle <openapi.yaml>");
            return ExitCode::FAILURE;
        };
        let yaml = match document.to_yaml() {
            Ok(yaml) => yaml,
            Err(error) => {
                eprintln!("failed to serialize bundled OpenAPI: {error}");
                return ExitCode::FAILURE;
            }
        };
        if let Err(error) = fs::write(&path, yaml) {
            eprintln!(
                "failed to write bundled OpenAPI to {}: {error}",
                path.to_string_lossy()
            );
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    if arguments.next().is_some() {
        eprintln!("usage: export_openapi <output-directory> | --bundle <openapi.yaml>");
        return ExitCode::FAILURE;
    }
    if let Err(error) =
        pg_kronika_web::openapi_export::export_multifile(&document, std::path::Path::new(&first))
    {
        eprintln!(
            "failed to export OpenAPI to {}: {error}",
            first.to_string_lossy()
        );
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
