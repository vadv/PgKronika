# kronika-reader

[Русская версия](README.ru.md)

`kronika-reader` opens a sealed PGM segment and decodes its sections. It starts
from the end catalog, then reads section bodies by the ranges stored there. The
API uses positional reads and applies size limits before allocation.

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

## Not Implemented Yet

Time-range queries, drill-down queries, the cross-segment `str_id` cache, and
on-demand `dict.blobs` reads are left for later steps.
