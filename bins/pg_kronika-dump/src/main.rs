//! Reserved entry point for command-line PGM diagnostics.
//!
//! No inspect, verify, extract, or comparison commands are implemented. The
//! placeholder prints an error and exits with status 2.

fn main() {
    eprintln!("pg_kronika-dump: not implemented yet, see docs/plan.md");
    std::process::exit(2);
}
