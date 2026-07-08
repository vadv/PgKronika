# kronika-store Foundation Design

Дата: 2026-07-06. Обновлено: 2026-07-08.

## Назначение

`kronika-store` даёт read-only storage-access к локальной директории PGM-данных:

- sealed `.pgm` files;
- live `active.parts` journal;
- decoded catalogs for both sources;
- raw byte reads for a selected sealed file or active part.

The crate does not decode registry sections and does not expose row, JSON,
HTTP, S3, cursor, rate, or anomaly APIs. Section decoding belongs to
`kronika-reader`.

## Workspace Contract

- `kronika-store` depends only on `kronika-format`.
- `kronika-reader` depends on `kronika-store`, `kronika-format`, and
  `kronika-registry`.
- `pg_kronika-collector` writes local data and does not depend on store/reader.
- `pg_kronika-archiver` can use store as storage-access without registry or
  reader.

This split keeps storage discovery independent from section schemas.

## Data Sources

### Sealed `.pgm`

For each sealed file, store reads only:

1. tail index;
2. catalog block before the tail index;
3. magic and format-version fields needed to validate the container;
4. catalog entry bounds.

Section bodies are not loaded while listing files.

### `active.parts`

`active.parts` is an append-only journal of framed PGM parts. Each part is a
self-contained PGM container with its own catalog and dictionary sections.

Store scans the journal with `scan_journal_streaming`, which reads one frame at a
time through `ReadAt`. Valid parts become `ActivePart { part, catalog }`.
Damaged regions are returned as `DamageRegion`.

## Public Model

```rust
pub struct SealedUnit {
    pub path: PathBuf,
    pub catalog: Catalog,
}

pub struct ActivePart {
    pub part: PartRef,
    pub catalog: Catalog,
}

pub struct LocalScan {
    pub sealed: Vec<SealedUnit>,
    pub active: Vec<ActivePart>,
    pub damages: Vec<DamageRegion>,
    pub warnings: Vec<StoreWarning>,
}

pub struct StoreWarning {
    pub path: PathBuf,
    pub reason: String,
}
```

`StoreWarning` means one file or part was skipped. `DamageRegion` describes
journal bytes that the frame scanner could not validate.

## LocalDir Scan

`LocalDir::scan` uses journal-first ordering:

1. Scan `active.parts`, if present.
2. Attach catalogs to valid active parts.
3. List sealed `.pgm` files.
4. Read sealed catalogs.
5. Return `LocalScan`.

The order covers seal races under the writer contract: before journal reset, the
part is visible in `active.parts`; after seal, the sealed file is visible in the
`.pgm` listing. The reader layer handles duplicate live parts.

Bad sealed files do not fail the whole scan. They produce `StoreWarning` and the
scan continues with other files.

## Reader Integration

`kronika-reader::PgmUnit<R>` decodes one PGM container over any `ReadAt` source:

- `File` for sealed segments;
- `&[u8]` for active journal parts already read into a bounded buffer.

`Segment` delegates to `PgmUnit<File>`. `LocalDirSnapshot` combines
`LocalDir::scan` output into one visible list:

- sealed units first;
- live parts after sealed units;
- live parts dropped when a sealed unit with the same `source_id` fully covers
  `[min_ts, max_ts]`.

`LocalDirSnapshot` is a foundation read view. It exposes unit metadata and
per-unit decode. It does not merge rows across units or provide cursor paging.

## Memory Bounds

- Journal scan reads one part body at a time.
- `JournalLimits::max_part_len` caps part allocation.
- `read_catalog` caps the catalog block before allocation.
- `PgmUnit::decode` caps section bodies with `MAX_SECTION_BYTES`.
- Store does not load sealed section bodies during discovery.
- The in-memory index grows with the number of sealed files and valid active
  parts, not with total segment body bytes.

## Corruption Semantics

- Incomplete final journal frame: `DamageKind::TornTail`.
- Corrupt frame followed by a valid frame: `DamageKind::Middle`.
- Corrupt final region with no later valid frame: `DamageKind::QuarantinedTail`.
- Bad sealed file: `StoreWarning`.

Store surfaces these signals. Query-level gaps are outside this foundation
layer; they require row/window semantics above `LocalDirSnapshot`.

## Storage Layout Context

`docs/segment-format.md` defines hourly placement:

```text
{root}/{cluster_id}/{yyyy}/{mm}/{dd}/{hh}/{first_ts}.pgm
{root}/{cluster_id}/{yyyy}/{mm}/{dd}/{hh}/manifest.idx
```

This foundation PR reads the local directory and sealed file tails. `manifest.idx`
and remote backends are separate storage-discovery work.

## Verification

Relevant checks:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- check-deps
```
