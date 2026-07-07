# kronika-store Foundation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Заложить фундамент read-половины: storage-access крейт `kronika-store` (только `kronika-format`) + расширить `kronika-reader` единым PGM-unit декодом для `.pgm` и незапечатанных parts журнала, плюс `LocalDirSnapshot` над sealed+active. Без HTTP/S3/manifest/`section()`-merge/JSON.

**Architecture:** Ограничение `xtask check-deps` (archiver allow-list = `format/store/store-s3`) заставляет `kronika-store` зависеть ТОЛЬКО от `kronika-format` — он не декодирует, а отдаёт байты+каталог. Декод и query живут в `kronika-reader` (`+kronika-store`; web/dump это пускают, коллектор reader не тянет). Объединяющий примитив — трейт `ReadAt` в `kronika-format`: «прочитать N байт по абсолютному offset», реализованный для `std::fs::File` и `&[u8]`; на нём строятся и потоковый scanner журнала (пик памяти — один part), и декод sealed-файла, и декод in-memory part одним кодом.

**Tech Stack:** Rust edition 2024 / MSRV 1.96; `kronika-format` (контейнер PGM, `scan_journal`, `Catalog`, `crc32c`), `kronika-registry` (`decode_any`, `VerifiedSection`, caps), `kronika-reader` (существующий `Segment`), `std::os::unix::fs::FileExt` (`read_exact_at`).

## Global Constraints

- MSRV **1.96**, edition **2024**. Гейт (перед каждым коммитом): `export PATH="$HOME/.cargo/bin:$PATH"` затем `cargo fmt --all -- --check` && `cargo clippy --workspace --all-targets -- -D warnings` && `cargo test --workspace --exclude kronika-bdd` && `cargo run -p xtask -- check-deps`.
- **check-deps (жёстко):** `kronika-store` `[dependencies]` = только `kronika-format`. НЕ добавлять `kronika-registry`/`kronika-reader`/`kronika-writer` (уронит archiver). `kronika-reader` может получить `kronika-store` (ребро в allow-list web/dump; коллектор reader не использует).
- clippy: `#[derive(Eq)]` кроме структур с `f64`-полями (тогда только `PartialEq`).
- Комментарии/rustdoc/тесты — по-английски (конвенция проекта). Коммит-сообщения — по-русски, БЕЗ `Co-Authored-By`.
- **Не материализовать журнал целиком** — потоковый скан, пик памяти = один part ≤ `DEFAULT_MAX_PART_LEN` + resync-окно + directory of `PartRef`.
- Сохранить капы декода: `MAX_SECTION_BYTES=8MiB`, `MAX_SECTION_ROWS=65_536`, `MAX_ROW_GROUPS=16` (в `kronika-registry`).
- Не фабриковать данные: битая единица/кадр → пропуск + отчёт (damage/warning), не паника, не «нули».
- Тесты host-independent: фикстуры через `kronika_format::build_part` + framed `active.parts`; без живого PG (live-BDD по матрице — отдельно, в CI).

---

## Реестр реальных сигнатур (grounding, origin/main @ 0a4d149)

Используются в шагах ниже — verbatim из кода:

```rust
// kronika-format (re-exported at crates/kronika-format/src/lib.rs:28-52)
pub const MAGIC: [u8; 4];               // = *b"PGM1" (lib.rs:46)
pub const FORMAT_VERSION: u32;          // = 1 (lib.rs:52)
pub const FRAME_MAGIC: [u8; 4];         // = *b"PGMP" (parts.rs:12)
pub const FRAME_HEADER_LEN: usize;      // = 16 (parts.rs:15)
pub const DEFAULT_MAX_PART_LEN: u64;    // = 64<<20 (parts.rs:20)
pub const ENTRY_LEN: usize;             // = 32
pub const META_LEN: usize;              // = 40
pub const TAIL_INDEX_LEN: usize;        // = 8
pub fn crc32c(bytes: &[u8]) -> u32;
pub struct Entry { pub type_id: u32, pub flags: u32, pub offset: u64, pub len: u64, pub rows: u32, pub crc32c: u32 }
pub struct Catalog { pub entries: Vec<Entry>, pub min_ts: i64, pub max_ts: i64, pub source_id: u64, pub format_version: u32 }
impl Catalog { pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError>; pub const fn encoded_len(&self) -> usize; }
pub struct TailIndex { pub catalog_len: u32 }
impl TailIndex { pub fn decode(bytes: [u8; TAIL_INDEX_LEN]) -> Result<Self, DecodeError>; }
pub struct FrameHeader { pub part_len: u64 }
impl FrameHeader { pub fn decode(bytes: [u8; FRAME_HEADER_LEN]) -> Result<Self, FrameError>; }
pub struct PartRef { pub offset: usize, pub len: usize }        // offset = ПОСЛЕ frame header
pub struct JournalLimits { pub max_part_len: u64 }              // Default = DEFAULT_MAX_PART_LEN
pub struct ScanReport { pub parts: Vec<PartRef>, pub damages: Vec<DamageRegion>, pub valid_len: usize }
pub struct DamageRegion { pub from: usize, pub kind: DamageKind }
pub enum DamageKind { TornTail, Middle { resumed_at: usize }, QuarantinedTail }
pub fn scan_journal(bytes: &[u8], limits: JournalLimits) -> ScanReport;   // buffer scanner (parts.rs:412)
pub fn validate_part_catalog(bytes: &[u8]) -> Result<Catalog, PartError>; // framing+catalog, no body CRC (parts.rs:218)
pub fn validate_part(bytes: &[u8]) -> Result<Catalog, PartError>;         // + per-section CRC (parts.rs:189)
pub fn build_part(sections: &[SectionInput<'_>], meta: PartMeta) -> Vec<u8>;
pub struct SectionInput<'a> { pub type_id: u32, pub rows: u32, pub body: &'a [u8] }
pub struct PartMeta { pub min_ts: i64, pub max_ts: i64, pub source_id: u64 }

// kronika-registry
pub const DICT_STRINGS_TYPE_ID: u32; // 3_001_001
pub const DICT_BLOBS_TYPE_ID: u32;   // 3_002_001
pub const MAX_SECTION_BYTES: usize;  // 8<<20
pub fn decode_any(type_id: u32, section: VerifiedSection) -> Result<DecodedSection, CodecError>;
pub fn section_name(type_id: u32) -> Option<&'static str>;   // type_id → name (МНОГИЕ id на имя!)
pub const fn registry() -> &'static [TypeContract];
pub struct VerifiedSection(/*priv*/ Bytes);
impl VerifiedSection { pub fn verify(bytes: Bytes, expected: u32, crc32c: impl FnOnce(&[u8]) -> u32) -> Result<Self, CodecError>; }
pub struct DecodedSection { pub batches: Vec<RecordBatch>, pub stats: DecodeStats }
pub use bytes::Bytes; // re-exported (lib.rs:59)

// kronika-reader (existing)
pub struct Segment { /* file: File, catalog: Catalog */ }
impl Segment { pub fn open(path: &Path) -> Result<Self, ReadError>; pub const fn catalog(&self) -> &Catalog;
  pub fn decode(&self, entry: &Entry) -> Result<DecodedSection, ReadError>; pub fn dictionary(&self) -> Result<Dictionary, ReadError>; }
// PRIVATE to mirror: verified_body (lib.rs:210) — read entry.len at entry.offset (cap MAX_SECTION_BYTES) → VerifiedSection::verify(Bytes, entry.crc32c, crc32c)
// PRIVATE to mirror: read_catalog (lib.rs:393) — tail → catalog → magic/version/bounds
pub enum ReadError { Io(io::Error), TooSmall{len:u64}, BadMagic{actual:[u8;4]}, UnsupportedFormat{version:u32},
  SectionOutOfBounds{type_id:u32}, DictionarySection{type_id:u32}, Tail(DecodeError), BadCatalogLen{catalog_len:u32},
  Catalog(DecodeError), SectionTooLarge{len:u64}, Codec(CodecError) }
```

Приватное в writer (`journal.rs:373 fn scan_file`, читает через `read_exact_at`, пик — один part; `RESYNC_CHUNK=1<<20`) — НЕ реэкспортировано и открывает журнал на запись. Task 2 выносит чистую потоковую логику в `kronika-format` над `ReadAt`, writer делегирует, store использует read-only.

---

## Task 1: `ReadAt` в `kronika-format`

**Files:**
- Create: `crates/kronika-format/src/read_at.rs`
- Modify: `crates/kronika-format/src/lib.rs` (добавить `mod read_at;` + re-export)
- Test: в `read_at.rs` `#[cfg(test)]`

**Interfaces:**
- Produces: `pub trait ReadAt { fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> std::io::Result<()>; fn byte_len(&self) -> std::io::Result<u64>; }`, `impl ReadAt for std::fs::File`, `impl ReadAt for &[u8]`.

- [ ] **Step 1: Failing test** (`read_at.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::ReadAt;
    #[test]
    fn slice_reads_at_offset_and_reports_len() {
        let data: &[u8] = b"0123456789";
        assert_eq!(ReadAt::byte_len(&data).unwrap(), 10);
        let mut buf = [0u8; 3];
        data.read_exact_at(&mut buf, 4).unwrap();
        assert_eq!(&buf, b"456");
    }
    #[test]
    fn slice_read_past_end_errors() {
        let data: &[u8] = b"abc";
        let mut buf = [0u8; 4];
        assert!(data.read_exact_at(&mut buf, 0).is_err());
        assert!(data.read_exact_at(&mut buf, 3).is_err());
    }
    #[test]
    fn file_reads_at_offset() {
        use std::io::Write;
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"hello world").unwrap();
        let file = std::fs::File::open(f.path()).unwrap();
        assert_eq!(ReadAt::byte_len(&file).unwrap(), 11);
        let mut buf = [0u8; 5];
        file.read_exact_at(&mut buf, 6).unwrap();
        assert_eq!(&buf, b"world");
    }
}
```
Add `tempfile = "3"` to `[dev-dependencies]` of `kronika-format` if absent.

- [ ] **Step 2: Run, expect fail** — `export PATH="$HOME/.cargo/bin:$PATH"; cargo test -p kronika-format read_at` → FAIL (`ReadAt` undefined).

- [ ] **Step 3: Implement** (`read_at.rs`):

```rust
//! Positional byte-source abstraction shared by the journal scanner and the
//! segment/part decoders: read exactly `buf.len()` bytes at an absolute offset.
use std::io;

pub trait ReadAt {
    /// Reads exactly `buf.len()` bytes starting at `offset`. Errors on short read.
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
    /// Total length of the source in bytes.
    fn byte_len(&self) -> io::Result<u64>;
}

impl ReadAt for std::fs::File {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        std::os::unix::fs::FileExt::read_exact_at(self, buf, offset)
    }
    fn byte_len(&self) -> io::Result<u64> { Ok(self.metadata()?.len()) }
}

impl ReadAt for &[u8] {
    fn read_exact_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        let end = start.checked_add(buf.len()).ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))?;
        if end > self.len() { return Err(io::Error::from(io::ErrorKind::UnexpectedEof)); }
        buf.copy_from_slice(&self[start..end]);
        Ok(())
    }
    fn byte_len(&self) -> io::Result<u64> { Ok(self.len() as u64) }
}
```
Add to `lib.rs`: `mod read_at;` + `pub use read_at::ReadAt;`.

- [ ] **Step 4: Run, expect pass** — `cargo test -p kronika-format read_at` → PASS.
- [ ] **Step 5: Gate + commit** — run full gate; `git add crates/kronika-format; git commit -m "kronika-format: трейт ReadAt для позиционного чтения"`.

---

## Task 2: Потоковый scanner журнала в `kronika-format` над `ReadAt`

**Files:**
- Modify: `crates/kronika-format/src/parts.rs` (добавить `scan_journal_streaming`)
- Modify: `crates/kronika-format/src/lib.rs` (re-export)
- Modify: `crates/kronika-writer/src/journal.rs` (`scan_file` делегирует в format)
- Test: в `parts.rs` `#[cfg(test)]` (parity vs `scan_journal`)

**Interfaces:**
- Consumes: `ReadAt` (Task 1), `scan_journal`/`ScanReport`/`JournalLimits`/`PartRef`/`DamageKind` (existing).
- Produces: `pub fn scan_journal_streaming<R: ReadAt>(reader: &R, limits: JournalLimits, resync_chunk: usize) -> std::io::Result<ScanReport>` — тот же результат, что `scan_journal(&whole_buffer, limits)`, но читает по кадрам (пик памяти = один part).

- [ ] **Step 1: Failing parity test** (`parts.rs`):

```rust
#[cfg(test)]
mod streaming_tests {
    use super::*;
    fn framed(parts: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for p in parts {
            out.extend_from_slice(&FrameHeader { part_len: p.len() as u64 }.encode());
            out.extend_from_slice(p);
        }
        out
    }
    fn sample_part() -> Vec<u8> {
        build_part(&[SectionInput { type_id: 1_006_001, rows: 0, body: b"" }],
                   PartMeta { min_ts: 1, max_ts: 2, source_id: 7 })
    }
    #[test]
    fn streaming_matches_buffer_on_clean_journal() {
        let p = sample_part();
        let buf = framed(&[&p, &p]);
        let want = scan_journal(&buf, JournalLimits::default());
        let got = scan_journal_streaming(&buf.as_slice(), JournalLimits::default(), 1 << 20).unwrap();
        assert_eq!(got, want);
    }
    #[test]
    fn streaming_matches_buffer_on_torn_tail() {
        let p = sample_part();
        let mut buf = framed(&[&p]);
        buf.extend_from_slice(&FrameHeader { part_len: 999 }.encode()); // header for absent body
        let want = scan_journal(&buf, JournalLimits::default());
        let got = scan_journal_streaming(&buf.as_slice(), JournalLimits::default(), 1 << 20).unwrap();
        assert_eq!(got, want);
    }
    #[test]
    fn streaming_matches_buffer_on_middle_corruption() {
        let p = sample_part();
        let mut buf = framed(&[&p]);
        buf.extend_from_slice(&[0xFF; 8]);        // garbage between valid frames
        buf.extend_from_slice(&framed(&[&p]));
        let want = scan_journal(&buf, JournalLimits::default());
        let got = scan_journal_streaming(&buf.as_slice(), JournalLimits::default(), 1 << 20).unwrap();
        assert_eq!(got, want);
    }
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p kronika-format streaming` → FAIL (`scan_journal_streaming` undefined).

- [ ] **Step 3: Implement `scan_journal_streaming`** in `parts.rs`. Mirror the existing buffer `scan_journal` state machine, but source bytes via `reader.read_exact_at`: read `FRAME_HEADER_LEN` at the cursor → `FrameHeader::decode`; on bad magic/crc or `part_len > limits.max_part_len` or `cursor+header+part_len > byte_len` classify exactly as `scan_journal` does (`Torn` at tail vs `Middle`/`QuarantinedTail`), advancing via a bounded resync window of `resync_chunk` bytes (re-read chunks, search for `FRAME_MAGIC`, overlap `FRAME_MAGIC.len()-1`). For a valid frame read the one part body into a single reused `Vec<u8>` sized to `part_len`, `validate_part(&body)`; on `Ok` push `PartRef { offset: cursor+FRAME_HEADER_LEN, len: part_len }`. Keep peak memory to one part + resync window. Return `ScanReport { parts, damages, valid_len }` identical to the buffer scanner. Refactor `scan_journal` and `scan_journal_streaming` to share the classification helper if practical (DRY), else keep the state machine parallel and rely on the parity tests. Re-export `pub use parts::scan_journal_streaming;` in `lib.rs`.

- [ ] **Step 4: Run, expect pass** — `cargo test -p kronika-format streaming` → PASS.

- [ ] **Step 5: Writer delegates (DRY)** — in `crates/kronika-writer/src/journal.rs` replace the body of private `scan_file` with a call to `kronika_format::scan_journal_streaming(file, limits, RESYNC_CHUNK)` (keeping `Journal::open`'s torn-tail `set_len` behaviour around it). Keep the existing `streaming_scan_matches_the_buffer_scan` test green.

- [ ] **Step 6: Gate + commit** — full gate (incl. `cargo test -p kronika-writer`); `git commit -m "kronika-format: потоковый scan_journal_streaming над ReadAt, writer делегирует"`.

---

## Task 3: `kronika-store` — storage-access над `LocalDir`

**Files:**
- Modify: `crates/kronika-store/Cargo.toml` (`[dependencies] kronika-format = { path = "../kronika-format" }`; `[dev-dependencies] kronika-writer`, `tempfile`)
- Modify: `crates/kronika-store/src/lib.rs` (mod + re-export)
- Create: `crates/kronika-store/src/source.rs` (типы единиц)
- Create: `crates/kronika-store/src/local.rs` (`LocalDir`)
- Test: `local.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `kronika_format::{ReadAt, Catalog, TailIndex, scan_journal_streaming, JournalLimits, PartRef, ScanReport, MAGIC, TAIL_INDEX_LEN, crc32c, validate_part_catalog}`.
- Produces:
```rust
pub struct SealedUnit { pub path: PathBuf, pub catalog: Catalog }   // cheap tail-read, NO body decode
pub struct ActivePart  { pub part: PartRef, pub catalog: Catalog }  // from journal scan + validate_part_catalog
pub struct LocalScan { pub sealed: Vec<SealedUnit>, pub active: Vec<ActivePart>, pub damages: Vec<kronika_format::DamageRegion>, pub warnings: Vec<StoreWarning> }
pub struct StoreWarning { pub path: PathBuf, pub reason: String }   // skipped unit (widened; not ReadError)
pub struct LocalDir { root: PathBuf }
impl LocalDir {
    pub fn open(root: &Path) -> std::io::Result<Self>;
    pub fn scan(&self) -> std::io::Result<LocalScan>;              // list *.pgm cheap-catalog + scan active.parts
    pub fn open_sealed(&self, u: &SealedUnit) -> std::io::Result<std::fs::File>; // raw byte source (ReadAt)
    pub fn read_active_part(&self, p: &ActivePart) -> std::io::Result<Vec<u8>>;   // one part bytes (bounded)
}
pub fn read_catalog<R: kronika_format::ReadAt>(reader: &R) -> Result<Catalog, StoreError>; // tail→catalog, magic/version/bounds
pub enum StoreError { Io(std::io::Error), TooSmall, BadMagic, UnsupportedFormat{version:u32}, BadCatalogLen, Catalog(kronika_format::DecodeError), OutOfBounds }
```

- [ ] **Step 1: Failing test** (`local.rs`) — build a temp dir with one sealed `.pgm` (via `kronika_format::build_part` written to `1000.pgm`) and an `active.parts` with two framed parts; assert `scan()` returns 1 sealed + 2 active with correct `source_id/min_ts/max_ts`, no whole-file allocation of the journal:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kronika_format::{build_part, SectionInput, PartMeta, FrameHeader};
    fn part(ts: i64, src: u64) -> Vec<u8> {
        build_part(&[SectionInput { type_id: 1_006_001, rows: 0, body: b"" }],
                   PartMeta { min_ts: ts, max_ts: ts + 1, source_id: src })
    }
    #[test]
    fn scan_lists_sealed_and_active_with_cheap_catalog() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1000.pgm"), part(1000, 7)).unwrap();
        let mut journal = Vec::new();
        for p in [part(2000, 7), part(3000, 7)] {
            journal.extend_from_slice(&FrameHeader { part_len: p.len() as u64 }.encode());
            journal.extend_from_slice(&p);
        }
        std::fs::write(dir.path().join("active.parts"), &journal).unwrap();
        let scan = LocalDir::open(dir.path()).unwrap().scan().unwrap();
        assert_eq!(scan.sealed.len(), 1);
        assert_eq!(scan.sealed[0].catalog.min_ts, 1000);
        assert_eq!(scan.active.len(), 2);
        assert_eq!(scan.active[1].catalog.min_ts, 3000);
        assert!(scan.warnings.is_empty());
    }
    #[test]
    fn corrupt_sealed_is_skipped_with_warning_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("1000.pgm"), part(1000, 7)).unwrap();
        std::fs::write(dir.path().join("bad.pgm"), b"not a pgm").unwrap();
        let scan = LocalDir::open(dir.path()).unwrap().scan().unwrap();
        assert_eq!(scan.sealed.len(), 1);           // good one served
        assert_eq!(scan.warnings.len(), 1);         // bad one warned, not fatal
    }
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p kronika-store` → FAIL (types undefined).
- [ ] **Step 3: Implement** `read_catalog` (mirror reader's `read_catalog` over `ReadAt`: read last `TAIL_INDEX_LEN` → `TailIndex::decode` → read catalog block → `Catalog::decode` → check `MAGIC` at 0, `format_version==FORMAT_VERSION`, entry bounds), `SealedUnit`/`ActivePart`/`LocalScan`/`StoreWarning`/`StoreError`, and `LocalDir` (`open`, `scan` = glob `*.pgm` sorted + `read_catalog` each [skip+warn on error], open `active.parts` if present + `scan_journal_streaming` + `validate_part_catalog` per `PartRef`, `open_sealed`, `read_active_part`). Store depends on `kronika-format` ONLY.
- [ ] **Step 4: Run, expect pass** — `cargo test -p kronika-store` → PASS.
- [ ] **Step 5: Gate + commit** — full gate incl. `cargo run -p xtask -- check-deps` (verify store pulls only format); `git commit -m "kronika-store: LocalDir storage-access — sealed каталоги + потоковый скан active.parts"`.

---

## Task 4: `kronika-reader` — `PgmUnit` (единый декод sealed-файла и in-memory part)

**Files:**
- Modify: `crates/kronika-reader/Cargo.toml` (`[dependencies] kronika-store = { path = "../kronika-store" }`)
- Create: `crates/kronika-reader/src/unit.rs` (`PgmUnit`)
- Modify: `crates/kronika-reader/src/lib.rs` (re-export `PgmUnit`; `Segment` делегирует в `PgmUnit` — DRY)
- Test: `unit.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `ReadAt`, `Catalog`, `Entry`, `crc32c` (format); `decode_any`, `VerifiedSection`, `DecodedSection`, `Bytes`, `MAX_SECTION_BYTES`, `DICT_STRINGS_TYPE_ID`, `DICT_BLOBS_TYPE_ID` (registry); `ReadError` (existing).
- Produces:
```rust
pub struct PgmUnit<R: ReadAt> { reader: R, catalog: Catalog }
impl<R: ReadAt> PgmUnit<R> {
    pub fn open(reader: R) -> Result<Self, ReadError>;              // read_catalog over ReadAt
    pub const fn catalog(&self) -> &Catalog;
    pub fn decode(&self, entry: &Entry) -> Result<DecodedSection, ReadError>; // verified_body + decode_any
    pub fn dictionary(&self) -> Result<Dictionary, ReadError>;
}
```

- [ ] **Step 1: Failing test** (`unit.rs`) — the SAME part bytes decode identically whether opened as a sealed file OR as an in-memory `&[u8]`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use kronika_format::{build_part, SectionInput, PartMeta};
    fn a_part() -> Vec<u8> {
        // pick a real zero-column-free snapshot type with 0 rows for a minimal valid section,
        // or a dict-free snapshot section from the registry; here archiver singleton 1_008_00x.
        build_part(&[SectionInput { type_id: 1_008_001, rows: 0, body: /* real encoded empty body */ b"" }],
                   PartMeta { min_ts: 5, max_ts: 6, source_id: 1 })
    }
    #[test]
    fn same_bytes_decode_via_file_and_memory() {
        let bytes = a_part();
        // in-memory
        let mem = PgmUnit::open(bytes.as_slice()).unwrap();
        // file
        let f = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(f.path(), &bytes).unwrap();
        let file = PgmUnit::open(std::fs::File::open(f.path()).unwrap()).unwrap();
        assert_eq!(mem.catalog(), file.catalog());
        let e = &mem.catalog().entries[0];
        assert_eq!(mem.decode(e).unwrap().stats.rows, file.decode(e).unwrap().stats.rows);
    }
    #[test]
    fn corrupt_body_fails_crc_before_decode() {
        let mut bytes = a_part();
        let off = bytes.len() / 2; bytes[off] ^= 0xFF;          // flip a body byte
        let unit = PgmUnit::open(bytes.as_slice());
        // open may still succeed (catalog intact); decode of the section must fail on CRC
        if let Ok(u) = unit { let e = u.catalog().entries[0]; assert!(u.decode(&e).is_err()); }
    }
}
```
(Implementer: replace `a_part()`'s section with a real minimal valid section body from an existing golden test in `kronika-registry`/`kronika-reader` — reuse the exact type_id + encoded body the current `Segment` tests use.)

- [ ] **Step 2: Run, expect fail** — `cargo test -p kronika-reader unit` → FAIL.
- [ ] **Step 3: Implement `PgmUnit`** — `read_catalog<R: ReadAt>` (reuse/mirror the existing private `read_catalog`, now generic over `ReadAt`), `verified_body<R: ReadAt>` (read `entry.len` at `entry.offset`, cap `MAX_SECTION_BYTES` → `SectionTooLarge`, `VerifiedSection::verify(Bytes::from(body), entry.crc32c, kronika_format::crc32c)`), `decode` (reject dict type_ids → `DictionarySection`, else `decode_any(entry.type_id, verified).map_err(ReadError::Codec)`), `dictionary` (scan catalog for the two dict type_ids, verified_body each, build `Dictionary`). Then **refactor `Segment` to hold `PgmUnit<File>`** and delegate `open/catalog/decode/dictionary` to it (DRY — one decode path). Keep all existing `Segment` tests green. Add `pub use unit::PgmUnit;`.
- [ ] **Step 4: Run, expect pass** — `cargo test -p kronika-reader` (unit + existing Segment tests) → PASS.
- [ ] **Step 5: Gate + commit** — full gate incl. `check-deps` (verify reader→store edge accepted for web/dump, collector untouched); `git commit -m "kronika-reader: PgmUnit — единый декод sealed-файла и in-memory part через ReadAt"`.

---

## Task 5: `kronika-reader` — `LocalDirSnapshot` над sealed+active

**Files:**
- Create: `crates/kronika-reader/src/snapshot.rs` (`LocalDirSnapshot`)
- Modify: `crates/kronika-reader/src/lib.rs` (re-export)
- Test: `snapshot.rs` `#[cfg(test)]`

**Interfaces:**
- Consumes: `kronika_store::{LocalDir, SealedUnit, ActivePart, LocalScan}`, `PgmUnit` (Task 4).
- Produces:
```rust
pub struct Unit { pub source_id: u64, pub min_ts: i64, pub max_ts: i64, pub live: bool, /* handle to decode */ }
pub struct LocalDirSnapshot { /* LocalDir + resolved LocalScan */ }
impl LocalDirSnapshot {
    pub fn open(root: &Path) -> std::io::Result<Self>;
    pub fn refresh(&mut self) -> std::io::Result<()>;         // re-scan (journal tail + new .pgm)
    pub fn units(&self) -> Vec<UnitMeta>;                     // sealed + active, deduped
    pub fn decode_unit(&self, idx: usize, entry: &Entry) -> Result<DecodedSection, ReadError>;
    pub fn warnings(&self) -> &[kronika_store::StoreWarning];
}
```
Snapshot-order rule (must-fix #1, minimal-safe form for this PR): **scan the journal FIRST, then list sealed `.pgm`**, so a part sealed between the two reads appears as a sealed unit (never lost); dedup an active part only when a sealed unit of the same `source_id` covers `[min_ts,max_ts]`. Do NOT implement `section()` merge/cursor here.

- [ ] **Step 1: Failing tests** (`snapshot.rs`):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // reuse fixture helpers: write sealed .pgm + framed active.parts into a tempdir.
    #[test]
    fn live_part_is_visible_before_seal() {
        // dir with ONLY active.parts (no .pgm) → units() shows the live part(s)
    }
    #[test]
    fn sealed_covering_part_is_deduped_no_double() {
        // sealed .pgm covering [1000,2000] + active part covering same range, same source
        // → units() contains the sealed one, the covered live part is dropped (no duplicate)
    }
    #[test]
    fn refresh_picks_up_appended_part() {
        // open; append a frame to active.parts; refresh(); new part visible
    }
    #[test]
    fn middle_corruption_reported_rest_served() {
        // active.parts: valid, garbage, valid → both valid parts visible, damage in report
    }
}
```

- [ ] **Step 2: Run, expect fail** — `cargo test -p kronika-reader snapshot` → FAIL.
- [ ] **Step 3: Implement** `LocalDirSnapshot` (scan journal→sealed order; build `Unit` list from `LocalScan`; dedup covered active parts by `(source_id, [min_ts,max_ts] ⊆ sealed)`; `decode_unit` opens the right `PgmUnit` (sealed `File` via `LocalDir::open_sealed`, or active part bytes via `LocalDir::read_active_part` as `&[u8]`) and delegates; `refresh` re-runs scan). NO `section()`/cursor/rows-merge.
- [ ] **Step 4: Run, expect pass** — `cargo test -p kronika-reader snapshot` → PASS.
- [ ] **Step 5: Gate + commit** — full gate; `git commit -m "kronika-reader: LocalDirSnapshot над sealed+active, устойчивый порядок и dedup покрытого part"`.

---

## Task 6: Документы и границы

**Files:**
- Modify: `docs/architecture.md` (роль `kronika-store` = storage-access; `kronika-reader` = query-core с PGM-unit + directory snapshot; ребро `reader → store`)
- Modify: `crates/kronika-store/src/lib.rs` (rustdoc: storage-access only, format-only deps), `crates/kronika-reader/src/lib.rs` (rustdoc: + PgmUnit/LocalDirSnapshot), `crates/kronika-store/README.md` / `crates/kronika-reader/README.md` если есть
- Modify: (если нужно) заметка в `docs/segment-format.md` — без изменений формата

- [ ] **Step 1:** Обновить `docs/architecture.md` раздел про store/reader под фактические зависимости (store: только `kronika-format`; reader: `+kronika-store`; коллектор reader/store не тянет; archiver — только storage-access). Одна-две правки rustdoc-шапок крейтов.
- [ ] **Step 2: Полный гейт** — `export PATH="$HOME/.cargo/bin:$PATH"; cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace --exclude kronika-bdd && cargo run -p xtask -- check-deps`. Ожидание: всё зелёное, check-deps ok (4 бинаря).
- [ ] **Step 3: Commit** — `git commit -m "docs: границы kronika-store (storage-access) и kronika-reader (query-core) под check-deps"`.

---

## Явные deferrals (НЕ в этом PR)

`section()` global merge + cursor контракт; `name→type_id` multi-version резолв и row/JSON API; `manifest.idx`; S3/`kronika-store-http`/`kronika-store-s3`; `pg_kronika-web` endpoints; cross-segment `str_id` LRU; rate/anomaly; live-BDD по PG-матрице (добавить сценарий «данные видны из active.parts до seal, затем из sealed без дублей» — позже, когда появится query-слой).
