# kronika-reader

[Русская версия](README.ru.md)

`kronika-reader` is the read core for PGM segments. `PgmUnit` decodes a PGM
container over any `ReadAt` source — a sealed file or an in-memory journal part —
through a single path. `LocalDirSnapshot` combines sealed segments with live
`active.parts` entries, suppresses exact sealed/live duplicates, and exposes
scan diagnostics.

## Opening a Segment

`Segment::open(path)` reads the last 8 bytes to find the catalog length, reads
the catalog block before them, and verifies its CRC. A `catalog_len` above the
cap or outside the file is rejected before allocation. `Segment::catalog()`
returns the decoded entries; section bodies are read on demand.

## Decoding a Section

`Segment::decode(entry)` reads the body at `entry.offset` (bounded by
`MAX_SECTION_BYTES`), verifies it against `entry.crc32c`, and decodes it through
`kronika_registry::decode_any`.

CRC is checked before bytes reach the Parquet parser. The reader supplies
`kronika_format::crc32c` to `VerifiedSection::verify`; the registry crate stays
independent of `kronika-format`.

## Resolving Strings

Snapshot columns store `str_id`, not string bytes. `Segment::dictionary()` reads
`dict.strings` and `dict.blobs` into a `str_id -> value` map. Dictionary sections
are CRC-checked and use the same row and row-group limits as data sections.

`Dictionary::resolve(str_id)` returns either a full string value or a blob value
with `full_len` and `truncated`. That keeps a stored prefix distinct from the
original full value. If the same id appears as both a string and a blob across
parts, the blob wins.

## Local Directory Snapshots

`LocalDirSnapshot::units()` returns sealed units first and then live journal
parts. A live part is hidden only when its catalog exactly matches a sealed unit
catalog. Time-range overlap does not prove that the live part was finalized.

`warnings()` returns skipped-file and skipped-part warnings. `damages()` returns
typed `DamageRegion` values for corrupt `active.parts` byte ranges; valid parts
around a damaged region remain visible.

## Scope Boundaries

This crate does not provide time-range queries, drill-down queries, a
cross-segment `str_id` cache, or on-demand `dict.blobs` reads. Those APIs belong
above `Segment`, `PgmUnit`, and `LocalDirSnapshot`.
