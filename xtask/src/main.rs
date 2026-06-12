//! Workspace maintenance tasks.
//!
//! `cargo run -p xtask -- check-deps` enforces the binary dependency rules
//! from `docs/architecture.md` ("Workspace layout"): every internal crate
//! reachable from a binary must be in that binary's allow list. The rules
//! exist so that S3 code and cloud credentials never reach the privileged
//! collector, the `PostgreSQL` client never reaches the web process, and the
//! archiver does not need a rebuild when data types are added.

use std::collections::{BTreeMap, BTreeSet};
use std::process::{Command, ExitCode};

/// Allowed internal (workspace) crates per binary, including transitive
/// dependencies. Keep in sync with `docs/architecture.md`; a change here
/// is an architectural decision, not a build fix.
const RULES: &[(&str, &[&str])] = &[
    (
        "pg_kronika-collector",
        // No store-*: S3 and remote-read code must not enter the
        // privileged process. No reader: the collector only writes.
        &[
            "kronika-format",
            "kronika-derive",
            "kronika-registry",
            "kronika-writer",
            "kronika-charts",
            "kronika-source-pg",
            "kronika-source-os",
            "kronika-source-log",
        ],
    ),
    (
        "pg_kronika-web",
        // No source-*: the PostgreSQL client and /proc readers must not
        // enter the web process.
        &[
            "kronika-format",
            "kronika-derive",
            "kronika-registry",
            "kronika-reader",
            "kronika-diff",
            "kronika-charts",
            "kronika-store",
            "kronika-store-http",
            "kronika-store-s3",
        ],
    ),
    (
        "pg_kronika-archiver",
        // No registry: the archiver checks only the container and must not
        // need a rebuild when data types are added.
        &["kronika-format", "kronika-store", "kronika-store-s3"],
    ),
    (
        "pg_kronika-dump",
        &[
            "kronika-format",
            "kronika-derive",
            "kronika-registry",
            "kronika-reader",
            "kronika-diff",
            "kronika-charts",
            "kronika-store",
            "kronika-store-http",
            "kronika-store-s3",
        ],
    ),
];

fn main() -> ExitCode {
    if std::env::args().nth(1).as_deref() == Some("check-deps") {
        check_deps()
    } else {
        eprintln!("usage: cargo run -p xtask -- check-deps");
        ExitCode::from(2)
    }
}

/// Internal dependency graph: workspace crate name -> direct workspace deps.
fn workspace_graph() -> BTreeMap<String, BTreeSet<String>> {
    let output = Command::new(env!("CARGO"))
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .expect("failed to run `cargo metadata`");
    assert!(
        output.status.success(),
        "`cargo metadata` failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let meta: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("invalid `cargo metadata` JSON");

    let packages = meta["packages"].as_array().expect("packages array");
    let names: BTreeSet<String> = packages
        .iter()
        .map(|p| p["name"].as_str().expect("package name").to_owned())
        .collect();

    packages
        .iter()
        .map(|p| {
            let name = p["name"].as_str().expect("package name").to_owned();
            let deps = p["dependencies"]
                .as_array()
                .expect("dependencies array")
                .iter()
                .map(|d| d["name"].as_str().expect("dependency name").to_owned())
                .filter(|d| names.contains(d))
                .collect();
            (name, deps)
        })
        .collect()
}

/// All workspace crates reachable from `start`, excluding `start` itself.
fn reachable(graph: &BTreeMap<String, BTreeSet<String>>, start: &str) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut stack: Vec<&str> = graph[start].iter().map(String::as_str).collect();
    while let Some(name) = stack.pop() {
        if seen.insert(name.to_owned()) {
            stack.extend(graph[name].iter().map(String::as_str));
        }
    }
    seen
}

fn check_deps() -> ExitCode {
    let graph = workspace_graph();
    let mut violations = Vec::new();

    for (bin, allowed) in RULES {
        assert!(
            graph.contains_key(*bin),
            "binary `{bin}` from the rules table is not in the workspace"
        );
        let allowed: BTreeSet<&str> = allowed.iter().copied().collect();
        for dep in reachable(&graph, bin) {
            if dep != "xtask" && !allowed.contains(dep.as_str()) {
                violations.push(format!(
                    "{bin}: depends on `{dep}`, which is not in its allow list"
                ));
            }
        }
    }

    if violations.is_empty() {
        println!("check-deps: ok ({} binaries checked)", RULES.len());
        ExitCode::SUCCESS
    } else {
        eprintln!("dependency rules from docs/architecture.md are violated:");
        for v in &violations {
            eprintln!("  {v}");
        }
        ExitCode::FAILURE
    }
}
