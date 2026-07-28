# Web Index Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Реализовать bounded wire-формат web-индекса в OVF: `UiSummary`,
`EntitySeries(view_code)`, сжатие, selective read и полную admission-проверку.

**Architecture:** `UiSummary` становится обязательным baseline-блоком и хранит
общую delta-таблицу времён снимков. `EntitySeries` является повторяемым
необязательным блоком, адресуемым через `(BlockKind, view_code)`. Оба codec
живут в отдельном модуле `overview/web_index`; container отвечает только за
framing, Zstd и directory contracts.

**Tech Stack:** Rust 2024, существующие `ByteReader`/`ByteWriter`, `zstd`
bulk codec, proptest, встроенный test harness.

## Global Constraints

- Production code пишется только после наблюдаемого RED-теста.
- Локальные тесты запускаются с `--target aarch64-apple-darwin`: default
  musl-target на этой машине не линкует zstd/mimalloc; Linux musl проверяет CI.
- `UiSummary`: не более 32 view, 4096 timestamps и 64 КиБ decoded bytes.
- `EntitySeries`: не более 16 metrics, K=64, 256 buckets, 1024 dictionary
  entries, 256 КиБ decoded и 128 КиБ stored bytes на view.
- Identity не длиннее 256 байтов, label не длиннее 160 байтов.
- Декомпрессия резервирует и проверяет `decoded_len` до allocation.
- Новые rustdoc/comments описывают contract и bounds, а не пересказывают код.
- README.md и README.ru.md меняются синхронно.

---

### Task 1: Общая модель и UiSummary codec

**Files:**
- Create: `crates/kronika-reader/src/overview/web_index/mod.rs`
- Create: `crates/kronika-reader/src/overview/web_index/summary.rs`
- Modify: `crates/kronika-reader/src/overview/mod.rs`
- Modify: `crates/kronika-reader/src/overview/limits.rs`

**Interfaces:**
- Produces: `IndexStatus`, `TimeGrid`, `ViewSummary`, `UiSummaryBlock`.
- `TimeGrid::for_range(first_ts_us, last_ts_us) -> Result<TimeGrid, BlockError>`.
- `UiSummaryBlock::new(grid, snapshot_times, views, bounds)`.
- `UiSummaryBlock::decode(body, bounds)`.

- [x] **Step 1: Написать failing tests UiSummary**

Проверить:

```rust
#[test]
fn ui_summary_round_trips_shared_snapshot_times() { /* two views, one union */ }

#[test]
fn ui_summary_population_belongs_to_each_present_snapshot() { /* no future bucket value */ }

#[test]
fn ui_summary_rejects_presence_population_mismatch() { /* popcount != values */ }

#[test]
fn adaptive_grid_never_truncates_a_long_segment() { /* bucket_count <= 256 */ }
```

- [x] **Step 2: Запустить focused test и подтвердить RED**

Run:

```bash
cargo test -p kronika-reader web_index::summary --lib \
  --target aarch64-apple-darwin
```

Expected: compile failure, потому что модуль и типы ещё отсутствуют.

- [x] **Step 3: Расширить Bounds**

Добавить отдельные поля для view, timestamps, metrics, top-K, buckets,
identity/label bytes, dictionary entries и размеров web-блоков. Включить их в
`is_within_absolute_limits`, `admits_profile`, `LIMIT` и exact-value test.

- [x] **Step 4: Реализовать common model и UiSummary**

Wire:

```text
summary_revision, grid, snapshot_time_count, delta timestamps, view_count
view_code, view_revision, status, snapshot_presence, populations, coverage
```

Constructor сортирует view по `view_code`, проверяет уникальность, strict
timestamps, bitset tails, popcount и grid projection. Decoder проверяет bounds
до `Vec::with_capacity`.

- [x] **Step 5: Подтвердить GREEN**

Run focused command из Step 2. Expected: все `web_index::summary` tests PASS.

### Task 2: EntitySeries codec

**Files:**
- Create: `crates/kronika-reader/src/overview/web_index/series.rs`
- Modify: `crates/kronika-reader/src/overview/web_index/mod.rs`
- Modify: `crates/kronika-reader/src/overview/proptests.rs`

**Interfaces:**
- Produces: `EntityDictionaryEntry`, `MetricAggregation`, `MetricStatus`,
  `EntitySeries`, `EntityMetric`, `EntitySeriesBlock`.
- `EntitySeriesBlock::new(...)` validates canonical order and exact bounds.
- `EntitySeriesBlock::decode(body, bounds)` consumes the whole body.

- [x] **Step 1: Написать failing tests EntitySeries**

```rust
#[test]
fn entity_series_round_trips_missing_and_observed_zero() { /* mask distinguishes */ }

#[test]
fn entity_series_rejects_duplicate_dictionary_keys() { /* typed bytes */ }

#[test]
fn entity_series_rejects_series_outside_top_k() { /* K=64 */ }

#[test]
fn entity_series_rejects_non_finite_or_negative_scores() { /* no NaN/negative */ }

#[test]
fn resource_limited_metric_carries_no_partial_series() { /* status final */ }
```

- [x] **Step 2: Подтвердить RED**

```bash
cargo test -p kronika-reader web_index::series --lib \
  --target aarch64-apple-darwin
```

- [x] **Step 3: Реализовать canonical codec**

Wire содержит actual observed range, grid, coverage, локальный dictionary,
metric revision/status/cutoff и series с `exact_score`, `max_bucket_value`,
presence mask и только `popcount(mask)` квантов. Constructor проверяет:

- dictionary sorted-unique;
- metrics sorted-unique;
- series sorted по `(score desc, entity_ref asc)`;
- refs внутри dictionary;
- `cutoff_score == 0` при `series_count < K`;
- non-complete metric не содержит series;
- observed range лежит внутри grid envelope.

- [x] **Step 4: Добавить property decoder coverage**

Arbitrary bytes должны возвращать typed error без panic. Generated valid
summary/series должны round-trip.

- [x] **Step 5: Подтвердить GREEN**

Run focused series tests и `overview::proptests` на native target.

### Task 3: Directory addressing и Zstd

**Files:**
- Modify: `crates/kronika-reader/Cargo.toml`
- Modify: `crates/kronika-reader/src/overview/block.rs`
- Modify: `crates/kronika-reader/src/overview/container.rs`
- Modify: `crates/kronika-analytics/src/overview/mod.rs`

**Interfaces:**
- `BlockKind::UiSummary` — обязательный baseline.
- `BlockKind::EntitySeries` — известный повторяемый kind.
- `BlockContent::{UiSummary, EntitySeries}`.
- `BlockContent::logical_id()` возвращает `view_code` для series.
- `FactFileReader::read_block(kind, logical_id)` читает ровно один block.
- Stored-body decode проверяет codec, CRC, stored/decoded bounds и exact
  decompressed length.

- [x] **Step 1: Написать failing container tests**

```rust
#[test]
fn canonical_file_contains_an_empty_ui_summary_baseline() {}

#[test]
fn two_entity_series_views_are_addressed_independently() {}

#[test]
fn duplicate_kind_and_logical_id_is_rejected() {}

#[test]
fn positional_read_of_one_view_does_not_read_another_view() {}

#[test]
fn zstd_round_trip_records_stored_and_decoded_lengths() {}

#[test]
fn oversized_zstd_decoded_len_is_rejected_before_decompression() {}
```

- [x] **Step 2: Подтвердить RED**

```bash
cargo test -p kronika-reader overview::container::tests --lib \
  --target aarch64-apple-darwin
```

- [x] **Step 3: Разделить baseline и known kinds**

Заменить предположение `BlockKind::ALL == baseline` на:

```rust
pub const BASELINE: [BlockKind; 10] = [/* existing nine + UiSummary */];
pub const KNOWN: [BlockKind; 11] = [/* baseline + EntitySeries */];
```

`FactFile::build` вставляет только missing baseline, разрешает несколько
`EntitySeries` с разными `logical_id` и сортирует по
`(kind, logical_id, min_ts)`.

- [x] **Step 4: Реализовать codec framing**

Добавить direct dependency `zstd`. Writer использует level 1 только при
`compressed_len + 64 < decoded_len`. Reader сначала проверяет CRC stored bytes,
затем bounded exact-length decompression. Unknown optional block не
декомпрессируется.

- [x] **Step 5: Реализовать selective read**

`read_block` ищет точную пару `(kind, logical_id)` и возвращает decoded body.
Stats учитывают один read, stored bytes и decoded bytes. Отсутствующий блок
возвращает `Ok(None)`, duplicate pair невозможна после admission.

- [x] **Step 6: Подтвердить GREEN**

Run container tests. Затем:

```bash
cargo test -p kronika-reader overview --lib \
  --target aarch64-apple-darwin
```

### Task 4: SegmentFacts и публичный контракт

**Files:**
- Modify: `crates/kronika-reader/src/overview/facts.rs`
- Modify: `crates/kronika-reader/src/overview/mod.rs`
- Modify: `crates/kronika-reader/README.md`
- Modify: `crates/kronika-reader/README.ru.md`

**Interfaces:**
- Existing `SegmentFacts::encode` получает `UiSummary` автоматически через
  baseline insertion.
- Public exports позволяют следующему projection-builder slice создать
  populated summary/series без доступа к private codec internals.

- [x] **Step 1: Написать failing integration test**

Cold extraction, encode, full admission и positional reload должны содержать
пустой валидный `UiSummary`, не читать PGM при reload и сохранять прежние
canonical facts.

- [x] **Step 2: Подтвердить RED**

```bash
cargo test -p kronika-reader \
  overview::facts::tests::fact_file_reload_matches_forced_raw_decode --lib \
  --target aarch64-apple-darwin
```

- [x] **Step 3: Довести wiring и exports**

Убрать exhaustive matches старого набора kinds, добавить validation новых
logical blocks и bump `FACT_SCHEMA_VERSION`, чтобы текущий writer/reader
имели одну identity.

- [x] **Step 4: Обновить README mirrors**

Описать sharding по view, selective read, local dictionary, missing-vs-zero и
точные hard bounds. Не ссылаться из rustdoc на `docs/`.

- [x] **Step 5: Подтвердить GREEN**

Run focused integration test и весь `kronika-reader` на native target.

### Task 5: Финальные проверки

**Files:**
- Review: весь staged diff.

- [x] **Step 1: Memory-bounds pass**

Для каждого constructor/decode/read зафиксировать peak:

- summary/series vectors ограничены отдельными `Bounds`;
- decompression ограничена `decoded_len`;
- compression input уже ограничен decoded block cap;
- selective read удерживает один stored и один decoded view block;
- нет map/vector, растущего от wire length без pre-check.

- [x] **Step 2: Comment-quality pass**

Удалить narration comments; оставить только format invariants, safety contract
и причины allocation order.

- [x] **Step 3: Formatting, lint, tests**

```bash
cargo fmt --all --check
cargo clippy -p kronika-reader --all-targets \
  --target aarch64-apple-darwin -- -D warnings
cargo test -p kronika-reader --target aarch64-apple-darwin
cargo run -p xtask --target aarch64-apple-darwin -- check-deps
git diff --check
```

- [x] **Step 4: Commit**

```bash
git add crates/kronika-analytics/src/overview/mod.rs \
  crates/kronika-reader docs/superpowers/plans/2026-07-28-web-index-implementation.md
git commit -m "feat: реализовать формат web-индекса OVF"
```

## Execution

План выполняется inline в этой сессии: пользователь уже запросил реализацию,
а parallel sub-agent execution для текущего режима не используется.
