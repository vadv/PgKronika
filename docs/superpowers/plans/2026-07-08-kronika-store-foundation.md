# План чтения локальных PGM-данных

## Цель

Подготовить read-path для локальных PGM-данных:

- `kronika-format`: общий `ReadAt` и потоковый scan `active.parts`.
- `kronika-store`: storage-access над локальной директорией без декода секций.
- `kronika-reader`: единый декод PGM-контейнера через `PgmUnit` и снимок
  `LocalDirSnapshot` над sealed-сегментами и live-частями журнала.

Этот срез не добавляет HTTP, S3, `manifest.idx`, row/JSON API, cursor-контракт и
слияние строк секции через несколько единиц.

## Границы крейтов

- `kronika-store` зависит только от `kronika-format`.
- `kronika-store` читает каталоги и байты, но не декодирует секции через
  `kronika-registry`.
- `kronika-reader` зависит от `kronika-store`, `kronika-format` и
  `kronika-registry`; в нём живёт декод секций и словарей.
- `pg_kronika-collector` не тянет `kronika-store` и `kronika-reader`.
- `pg_kronika-archiver` может использовать `kronika-store` как storage-access без
  зависимости от registry/reader.

`xtask check-deps` должен подтверждать эти границы.

## Memory Bounds

- Journal scan не загружает `active.parts` целиком.
- Пик streaming scan: один part, один catalog, resync-окно и `Vec<PartRef>`.
- `JournalLimits::max_part_len` ограничивает размер part до выделения памяти.
- `read_catalog` ограничивает catalog block перед allocation.
- `PgmUnit::decode` применяет `MAX_SECTION_BYTES` перед чтением тела секции.
- Директории и список `PartRef` остаются пропорциональны числу найденных единиц;
  это индекс, а не загрузка тел секций.

## Work Items

### 1. `ReadAt` в `kronika-format`

Добавить позиционное чтение:

```rust
pub trait ReadAt {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()>;
    fn byte_len(&self) -> std::io::Result<u64>;
}
```

Реализации: `std::fs::File` и `&[u8]`. Ошибка короткого чтения возвращается как
`UnexpectedEof`.

### 2. Streaming scan журнала

Добавить:

```rust
pub fn scan_journal_streaming<R: ReadAt>(
    reader: &R,
    limits: JournalLimits,
    resync_chunk: usize,
) -> std::io::Result<ScanReport>;
```

Функция должна выдавать тот же `ScanReport`, что и `scan_journal(&bytes, limits)`,
но читать кадры через `ReadAt`. Повреждения классифицируются теми же
`DamageKind`: `TornTail`, `Middle`, `QuarantinedTail`.

`kronika-writer` использует этот scanner при recovery `active.parts`; отдельная
потоковая логика в writer не нужна.

### 3. `kronika-store::LocalDir`

Добавить storage-access API:

```rust
pub struct SealedUnit { pub path: PathBuf, pub catalog: Catalog }
pub struct ActivePart { pub part: PartRef, pub catalog: Catalog }
pub struct LocalScan {
    pub sealed: Vec<SealedUnit>,
    pub active: Vec<ActivePart>,
    pub damages: Vec<DamageRegion>,
    pub warnings: Vec<StoreWarning>,
}
pub struct LocalDir { /* root */ }
```

`LocalDir::scan`:

- scans `active.parts` before listing sealed `.pgm` files;
- uses `scan_journal_streaming` for the journal;
- reads one bounded part to attach its catalog;
- reads only tail + catalog for sealed files;
- records bad sealed files as `StoreWarning`;
- returns journal damages separately from file warnings.

`LocalDir::open_sealed` returns raw file bytes. `LocalDir::read_active_part`
returns one bounded part body.

### 4. `kronika-reader::PgmUnit`

Добавить generic decoder:

```rust
pub struct PgmUnit<R: ReadAt> { /* reader + catalog */ }
```

`PgmUnit::open` reads the end catalog from any `ReadAt`. `decode` reads one
bounded section body, verifies CRC, and calls `kronika_registry::decode_any`.
`dictionary` reads `dict.strings` and `dict.blobs`.

`Segment` delegates to `PgmUnit<File>` so sealed files and in-memory journal parts
share one decode path.

### 5. `kronika-reader::LocalDirSnapshot`

Добавить read view over sealed + live units:

```rust
pub struct UnitMeta {
    pub source_id: u64,
    pub min_ts: i64,
    pub max_ts: i64,
    pub live: bool,
}
```

`LocalDirSnapshot` owns a `LocalDir` and the latest `LocalScan`. `units()` returns
sealed units first, then live parts whose catalog does not exactly match a sealed
unit catalog. `refresh()` rescans the directory and journal. `warnings()` exposes
skipped-file and skipped-part warnings; `damages()` exposes journal damage
regions. `decode_unit` opens the selected sealed file or active part and
delegates to `PgmUnit`.

## Tests

Required coverage:

- `ReadAt` reads files and slices at an absolute offset and errors on short read.
- streaming journal scan matches buffer scan for clean, torn-tail, and middle
  corruption cases.
- `LocalDir::scan` returns sealed catalogs, active parts, warnings for bad
  `.pgm`, and journal damages.
- `read_catalog` covers short source, bad tail, bad magic, bad format version,
  bad catalog length, corrupt catalog, out-of-bounds section, and happy path.
- `PgmUnit` decodes the same bytes from `File` and `&[u8]`.
- `LocalDirSnapshot` exposes live parts before seal, deduplicates exact
  sealed/live catalog matches, keeps overlapping distinct live parts visible,
  exposes journal damages, sees appended parts after `refresh`, and keeps valid
  parts around a damaged journal region.

## Verification

For Rust changes in this plan:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -p xtask -- check-deps
```

`kronika-bdd` не меняется.
