# Complete Source ID Removal Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Remove the global `source_id` entity from PgKronika formats, storage, analytics, CLI, and HTTP API so one data root always represents one PostgreSQL observation stream.

**Architecture:** Convert the codebase to a single-root model from the persistence boundary outward. PGM and OVF identities retain content-derived descriptors and contract versions, while all numeric source qualification, grouping, filtering, and wire fields disappear. HTTP operates implicitly on the configured root and publishes singular freshness and loss metadata.

**Tech Stack:** Rust 2024 workspace, Axum, Serde, SHA-256, CRC32C, proptest, Cucumber BDD

## Global Constraints

- One data root belongs to one collector and one observed PostgreSQL.
- Do not add a constant, sentinel, generated replacement, compatibility field, or migration reader for `source_id`.
- Old PGM, journal, and OVF bytes are unsupported.
- Rename metric `source_type_id` to `section_type_id`.
- Keep `SourceDescriptor` and `source_file_len`; they identify the exact input PGM, not a PostgreSQL namespace.
- Remove `GET /v1/sources`, every `source` query parameter, and every global source field in JSON.
- Preserve bounded reads, CRC validation, admission limits, and cache-key determinism.
- Do not modify the unrelated untracked `bins/.DS_Store`.

---

### Task 1: Make Analytics Identities Single-Root

**Files:**
- Modify: `crates/kronika-analytics/src/overview/observation.rs`
- Modify: `crates/kronika-analytics/src/overview/metric.rs`
- Modify: `crates/kronika-analytics/src/overview/fact.rs`
- Modify: `crates/kronika-analytics/src/overview/notable.rs`
- Test: unit tests in the same modules

**Interfaces:**
- Produces: `SegmentIdentity::sealed(source_descriptor: [u8; 32])`
- Produces: `SegmentIdentity::live_approximate(journal_generation: u64, first_part_descriptor: &[u8])`
- Produces: `MetricSeriesDescriptor::new(factor, section_type_id, unit, entity, reset_family, series_discriminator)`
- Produces: `derive_entity(kind, source_identity)` and `derive_alignment(entity)`
- Removes: every global `source_id` member and accessor from analytics facts, coverage, observations, and metric descriptors

- [ ] **Step 1: Write failing identity tests**

Replace source-qualified constructors in the unit tests with the required API and
assert that identities remain content-sensitive:

```rust
#[test]
fn sealed_lineage_depends_on_descriptor() {
    assert_ne!(
        SegmentIdentity::sealed([1; 32]).id(),
        SegmentIdentity::sealed([2; 32]).id(),
    );
}

fn descriptor(section_type_id: u32) -> MetricSeriesDescriptor {
    MetricSeriesDescriptor::new(
        MetricFactor::PgDatabaseDeadlocks,
        section_type_id,
        MetricUnit::Count,
        Some(derive_entity(
            EntityKind::Database,
            &42_u32.to_le_bytes(),
        )),
        Some(ResetFamily::PgStatDatabase),
        &42_u32.to_le_bytes(),
    )
}

#[test]
fn metric_series_depends_on_section_type() {
    let left = descriptor(10);
    let right = descriptor(11);
    assert_ne!(left.series_id, right.series_id);
}
```

- [ ] **Step 2: Run the focused tests and verify RED**

Run:

```bash
cargo test -p kronika-analytics overview::observation::tests::sealed_lineage_depends_on_descriptor
cargo test -p kronika-analytics overview::metric::tests::metric_series_depends_on_section_type
```

Expected: compile failure because the new constructor signatures and
`section_type_id` do not exist.

- [ ] **Step 3: Remove source qualification from identities**

Use the existing domain tags but hash only proven inputs:

```rust
pub fn sealed(source_descriptor: [u8; 32]) -> Self {
    Self {
        id: SegmentLineageId(sha256::digest_parts(&[
            LINEAGE_DOMAIN_TAG,
            &source_descriptor,
        ])),
        quality: IdentityQuality::ContentDerived,
    }
}
```

Remove `source_id` from `SegmentIdentity`, `Observation`, `MetricSeriesDescriptor`,
coverage/fact structs, evidence derivation, and notable hashes. Rename
`source_type_id` fields and parameters to `section_type_id`.

- [ ] **Step 4: Run analytics tests and verify GREEN**

Run:

```bash
cargo test -p kronika-analytics
```

Expected: all analytics tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/kronika-analytics
git commit -m "refactor: remove source-qualified analytics identities"
```

---

### Task 2: Replace the PGM Catalog Layout

**Files:**
- Modify: `crates/kronika-format/src/catalog.rs`
- Modify: `crates/kronika-format/src/parts.rs`
- Modify: `crates/kronika-format/src/dictionary.rs`
- Modify: `crates/kronika-format/src/lib.rs`
- Modify: `crates/kronika-format/tests/fixture.rs`
- Modify: `crates/kronika-format/tests/parts_property.rs`
- Modify: `crates/kronika-format/tests/property.rs`
- Modify: `crates/kronika-store/src/catalog_summary.rs`
- Modify: `crates/kronika-store/src/local.rs`
- Test: catalog and store tests in the same files

**Interfaces:**
- Produces: `META_LEN == 32`
- Produces: `Catalog { entries, min_ts, max_ts, format_version, window_count }`
- Produces: `CatalogView` and `CatalogSummary` without `source_id`
- Removes: `CollectionMeta::source_id` and source bytes from logical/layout digests

- [ ] **Step 1: Write failing catalog-layout tests**

Update the canonical catalog fixture to omit `source_id` and add:

```rust
#[test]
fn catalog_metadata_is_32_bytes() {
    assert_eq!(META_LEN, 32);
}

fn old_layout_catalog_bytes() -> Vec<u8> {
    let mut bytes = vec![0_u8; 40];
    bytes[0..8].copy_from_slice(&1_i64.to_le_bytes());
    bytes[8..16].copy_from_slice(&2_i64.to_le_bytes());
    bytes[16..24].copy_from_slice(&7_u64.to_le_bytes());
    bytes[24..28].copy_from_slice(&0_u32.to_le_bytes());
    bytes[28..32].copy_from_slice(&1_u32.to_le_bytes());
    bytes[36..40].copy_from_slice(&1_u32.to_le_bytes());
    let checksum = crc32c(&bytes);
    bytes[32..36].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

#[test]
fn old_40_byte_metadata_is_rejected() {
    let old = old_layout_catalog_bytes();
    assert!(matches!(
        Catalog::decode(&old),
        Err(DecodeError::BadCatalogLen { .. })
    ));
}
```

The old-layout fixture must append fields at offsets `16..24`, `24..28`,
`28..32`, `32..36`, and `36..40` and recompute its old CRC.

- [ ] **Step 2: Run format tests and verify RED**

Run:

```bash
cargo test -p kronika-format catalog_metadata_is_32_bytes
cargo test -p kronika-format old_40_byte_metadata_is_rejected
```

Expected: the size assertion fails with `left: 40`, and the old layout is still
accepted by the current decoder.

- [ ] **Step 3: Encode and decode the 32-byte metadata**

Set the offsets exactly:

```text
0..8    min_ts
8..16   max_ts
16..20  entry_count
20..24  format_version
24..28  crc32c
28..32  window_count
```

Set `META_CRC_OFFSET` to `24`, remove `source_id` from owned and borrowed
catalogs, update `FORMAT_VERSION`, and remove the field from part metadata.

- [ ] **Step 4: Remove source bytes from store summaries**

Update `CatalogSummary::from_view`, catalog digests, discovery metadata, local
fixtures, and equality checks. Logical and layout digests must still include
timestamps, format version, window count, entries, offsets, lengths, rows, and
checksums.

- [ ] **Step 5: Run format and store tests and verify GREEN**

Run:

```bash
cargo test -p kronika-format
cargo test -p kronika-store
```

Expected: all tests pass, including property tests and old-layout rejection.

- [ ] **Step 6: Commit**

```bash
git add crates/kronika-format crates/kronika-store
git commit -m "feat: remove source id from pgm format"
```

---

### Task 3: Remove Source State from Writer and Collector

**Files:**
- Modify: `crates/kronika-writer/src/buffer.rs`
- Modify: `crates/kronika-writer/src/interner.rs`
- Modify: `crates/kronika-writer/src/journal.rs`
- Modify: `crates/kronika-writer/src/recovery.rs`
- Modify: `crates/kronika-writer/src/segment.rs`
- Modify: `crates/kronika-writer/src/lib.rs`
- Modify: `bins/pg_kronika-collector/src/config.rs`
- Modify: `bins/pg_kronika-collector/src/logging.rs`
- Modify: `bins/pg_kronika-collector/src/segments.rs`
- Modify: `bins/pg_kronika-collector/src/tests/runtime.rs`
- Modify: `crates/kronika-writer/examples/measure_coalesced_sections.rs`
- Test: writer and collector unit tests

**Interfaces:**
- Produces: `Buffer::flush_with_summary(dict_sections)`
- Removes: mixed-source seal errors and all source arguments from writer APIs

- [ ] **Step 1: Write failing single-root writer and reader tests**

Change the writer test call to:

```rust
let flushed = buffer.flush_with_summary(&dict_sections).expect("flush");
assert_eq!(flushed.catalog.min_ts, expected_min);
```

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p kronika-writer flush
```

Expected: compile failures because the existing APIs still require `source_id`.

- [ ] **Step 3: Simplify writer and collector APIs**

Remove source parameters from buffer flush, part construction, journal recovery,
seal planning, seal logging, and collector configuration. Delete
`MixedSourceIds`-style errors and all `0` sentinel merging.

- [ ] **Step 4: Run affected tests and verify GREEN**

Run:

```bash
cargo test -p kronika-writer
cargo test -p pg_kronika-collector
```

Expected: all affected tests pass and collector config tests no longer mention
`KRONIKA_SOURCE_ID`.

- [ ] **Step 5: Commit**

```bash
git add crates/kronika-writer bins/pg_kronika-collector
git commit -m "refactor: make collection single-root"
```

---

### Task 4: Make Reader and OVF Single-Root

**Files:**
- Modify: `crates/kronika-reader/src/lib.rs`
- Modify: `crates/kronika-reader/src/unit.rs`
- Modify: `crates/kronika-reader/src/snapshot.rs`
- Modify: `crates/kronika-reader/src/snapshot/tests.rs`
- Modify: `crates/kronika-reader/src/refresh.rs`
- Modify: `crates/kronika-reader/src/query/cursor.rs`
- Modify: `crates/kronika-reader/src/query/latest.rs`
- Modify: `crates/kronika-reader/src/query/section.rs`
- Modify: `crates/kronika-reader/src/query/value.rs`
- Modify: `crates/kronika-reader/src/overview/container.rs`
- Modify: `crates/kronika-reader/src/overview/factkey.rs`
- Modify: `crates/kronika-reader/src/overview/block.rs`
- Modify: `crates/kronika-reader/src/overview/event_facts.rs`
- Modify: `crates/kronika-reader/src/overview/descriptors.rs`
- Modify: `crates/kronika-reader/src/overview/dictionary.rs`
- Modify: `crates/kronika-reader/src/overview/facts.rs`
- Modify: `crates/kronika-reader/src/overview/fallback.rs`
- Modify: `crates/kronika-reader/src/overview/live.rs`
- Modify: `crates/kronika-reader/src/overview/metric_extract.rs`
- Modify: `crates/kronika-reader/src/overview/publish.rs`
- Modify: `crates/kronika-reader/src/overview/gc.rs`
- Modify: `crates/kronika-reader/src/overview/gc/tests.rs`
- Modify: `crates/kronika-reader/src/overview/persist_mode/tests.rs`
- Modify: `crates/kronika-reader/src/overview/qualification_fixture.rs`
- Modify: `crates/kronika-reader/benches/diff_scan.rs`
- Modify: `crates/kronika-reader/benches/overview_codec.rs`
- Modify: `crates/kronika-reader/benches/serving.rs`
- Modify: `crates/kronika-reader/examples/overview_qualification.rs`
- Test: overview codec, facts, GC, live, and publish tests

**Interfaces:**
- Produces: `HeaderIdentity::from_current_contract(format, min, max, len, descriptor, lineage)`
- Produces: 184-byte OVF header with CRC at offset 180
- Produces: `FactKey::derive(descriptor, file_kind, schema, extractor, registry)`
- Produces: `UnitMeta { min_ts, max_ts, live }`
- Produces: range/section queries without a numeric source argument
- Removes: `source_summaries` and their limit/error types
- Removes: serialized source fields from manifests, descriptors, facts, and event blocks

- [ ] **Step 1: Write failing header and key tests**

Add or update:

```rust
#[test]
fn header_is_184_bytes_without_source_id() {
    assert_eq!(HEADER_LEN, 184);
}

#[test]
fn fact_key_depends_on_descriptor_and_contract() {
    assert_ne!(
        FactKey::for_current_segment(descriptor(1)),
        FactKey::for_current_segment(descriptor(2)),
    );
}
```

Add an admission test that feeds an old 192-byte header and expects
`CacheReadError::Incompatible`.

Change a section query test to call the public query without a source:

```rust
let page = section(&mut snapshot, "pg_stat_activity", from, to, limit, None)
    .expect("single-root section");
assert_eq!(page.rows.len(), expected_rows);
```

- [ ] **Step 2: Run focused overview tests and verify RED**

Run:

```bash
cargo test -p kronika-reader overview::container::tests::header_is_184_bytes_without_source_id
cargo test -p kronika-reader overview::factkey::tests::fact_key_depends_on_descriptor_and_contract
cargo test -p kronika-reader query::section
```

Expected: compile or assertion failures under the old header and constructor.

- [ ] **Step 3: Encode the new header and fact key**

Remove the `u64` immediately after `source_format_version`; set
`HEADER_LEN=184`, `HEADER_CRC_OFFSET=180`, and `directory_offset=184`. Change
the OVF internal version constants so old bytes cannot be admitted.

Hash `FactKey` from:

```rust
[
    FACT_KEY_TAG,
    source_descriptor,
    file_kind,
    fact_schema_version,
    extractor_semantics_version,
    registry_contract_version,
]
```

- [ ] **Step 4: Remove source fields from OVF blocks and extraction**

Update encoded lengths and field order for manifests, event facts, metric
descriptors, coverage, reset boundaries, live state, and GC header probes.
Rename every metric descriptor wire/member occurrence from `source_type_id` to
`section_type_id`.

- [ ] **Step 5: Flatten snapshot and query selection**

Remove source from `UnitMeta`, descriptors, active state, cursor state, and query
filters. Sort the whole root canonically by time and locator. Delete
`source_summaries`, `SourceSummaryLimits`, `SourceSummaryError`, and
`SourceSummaryResource` exports.

- [ ] **Step 6: Run all reader tests and verify GREEN**

Run:

```bash
cargo test -p kronika-reader --lib
cargo test -p kronika-reader --tests
```

Expected: query, codec, corruption, admission, facts, web-index, live, publish,
and GC tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/kronika-reader
git commit -m "feat: remove source id from ovf format"
```

---

### Task 5: Remove Source from the Complete Web API

**Files:**
- Modify: `bins/pg_kronika-web/src/lib.rs`
- Modify: `bins/pg_kronika-web/src/problem.rs`
- Modify: `bins/pg_kronika-web/src/params.rs`
- Modify: `bins/pg_kronika-web/src/handlers/v1.rs`
- Modify: `bins/pg_kronika-web/src/handlers/anomalies.rs`
- Modify: `bins/pg_kronika-web/src/handlers/incidents.rs`
- Modify: `bins/pg_kronika-web/src/overview/view.rs`
- Modify: `bins/pg_kronika-web/src/overview/selection.rs`
- Modify: `bins/pg_kronika-web/src/overview/live.rs`
- Modify: `bins/pg_kronika-web/src/overview/memory_cache.rs`
- Modify: `bins/pg_kronika-web/src/overview/handlers.rs`
- Modify: `bins/pg_kronika-web/src/overview/dto.rs`
- Modify: `bins/pg_kronika-web/src/serialize.rs`
- Modify: `bins/pg_kronika-web/src/ui/data.rs`
- Modify: `bins/pg_kronika-web/src/ui/handlers.rs`
- Modify: `bins/pg_kronika-web/src/ui/heatmap.rs`
- Modify: `bins/pg_kronika-web/src/incident/engine.rs`
- Modify: `bins/pg_kronika-web/src/incident/entity_join.rs`
- Modify: `bins/pg_kronika-web/src/incident_input.rs`
- Modify: `bins/pg_kronika-web/src/incident_response.rs`
- Modify: `bins/pg_kronika-web/src/plan_anomaly.rs`
- Modify: `bins/pg_kronika-web/src/qualification.rs`
- Modify: `bins/pg_kronika-web/src/startup.rs`
- Modify: `bins/pg_kronika-web/benches/anomalies.rs`
- Test: `bins/pg_kronika-web/src/tests/mod.rs`
- Test: `bins/pg_kronika-web/src/tests/anomalies.rs`
- Test: `bins/pg_kronika-web/src/tests/incidents.rs`
- Test: `bins/pg_kronika-web/src/tests/overview_timeline.rs`
- Test: `bins/pg_kronika-web/src/tests/overview_resilience.rs`
- Test: `bins/pg_kronika-web/src/tests/overview_admission.rs`
- Test: `bins/pg_kronika-web/src/tests/problems.rs`
- Test: `bins/pg_kronika-web/src/tests/sections.rs`
- Test: `bins/pg_kronika-web/src/tests/ui_catalog.rs`
- Test: `bins/pg_kronika-web/src/tests/ui_data.rs`
- Test: `bins/pg_kronika-web/src/tests/version_diff.rs`
- Test: `bins/pg_kronika-web/src/tests/auth_static.rs`
- Test: `bins/pg_kronika-web/src/tests/probes_metrics.rs`

**Interfaces:**
- Produces: selected timeline plan over the whole snapshot and requested range
- Produces: singular `FreshnessDto` and `LossDto`
- Produces: `EntityScope::new(node_self_id: &str) -> Option<EntityScope>`
- Removes: `QueryParameter::Source` and `GET /v1/sources`
- Removes: `DescriptorSource`, source grouping, source-set hash, source filters, and per-source accumulators

- [ ] **Step 1: Write failing timeline contract tests**

Change the request to omit source:

```rust
let response = get("/v1/timeline/overview?from=1000&to=2000").await;
assert_eq!(response.status(), StatusCode::OK);
assert!(response.json()["meta"].get("sources").is_none());
assert!(response.json()["meta"]["freshness"].is_object());
assert!(response.json()["meta"]["loss"].is_object());
```

Add an assertion that `source=7` returns the existing unknown-query-parameter
problem. Add router, anomaly, and incident cases:

```rust
assert_eq!(request("/v1/sources").await.status(), StatusCode::NOT_FOUND);
assert_problem(
    request("/v1/sections?source=7&from=0&to=1").await,
    "unknown_query_parameter",
    "source",
);
assert_eq!(
    get("/v1/anomalies?from=0&to=1000000&window=1m").await.status(),
    StatusCode::OK,
);
assert_eq!(
    get("/v1/incidents?from=0&to=1000000&window=1m&step=10s").await.status(),
    StatusCode::OK,
);
```

- [ ] **Step 2: Run the focused timeline test and verify RED**

Run:

```bash
cargo test -p pg_kronika-web overview_without_source_uses_the_whole_root
cargo test -p pg_kronika-web sources_route_is_absent
cargo test -p pg_kronika-web anomalies_without_source_use_the_root
cargo test -p pg_kronika-web incidents_without_source_use_the_root
```

Expected: missing-source errors, the still-present `/v1/sources` route, or old
response-shape assertions.

- [ ] **Step 3: Replace per-source selection with one accumulator**

Select all sealed and live units intersecting the range. Compute one fact-set
hash from ordered descriptors, one data-through boundary, one completeness
status, one known-gap set, and one loss lower bound. Remove source from response
keys, cursor hashes, event projection, ETag, and memory-cache keys.

- [ ] **Step 4: Publish singular timeline metadata**

Replace `SourceFreshnessDto` and `SourceLossDto` with:

```rust
struct FreshnessDto {
    data_through_us: Option<i64>,
    status: &'static str,
    completeness: &'static str,
    retained_exactness: &'static str,
    physical_count_semantics: &'static str,
}

struct LossDto {
    known_gaps: Vec<CoverageSpanDto>,
    dropped_count_lower_bound: Option<u64>,
}
```

Remove `sources`, `available_sources`, and `source_status` from
`TimelineMetaDto`; keep singular `status`, `freshness`, and `loss`.

- [ ] **Step 5: Remove the public parameter and data filters**

Delete `QueryParameter::Source`, the `/v1/sources` route and handler,
source-summary error mapping, and source parsing/filtering from sections,
segments, UI catalog, summary, and heatmap. Ensure `/v1/*` fallback returns an
API `404` rather than the SPA document.

- [ ] **Step 6: Flatten anomalies and incidents**

Remove source from request validation, unit selection, prepared input,
detectors, incident engine config, entity joins, response builders, stable
keys, and tests. Join node entities only when `node_self_id` is non-empty:

```rust
#[test]
fn entity_scope_is_defined_by_node_identity() {
    assert_eq!(scope("node-a"), scope("node-a"));
    assert_ne!(scope("node-a"), scope("node-b"));
}
```

- [ ] **Step 7: Run all web tests and verify GREEN**

Run:

```bash
cargo test -p pg_kronika-web
```

Expected: all router, section, UI, timeline, anomaly, incident, cursor, loss,
resilience, and qualification tests pass without source fields or parameters.

- [ ] **Step 8: Commit**

```bash
git add bins/pg_kronika-web
git commit -m "feat: remove source from web api"
```

---

### Task 6: Update Dump, BDD, Documentation, and Enforce Absence

**Files:**
- Modify: `bins/pg_kronika-dump/tests/dump.rs`
- Modify: `crates/kronika-bdd/src/harness/assert_row.rs`
- Modify: `crates/kronika-bdd/src/harness/web.rs`
- Modify: `crates/kronika-bdd/src/harness/web_lifecycle.rs`
- Modify: `crates/kronika-bdd/src/harness/web_process.rs`
- Modify: `crates/kronika-bdd/src/steps/connection_pool.rs`
- Modify: `crates/kronika-bdd/src/steps/plan_anomalies.rs`
- Modify: `crates/kronika-bdd/src/steps/web.rs`
- Modify: `crates/kronika-source-os/src/mount.rs`
- Modify: `bins/pg_kronika-collector/README.md`
- Modify: `bins/pg_kronika-collector/README.ru.md`
- Modify: `bins/pg_kronika-web/README.md`
- Modify: `bins/pg_kronika-web/README.ru.md`
- Modify: `crates/kronika-format/README.md`
- Modify: `crates/kronika-format/README.ru.md`
- Modify: `crates/kronika-reader/README.md`
- Modify: `crates/kronika-reader/README.ru.md`
- Modify: `crates/kronika-registry/README.md`
- Modify: `crates/kronika-registry/README.ru.md`
- Modify: `README.md`
- Modify: `README.ru.md`
- Modify: `CLAUDE.md`
- Test: dump integration tests and BDD suite

**Interfaces:**
- Produces: dump output and documentation with no global source ID
- Produces: static repository audit for forbidden production tokens

- [ ] **Step 1: Write failing dump and BDD expectations**

Change dump fixtures to build catalogs without source and assert:

```rust
assert!(!stdout.contains("source_id"));
```

Change BDD web URIs to omit `source` and JSON assertions to reject
`source_id`, `sources`, and `available_sources`.

- [ ] **Step 2: Run focused tests and verify RED**

Run:

```bash
cargo test -p pg_kronika-dump
cargo test -p kronika-bdd --lib
```

Expected: fixture compile failures or old output assertions until callers are
updated.

- [ ] **Step 3: Update dump, BDD, and documentation**

Remove environment/config/API references. Update request examples to use only
time range and endpoint-specific parameters. Document the single-root
invariant and the requirement to regenerate pre-change demo data.

- [ ] **Step 4: Run the static absence audit**

Run:

```bash
rg -n '\bsource_id\b|KRONIKA_SOURCE_ID|/v1/sources|QueryParameter::Source' \
  --glob '*.rs' --glob '*.md' --glob '*.feature' \
  --glob '!docs/superpowers/plans/**' \
  --glob '!docs/superpowers/specs/**'
```

Expected: no global-source production or active-document matches. Rename
unrelated ambiguous locals such as the mount source string intern ID to
`source_str_id`. Historical design documents may retain matches only inside
the excluded paths.

- [ ] **Step 5: Run full verification**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets --all-features
```

Expected: all commands exit `0`.

- [ ] **Step 6: Commit**

```bash
git add README.md README.ru.md CLAUDE.md bins crates docs
git commit -m "docs: document the single-root data model"
```

- [ ] **Step 7: Inspect the final diff**

Run:

```bash
git diff --check origin/main...HEAD
git status --short
git log --oneline origin/main..HEAD
```

Expected: no whitespace errors; only `bins/.DS_Store` may remain untracked; all
commits belong to complete source removal.
