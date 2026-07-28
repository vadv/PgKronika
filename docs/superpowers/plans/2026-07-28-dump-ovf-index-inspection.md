# pg_kronika-dump OVF Index Inspection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Научить `pg_kronika-dump` определять `.ovf` по имени, показывать его header/directory и с `--rows` декодировать `UiSummary` и `EntitySeries`.

**Architecture:** `kronika-reader` получает автономный metadata admission для одного OVF без expected PGM identity; production `open` сохраняет строгую внешнюю проверку. Новый модуль dump строит JSON только из публичных reader types, поэтому не копирует wire layout, CRC или zstd. CLI выбирает PGM/OVF/journal по суффиксу имени.

**Tech Stack:** Rust 2024, `kronika-reader`, `kronika-format::ReadAt`, `serde`, `serde_json`, Cargo test/clippy.

## Global Constraints

- Новый формат не имеет legacy-режима или fallback по magic.
- `*.pgm` выбирает PGM, `*.ovf` выбирает OVF, остальные regular files выбирают journal.
- Без `--rows` OVF не читает тела блоков.
- С `--rows` декодируются только `UiSummary` и `EntitySeries`.
- `--limit N` ограничивает series отдельно в каждом metric; dictionary остаётся полным.
- Missing bucket выводится как `null`, наблюдаемый zero — как `0.0`.
- Dump остаётся read-only и не требует sibling PGM.
- Все проверки выполняются с `--target aarch64-apple-darwin`.

---

### Task 1: Autonomous OVF Metadata Admission

**Files:**
- Modify: `crates/kronika-reader/src/overview/container.rs`
- Modify: `crates/kronika-reader/src/overview/web_index/read.rs`

**Interfaces:**
- Consumes: `FactFileReader<R>::open(reader, expected, bounds)`.
- Produces: `FactFileReader<R>::inspect(reader, bounds) -> Result<Self, CacheReadError>`.
- Produces: `FactFileReader<R>::read_ui_summary(bounds) -> Result<UiSummaryBlock, CacheReadError>`.
- Produces: `FactFileReader<R>::read_entity_series(view_code, bounds) -> Result<Option<EntitySeriesBlock>, CacheReadError>`.
- Preserves: `open` продолжает возвращать `WrongSource` при несовпадении expected identity.

- [x] **Step 1: Write failing reader tests**

Добавить рядом с тестами `FactFileReader::open`:

```rust
#[test]
fn positional_inspection_admits_metadata_without_external_identity() {
    let bytes = valid_file();
    let reader = FactFileReader::inspect(bytes.as_slice(), &LIMIT).expect("inspect");
    assert_eq!(reader.header().file_len, bytes.len() as u64);
    assert_eq!(reader.directory().len(), BlockKind::BASELINE.len());
    assert_eq!(reader.stats().read_calls, 2);
}

#[test]
fn positional_inspection_rejects_an_invalid_embedded_fact_key() {
    let mut bytes = valid_file();
    bytes[96] ^= 1;
    reseal_header(&mut bytes);
    assert!(matches!(
        FactFileReader::inspect(bytes.as_slice(), &LIMIT),
        Err(CacheReadError::Corrupt)
    ));
}
```

- [x] **Step 2: Run the focused reader tests and verify RED**

```bash
cargo test -p kronika-reader overview::container::tests::positional_inspection \
  --lib --target aarch64-apple-darwin
```

Expected: compile failure because `FactFileReader::inspect` does not exist.

- [x] **Step 3: Factor common metadata admission**

Implement:

```rust
pub fn inspect(reader: R, bounds: &Bounds) -> Result<Self, CacheReadError> {
    Self::open_inner(reader, None, bounds)
}

pub fn open(
    reader: R,
    expected: &HeaderIdentity,
    bounds: &Bounds,
) -> Result<Self, CacheReadError> {
    Self::open_inner(reader, Some(expected), bounds)
}
```

`open_inner` reads header/directory once, calls
`validate_api_inputs(&header.identity, bounds)`, checks the embedded sealed
lineage with:

```rust
let lineage = SegmentIdentity::sealed(
    header.identity.pgm_source_id,
    header.identity.source_descriptor.0,
);
if lineage.id() != header.identity.segment_lineage_id {
    return Err(CacheReadError::Corrupt);
}
```

and invokes `verify_identity` only when `expected` is present.

- [x] **Step 4: Add typed web-index reads**

Move the body of the current crate-private `read_ui_summary` and
`read_entity_series` helpers into public inherent methods on
`FactFileReader<R>`. Each method must:

1. select the exact directory address;
2. call `read_block`;
3. decode through `UiSummaryBlock::decode` or `EntitySeriesBlock::decode`;
4. call `validate_block_descriptor`;
5. verify `EntitySeriesBlock::view_code()` equals the requested `view_code`.

Keep the existing free functions as thin adapters so production callers and
their read-stat behavior do not change:

```rust
let mut fact_reader = FactFileReader::open(reader, expected, bounds)?;
let summary = fact_reader.read_ui_summary(bounds)?;
Ok((summary, fact_reader.stats()))
```

- [x] **Step 5: Run reader tests and verify GREEN**

```bash
cargo test -p kronika-reader overview::container::tests::positional_inspection \
  --lib --target aarch64-apple-darwin
cargo test -p kronika-reader overview::container \
  --lib --target aarch64-apple-darwin
```

Expected: both commands pass.

- [x] **Step 6: Commit reader API**

```bash
git add crates/kronika-reader/src/overview/container.rs \
  crates/kronika-reader/src/overview/web_index/read.rs
git commit -m "feat(reader): admit standalone OVF metadata"
```

---

### Task 2: Filename Dispatch and OVF Metadata JSON

**Files:**
- Create: `bins/pg_kronika-dump/src/ovf.rs`
- Modify: `bins/pg_kronika-dump/src/lib.rs`
- Modify: `bins/pg_kronika-dump/src/model.rs`
- Modify: `bins/pg_kronika-dump/tests/dump.rs`

**Interfaces:**
- Consumes: `FactFileReader::inspect(file, &LIMIT)` and its typed web-index reads.
- Produces: `ovf::inspect_file(file, path, options) -> Result<OvfOutput, DumpError>`.
- Produces: `Output::Ovf(OvfOutput)` with `kind: "ovf"`.

- [x] **Step 1: Write failing filename and metadata tests**

Build a bounded OVF fixture through `FactFile::build`, write it as
`segment.ovf`, and assert:

```rust
#[test]
fn ovf_name_selects_metadata_dump_without_reading_bodies() {
    let fixture = TempDir::new().expect("tempdir");
    let path = fixture.path().join("segment.ovf");
    fs::write(&path, ovf_fixture()).expect("write OVF");

    let (code, json, stderr) = run_json([path.into_os_string()]);

    assert_eq!(code, ExitCode::SUCCESS, "{stderr}");
    assert_eq!(json["kind"], "ovf");
    assert_eq!(json["header"]["pgm_source_id"], 7);
    assert!(json["blocks"].as_array().is_some_and(|blocks| !blocks.is_empty()));
    assert!(json["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .all(|block| block.get("content").is_none()));
}
```

Add separate assertions that a valid PGM named `.ovf` fails as OVF and a valid
OVF named `.parts` fails as journal. This proves there is no magic fallback.

- [x] **Step 2: Run dump integration tests and verify RED**

```bash
cargo test -p pg_kronika-dump ovf_name_selects \
  --test dump --target aarch64-apple-darwin
```

Expected: failure because `.ovf` is routed to journal and `Output::Ovf` is absent.

- [x] **Step 3: Add filename dispatch**

Replace magic-based PGM selection with exact final suffix selection:

```rust
match arguments.path.extension().and_then(OsStr::to_str) {
    Some("pgm") => pgm::inspect_file(file, &arguments.path, arguments.options).map(Output::Pgm),
    Some("ovf") => ovf::inspect_file(file, &arguments.path, arguments.options).map(Output::Ovf),
    _ => journal::inspect_file(&file, &arguments.path, arguments.options).map(Output::Journal),
}
```

Keep each selected decoder responsible for validating its own magic and
framing. Remove `has_pgm_magic`.

- [x] **Step 4: Add metadata model and converter**

Define serializable `OvfOutput`, `OvfHeaderOutput`, and `OvfBlockOutput`.
`OvfBlockOutput.content` is `None` in this task. Convert binary identifiers with
a local allocation-bounded helper:

```rust
fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len().saturating_mul(2));
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("String writes cannot fail");
    }
    output
}
```

Map every known `BlockKind` to stable snake_case; retain unknown kinds as
`None`. Map `BlockCodec::{None,Zstd}` to `"none"` and `"zstd"`. Serialize
directory timestamps as `Option<i64>` when `has_time_range` is false.

- [x] **Step 5: Run metadata tests and verify GREEN**

```bash
cargo test -p pg_kronika-dump ovf_name_selects \
  --test dump --target aarch64-apple-darwin
cargo test -p pg_kronika-dump filename \
  --test dump --target aarch64-apple-darwin
```

Expected: filename and metadata tests pass.

- [x] **Step 6: Commit metadata mode**

```bash
git add bins/pg_kronika-dump/src/lib.rs \
  bins/pg_kronika-dump/src/model.rs \
  bins/pg_kronika-dump/src/ovf.rs \
  bins/pg_kronika-dump/tests/dump.rs
git commit -m "feat(dump): inspect OVF metadata by filename"
```

---

### Task 3: Decode UiSummary and EntitySeries with --rows

**Files:**
- Modify: `bins/pg_kronika-dump/src/model.rs`
- Modify: `bins/pg_kronika-dump/src/ovf.rs`
- Modify: `bins/pg_kronika-dump/tests/dump.rs`

**Interfaces:**
- Consumes: `UiSummaryBlock::decode`, `EntitySeriesBlock::decode`.
- Produces: `OvfBlockOutput.content: Option<OvfBlockContentOutput>`.
- Preserves: bodies are never read when `Options.rows == false`.

- [ ] **Step 1: Write failing logical-content tests**

Create a fixture with one summary view and one entity metric containing both a
missing bucket and an observed zero. Assert:

```rust
#[test]
fn ovf_rows_decodes_web_index_and_preserves_missing_zero() {
    let path = write_ovf_fixture();
    let (code, json, stderr) =
        run_json([path.into_os_string(), OsString::from("--rows")]);
    assert_eq!(code, ExitCode::SUCCESS, "{stderr}");

    let series = &json["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .find(|block| block["kind"] == "entity_series")
        .expect("entity series")["content"]["metrics"][0]["series"][0];
    assert!(series["values"][0].is_null());
    assert_eq!(series["values"][1], 0.0);
    assert_eq!(series["key"], "0102");
    assert_eq!(series["label"], "backend 42");
}
```

Add a two-series fixture and run with `--rows --limit 1`; assert one returned
series, `truncated: true`, and full dictionary length.

- [ ] **Step 2: Run logical-content tests and verify RED**

```bash
cargo test -p pg_kronika-dump ovf_rows \
  --test dump --target aarch64-apple-darwin
```

Expected: failure because OVF blocks have no `content`.

- [ ] **Step 3: Implement UiSummary conversion**

Read only `(BlockKind::UiSummary, logical_id=0)` through the typed reader method:

```rust
let summary = reader.read_ui_summary(&LIMIT)?;
```

Expand snapshot presence/notable masks against `snapshot_times()` into arrays of
`Option<u64>` and `Option<bool>`. Expand coverage to `bucket_count` booleans.
The canonical empty summary emits `grid: null`, empty snapshots, and empty views.

- [ ] **Step 4: Implement EntitySeries conversion**

For every directory entry with `BlockKind::EntitySeries`, call
`read_entity_series(view_code, &LIMIT)` and serialize dictionary, metrics and at
most `options.limit` series per metric. Resolve every `entity_ref` through the
full dictionary and reject a missing reference.

Build bucket values without changing semantics:

```rust
let values = (0..usize::from(block.grid().bucket_count()))
    .map(|bucket| series.value_at(bucket))
    .collect::<Vec<_>>();
```

Set `truncated` to `metric.series().len() > options.limit`.

- [ ] **Step 5: Run logical-content and package tests and verify GREEN**

```bash
cargo test -p pg_kronika-dump ovf_rows \
  --test dump --target aarch64-apple-darwin
cargo test -p pg_kronika-dump \
  --target aarch64-apple-darwin
```

Expected: all dump tests pass.

- [ ] **Step 6: Commit logical contents**

```bash
git add bins/pg_kronika-dump/src/model.rs \
  bins/pg_kronika-dump/src/ovf.rs \
  bins/pg_kronika-dump/tests/dump.rs
git commit -m "feat(dump): decode OVF web indexes"
```

---

### Task 4: Documentation and Full Verification

**Files:**
- Modify: `bins/pg_kronika-dump/README.md`
- Modify: `bins/pg_kronika-dump/README.ru.md`

**Interfaces:**
- Documents: filename dispatch, metadata output, `--rows`, `--limit`, and the
  absence of sibling provenance verification.

- [ ] **Step 1: Update both READMEs**

Add runnable examples:

```bash
pg_kronika-dump /var/lib/pg_kronika/2026/07/28/1785200000000000.ovf |
  jq '{header, blocks}'

pg_kronika-dump /var/lib/pg_kronika/2026/07/28/1785200000000000.ovf \
  --rows --limit 10 |
  jq '.blocks[] | select(.content != null)'
```

State that `.ovf` selection is filename-based, metadata mode does not read
bodies, and standalone inspection proves internal integrity but not sibling PGM
ownership.

- [ ] **Step 2: Run complete verification**

```bash
cargo fmt --all -- --check
cargo test -p kronika-reader --lib --target aarch64-apple-darwin
cargo test -p pg_kronika-dump --target aarch64-apple-darwin
cargo clippy -p kronika-reader -p pg_kronika-dump --all-targets \
  --target aarch64-apple-darwin -- -D warnings
cargo run -q -p xtask --target aarch64-apple-darwin -- check-deps
git diff --check
```

Expected: every command exits 0.

- [ ] **Step 3: Commit documentation**

```bash
git add bins/pg_kronika-dump/README.md bins/pg_kronika-dump/README.ru.md
git commit -m "docs(dump): describe OVF index inspection"
```

- [ ] **Step 4: Push the existing PR branch and wait for CI**

```bash
git push origin feat/web-index
gh pr checks 135 --watch --interval 10
```

Expected: PR #135 points at the final commit and all required checks pass.
