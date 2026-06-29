# PgKronika agent instructions

## Standing Review Rule: Memory Bounds

Every diff review, manual or automated, must include a memory-bounds check.
The collector runs on the database host; an out-of-memory failure there is
worse than a lost segment.

For each new or changed code path the review must answer:

1. **What is the peak memory, and what enforces the bound?** Acceptable
   bounds are a config limit (`DictLimits`, `JournalLimits`,
   `JournalConfig::max_journal_len`), a format constant, or the size of an
   input the caller already holds. "Usually small" is not a bound.
2. **Is anything sized by the outside world materialized whole?** Reading an
   entire file, journal, or section into memory when its size is controlled
   by external input is forbidden — stream with a fixed-size buffer instead
   (see `Journal::open` recovery as the reference pattern).
3. **Does any structure grow without an enforced limit?** Unbounded growth
   is allowed only with an enforced cap and a typed overflow signal that
   tells the caller what to do (reference: `JournalError::Full` → merge
   early and reset).
4. **Are doc claims about memory accurate?** If a doc says "one part in
   memory", the review must check that this is literally true, including
   per-item directories, clones, and growth slack.

Independent review workflows must include a dedicated memory-bounds pass
alongside bugs, spec, and tests. Verify findings with allocation counting or
malformed inputs where practical.

## Standing Review Rule: Comment Quality

Every diff review and pre-commit pass must include a comment-quality check,
with the same standing as the memory-bounds check. Apply the local
`code-comments` rules whenever writing or reviewing comments. A comment that
only restates the code is a defect to delete, not decoration to keep.

For every comment and doc-comment in the diff the review must answer:

1. **Does it say something the code does not?** A comment that paraphrases the
   next line is the *what* — delete it. A comment earns its place only by
   carrying what the code cannot: rationale, an invariant the types don't
   enforce, a trade-off, a footgun, or a pointer to the contract.
2. **Will it survive a plausible refactor?** Line-by-line narration goes stale
   when the code moves. Rewrite to durable intent, or — better — extract a
   named function so the name carries the meaning the compiler can check.
3. **Is it terse and at the right density?** No `// Note that`, no preamble, no
   restating the function name. Dense mechanical code gets no per-line
   commentary; the one subtle line gets one.
4. **Do doc-comments state the contract, not the body?** Public `///` items
   give `# Errors`/`# Panics`/bounds and what the caller can rely on — not a
   narration of the implementation, which is free to change.

Independent review workflows must include a dedicated comment-quality pass
alongside bugs, spec, tests, and memory bounds.

## Other standing gates

- `cargo fmt --all --check`, strict clippy (workspace lints, warnings are
  errors in CI), `cargo test --workspace`, `cargo run -p xtask -- check-deps`.
- Code comments and rustdoc reference crate README sections, never `docs/`
  paths (the design archive in `docs/` will be deleted).
- Crate READMEs are the contract documentation: English `README.md` with a
  Russian `README.ru.md` mirror, kept in sync.

## Standing Rule: Multi-database collection

A collector run must cover **every** database in the instance, not only the one
named in the DSN. Classifying a metric's source view on this axis is the first
decision when adding any metric — not optional, not deferrable.

- **Instance-wide** — one connection returns rows for the whole cluster.
  `pg_stat_database`, `pg_stat_activity`, `pg_database` (wraparound), `pg_locks`,
  `pg_prepared_xacts`, `pg_stat_statements` (rows carry `dbid`),
  `pg_stat_progress_*` (rows carry `datid`), and the cluster counters with no
  owning database (`bgwriter`, `wal`, `archiver`, `io`, `replication`, slots).
  Collect once on the main connection; where the view names the owning database,
  label the row with `datname`.
- **Database-local** — the view shows only objects of the connected database, and
  the same `relname` lives in other databases invisibly. `pg_stat_user_tables` /
  `all_tables`, `pg_stat_user_indexes`, `pg_statio_user_*`,
  `pg_stat_user_functions`, bloat (`pg_class`/`pg_namespace`), object sizes.
  Collect by connecting to each database; **every row carries a non-nullable
  `datname: StrId`** taken from the connection. A database-local metric run on a
  single connection silently drops every database but the DSN's — a correctness
  bug, not a coverage gap.

**Which databases, and how.** Keep a session open to every database that accepts
connections — `pg_database WHERE datallowconn AND NOT datistemplate` — minus an
explicit exclude list from config. Sessions persist across snapshots and are
refreshed as databases appear or disappear. A database that refuses or errors is
skipped for that snapshot and never aborts the others. An explicit
single-database config (`PGDATABASE` / DSN `dbname=`) restricts the run to that
one database.

## Playbook: adding a `pg_stat_*` snapshot metric (Step 7)

Reverse-engineering the pattern is what costs time, not the change itself. Copy
a reference type instead of rediscovering the wiring.

**Reference types (copy these, don't start blank):**

- `1_006_001` bgwriter — single-row, numeric only, version split by `Option`
  columns. Files: `kronika-registry/src/codec/bgwriter_checkpointer.rs`,
  `kronika-source-pg/src/lib.rs` (`collect_bgwriter_checkpointer`).
- `1_001_00x` pg_stat_activity — **multi-row, version-grouped `type_id`s,
  string interning**. The reference for anything with rows and text. Files:
  `kronika-registry/src/codec/pg_stat_activity.rs`,
  `kronika-source-pg/src/activity.rs`, the `push_activity`/`activity_dict_limits`
  block in `pg_kronika-collector/src/main.rs`, and the
  `pg_stat_activity.feature` + `assert_activity_section` BDD pair.

**First, classify the metric** (see *Standing Rule: Multi-database collection*):
instance-wide or database-local? Database-local adds a non-nullable `datname:
StrId` to the schema and a per-database collection loop; instance-wide does not.

**Five places to change (in this order — doc leads code):**

1. `docs/type-registry/postgresql.md` — summary table row(s) + the type section.
   A new version (`VVV`) is for a *PostgreSQL catalog* difference across majors
   (monotonic column adds → separate versions, **not** one type with `Option`
   version columns; that is the registry discipline). An enrichment column that is
   identical on every major goes into the existing layouts in place — pre-release,
   no `VVV` bump, since no stored segments must stay readable. Layouts freeze at
   the first release.
2. `kronika-registry/src/codec/<name>.rs` — one `#[derive(Section)]` struct per
   version. `#[section(id=, name=, semantics=snapshot_full, sort_key("ts",...))]`,
   fields `#[column(t|c|g|l)]`. `t` = `Ts`, non-null, must be named `ts` (linter).
   `Option<T>` → nullable. Types: `i8..u64`, `f32/f64`, `bool`, `Ts`, `StrId`.
   Wire it: `codec.rs` `pub mod <name>;`; `lib.rs` add the module to the
   `pub use codec::{...}` line and the `CONTRACT`s to `registry()`.
   Database-local metric: add `datname: StrId` (non-nullable, `#[column(l)]`) and
   lead `sort_key` with it, so rows are keyed per database.
3. `kronika-source-pg/src/<name>.rs` — depends only on `registry` +
   `tokio-postgres`. Own `marked!` macro (path = this file). Shape:
   `enum Version`, `const fn <name>_version(major) -> Version`,
   `const fn <name>_query(version) -> &'static str` (wrapped in `marked!`),
   `pub struct <Name>Row` (owned `String`/`Option<String>`/numbers),
   `pub fn to_vN<E>(row, mut intern: impl FnMut(&[u8]) -> Result<StrId, E>) -> Result<TypeVN, E>`
   (interner injected as a closure — keeps this crate free of the writer; the
   mapping is then a pure fn, golden-testable with a fake intern), and
   `pub async fn collect_<name>(client, major) -> Result<(Version, Vec<<Name>Row>), tokio_postgres::Error>`.
   Add `mod <name>;` + `pub use` to `lib.rs`.
4. `pg_kronika-collector/src/main.rs` — in `snapshot_and_seal` run **all**
   `collect_*().await` first, *then* build `SectionBuffers`/`Interner` (they are
   not `Send`; holding them across an await breaks the async fn). Buffer via
   `match version`, interning with the closure
   `|b| interner.intern(b).map(|id| StrId(id.get()))`
   (`kronika_format::StrId` is `NonZeroU64` → `.get()`; `kronika_registry::StrId`
   is plain `u64`). Then `dict::encode(interner.window())` →
   `buffers.flush(&dict_sections, source_id)`. `DictLimits::new(4096, 64*1024)`
   `.and_then(|l| l.with_max_total_bytes(16<<20))` — caps collector memory.
   Database-local metric: call `collect_<name>` once per open database session and
   intern that session's `datname` into every row. Run every session's
   `collect_*().await` before building the (non-`Send`) `Interner`/`SectionBuffers`.
5. `kronika-bdd/` — `features/<name>.feature` scenario + a `then` step. Verify
   via `Segment::open`, `catalog().entries.find(type_id)`,
   `VerifiedSection::verify(Bytes, crc, crc32c)` + `TypeVN::decode`, and
   `segment.dictionary().resolve(str_id.0)` → `Some(Resolved::String(bytes))`
   to prove the string path end-to-end. Add the major to `flake.nix` `pgMatrix`,
   image `contents`, and `KRONIKA_PG_MATRIX` if extending the version matrix.

**Gotchas that cost time here:**

- clippy `-D warnings`: backtick code terms in doc/`//!` (`doc_markdown`); first
  doc paragraph = one sentence then a blank line (`too_long_first_doc_paragraph`);
  a test fake-interner returning `Result<StrId, Infallible>` needs
  `#[allow(clippy::unnecessary_wraps)]` (it must match the fallible interner
  signature `to_vN` expects).
- `kronika-source-pg` has **no** README; do not reference "crate README" in its
  rustdoc. The registry README documents types via code (bgwriter is the single
  worked example) — new types need no README section.
- `nix` is unavailable on the dev host (only `docker`/`podman`). BDD live runs in
  CI only; locally just check that `kronika-bdd` compiles + passes clippy. Only
  the layout matching the in-matrix majors is exercised live; older version
  layouts are golden-codec-only.
- SQL is the implementer's call (no owner pre-approval) — decide it per the
  registry discipline; the DBA review below is the safety net.

**Then:** workspace gate (`fmt --check`, `clippy --workspace --all-targets -D
warnings`, `test --workspace`, `xtask check-deps`) → Russian commit → **mandatory
pre-PR review panel** → PR.

**Pre-PR review panel** (run on the staged diff before the PR is opened; every
metric PR goes through it):

1. **Rust performance** — hot paths, allocations, needless clones, per-row
   work, and async overhead in the collect path.
2. **Rust anti-OOM / memory** — what enters process RAM; nothing sized by the
   database (row counts, query text, result sets) may be materialized whole
   without an enforced cap. The collector runs on the DB host: OOM there is
   worse than a lost segment.
3. **PostgreSQL DBA** — confirm that the metrics are correct and complete
   across PG 10-18, version handling is right, and the SQL is idiomatic and
   safe (no injection, correct casts, NULL handling).
4. **Linux performance / observability** — confirm that these are the right
   system and PostgreSQL signals, modelled with correct semantics (counter vs
   gauge, units, and the operator view they need).

Plus the standing comment-quality, language, test, and commit-focus review.
Fix every blocker the panel raises and re-run the relevant check before
opening the PR.
