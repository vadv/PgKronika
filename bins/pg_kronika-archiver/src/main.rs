//! Reserved entry point for remote archival.
//!
//! No archival backend, configuration, upload, retry, or deletion behavior is
//! implemented. The placeholder prints an error and exits with status 2.

fn main() {
    eprintln!("pg_kronika-archiver: not implemented yet, see docs/plan.md");
    std::process::exit(2);
}
