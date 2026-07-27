//! Command-line entry point for offline `PgKronika` data inspection.
#![allow(
    unused_crate_dependencies,
    reason = "this binary consumes the pg_kronika_dump library; the package dependencies belong to the library and its tests"
)]

fn main() -> std::process::ExitCode {
    pg_kronika_dump::run(
        std::env::args_os().skip(1),
        std::io::stdout().lock(),
        std::io::stderr().lock(),
    )
}
