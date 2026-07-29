# Single-Root Terminology Cleanup Implementation Plan

Status: `IMPLEMENTED`

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove every current-tree trace of the retired global numeric namespace while preserving the single-root runtime and useful qualification measurements.

**Architecture:** A repository guard assembles the two retired tokens from fragments and scans tracked files without embedding either token in its own source. Rust identifiers are renamed by their actual meaning; stale compatibility assertions and migration records are removed; active contracts and qualification artifacts are normalized to the current single-root format.

**Tech Stack:** Rust 2024 workspace, Python 3 standard library, GitHub Actions, JSON qualification artifacts, Markdown contracts

## Global Constraints

- The current tree must contain none of the tokens assembled as `"source_" + "id"`, `"KRONIKA_SOURCE_" + "ID"`, `"source_" + "identity"`, `"source " + "identity"`, and `"source " + "ID"`.
- Git history is not rewritten.
- No sentinel, alias, compatibility field, migration reader, or old query parameter is added.
- File replacement detection, typed entity hashing, serialization, resource bounds, and runtime behavior remain unchanged.
- Manual edits use `apply_patch`; mechanical renames may use formatter-assisted tooling.
- The final verification includes strict clippy, workspace tests, qualification validators, and dependency checks.

---

### Task 1: Add the Repository Guard

**Files:**
- Create: `scripts/validate-single-root-terminology.py`
- Create: `scripts/test_validate_single_root_terminology.py`
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: `forbidden_terms() -> tuple[str, ...]`
- Produces: `find_matches(root: Path) -> list[Match]`
- Produces: CLI exit code `0` for a clean tree and `1` with `path:line` diagnostics otherwise

- [x] **Step 1: Write guard unit tests**

Create temporary trees with one clean file and one file containing each token assembled in the test from the same fragments. Assert clean input returns no matches, text input reports exact path/line, `.git` and binary files are ignored, and the guard source does not contain either assembled token.

- [x] **Step 2: Run tests to verify RED**

Run:

```bash
python3 -B scripts/test_validate_single_root_terminology.py
```

Observed: the tests failed before the validator implementation existed.

- [x] **Step 3: Implement the validator**

In a Git worktree, stream paths from `git ls-files -z` so only tracked files
participate. In isolated non-Git test trees, walk the filesystem while skipping
`.git`, `target`, symlinks, non-files, and invalid UTF-8. Stream file contents in
bounded chunks, preserve matches across chunk boundaries, cap diagnostics, build
all tokens only at runtime from prefix/suffix fragments, and sort diagnostics by
repository-relative path and line.

- [x] **Step 4: Wire the guard before expensive CI checks**

Add the following lint step before formatting:

```yaml
- name: Validate single-root terminology
  run: python3 -B scripts/validate-single-root-terminology.py
```

- [x] **Step 5: Verify the guard**

Run the unit test, then run the validator against the current repository.

Observed: unit tests pass; the repository validation initially listed the known
legacy files and passes after their removal.

### Task 2: Rename Neutral Rust Identifiers

**Files:**
- Modify: `crates/kronika-layout/src/root.rs`
- Modify: `crates/kronika-writer/src/recovery.rs`
- Modify: `bins/pg_kronika-dump/src/journal.rs`
- Modify: `crates/kronika-analytics/src/overview/metric.rs`
- Modify: `crates/kronika-source-os/src/mount.rs`

**Interfaces:**
- Preserves: `FileIdentity`, `JournalRecovery`, `OvfTemp`, `derive_entity`, and `mount_row` public behavior
- Renames: source-file snapshots to `input_file_identity`
- Renames: entity hash input to `entity_identity_bytes`
- Renames: mount source dictionary reference to `source_str_id`

- [x] **Step 1: Apply semantic identifier renames**

Rename fields, parameters, locals, and their uses. Do not change types, error mapping, hashing domain tags, comparisons, serialized fields, or function signatures beyond parameter names.

- [x] **Step 2: Run focused Rust checks**

Run:

```bash
cargo fmt --all --check
cargo test -p kronika-layout --lib --target aarch64-apple-darwin
cargo test -p kronika-writer --lib --target aarch64-apple-darwin
cargo test -p kronika-analytics --lib --target aarch64-apple-darwin
cargo test -p kronika-source-os --lib --target aarch64-apple-darwin
cargo test -p pg_kronika-dump --target aarch64-apple-darwin
```

Expected: all focused suites pass with unchanged behavior.

### Task 3: Remove Compatibility Assertions And Stale Records

**Files:**
- Delete: the retired migration specification
- Delete: the retired migration implementation plan
- Modify: `bins/pg_kronika-web/src/tests/problems.rs`
- Modify: `bins/pg_kronika-web/src/tests/incidents.rs`
- Modify: `bins/pg_kronika-web/src/tests/overview_timeline.rs`
- Modify: `bins/pg_kronika-dump/tests/dump.rs`
- Modify: active and implemented Markdown files reported by the guard

**Interfaces:**
- Preserves: closed OpenAPI schemas and exact response shapes
- Removes: tests and prose that retain the retired field name only to assert its absence
- Replaces: old global-namespace language with one data root, file descriptors, `node_self_id`, typed entity identity, or contract revision as appropriate

- [x] **Step 1: Replace negative field-name assertions**

Use exact expected property sets or the existing closed-schema assertions. For dump headers, assert the complete supported header property set rather than naming one removed property. For event and incident DTOs, rely on exact/golden response shapes already exercised by the surrounding test.

- [x] **Step 2: Remove completed migration-only documents**

Delete the dedicated migration spec and plan. Update `docs/superpowers/README.md` counts only if necessary after deletion.

- [x] **Step 3: Normalize remaining active contracts**

Remove obsolete clauses from timeline, incident, analysis, Health, and PGM research documents. Preserve only current single-root semantics and use exact modern identifiers where required.

- [x] **Step 4: Normalize or retire implemented records**

For each matched implemented plan/spec, remove obsolete snippets or rewrite the local paragraph to current terminology. Delete a record only when its remaining value is entirely superseded and preserved in Git history.

- [x] **Step 5: Run documentation and Web API checks**

Run:

```bash
python3 -B scripts/validate-single-root-terminology.py
cargo test -p pg_kronika-web --lib --target aarch64-apple-darwin
cargo test -p pg_kronika-dump --target aarch64-apple-darwin
```

Expected: the guard reports only qualification artifacts or validator clauses; both Rust suites pass.

### Task 4: Normalize Qualification And Finish Verification

**Files:**
- Modify: `docs/qualification/pgm-size-reduction-v1.json`
- Modify: `docs/qualification/pgm-coalesced-sections-production-v1.json`
- Modify: `scripts/validate-pgm-size-reduction.py`
- Modify: `docs/superpowers/specs/2026-07-26-pgm-size-reduction-research.md`

**Interfaces:**
- Preserves: size, compression ratio, RSS, CPU, I/O, hashes, determinism, contract inventory, and timestamp-range evidence
- Removes: the obsolete global-namespace axis and its nonzero fixture assertion
- Replaces: identity proof with current catalog structure, contract inventory, normalized dictionary, canonical rows, and timestamp range

- [x] **Step 1: Update the validator contract**

Delete the obsolete fixture field assertion. Keep sorted unique type inventory, registry digests, timestamp ordering, deterministic hash, row counts, and ratio equations.

- [x] **Step 2: Normalize checked-in reports**

Remove obsolete keys from every object and update prose arrays to describe current identity evidence. Do not alter measured byte, RSS, CPU, I/O, hash, row, section, or timestamp values.

- [x] **Step 3: Normalize the research document**

Remove the obsolete identity dimension and describe equality through current catalog metadata, time range, type inventory, canonical rows, and dictionary normalization.

- [x] **Step 4: Run qualification checks**

Run:

```bash
python3 -B scripts/validate-pgm-size-reduction.py
python3 -B scripts/validate-pgm-coalesced-sections.py
python3 -B scripts/test_validate_single_root_terminology.py
python3 -B scripts/validate-single-root-terminology.py
```

Expected: every command passes and the repository guard prints no matches.

- [x] **Step 5: Run repository gates**

Run:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --target aarch64-apple-darwin -- -D warnings
cargo test --workspace --target aarch64-apple-darwin
cargo run -p xtask --target aarch64-apple-darwin -- check-deps
git diff --check
git status --short
```

Observed: formatting, strict clippy, dependency checks, validators, and all
platform-compatible workspace tests pass. Eleven quarantine/recovery tests are
excluded on macOS because the production `rename_noreplace` operation is
Linux-only and intentionally returns `Unsupported` on other platforms. The
unfiltered failures reproduce without this cleanup.

- [x] **Step 6: Commit the cleanup**

```bash
git add -A
git commit -m "refactor: полностью удалить следы прежнего пространства"
```
