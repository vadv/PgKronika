# kronika-reader

[Русская версия](README.ru.md)

`kronika-reader` opens a sealed PGM segment and decodes its sections. It reads
the end catalog from the file tail, then reads each section body by its catalog
range — positional and bounded, never the whole file (segment-format.md,
"Reading from S3").

## Opening a Segment

`Segment::open(path)` reads the last 8 bytes (the tail index) for the catalog
length, reads the catalog block before it, and verifies the catalog CRC. The
catalog is the segment tail and is bounded: a `catalog_len` above the cap or
outside the file is a corrupt tail and is rejected before anything is allocated.
`Segment::catalog()` returns the decoded entries; section bodies are read on
demand.

## Decoding a Section

`Segment::decode(entry)` reads the body at `entry.offset` (bounded by
`MAX_SECTION_BYTES`), verifies it against `entry.crc32c`, and decodes it through
`kronika_registry::decode_any`.

This is the production use of the registry's CRC trust boundary: the reader
injects `kronika_format::crc32c` into `VerifiedSection::verify`, so a tampered or
rotted body is rejected before the Parquet parser sees it, while the registry
keeps no dependency on the format crate (the option-C split).

## Resolving Strings

Snapshot columns store a `str_id`, not the string. `Segment::dictionary()` reads
the segment's `dict.strings` and `dict.blobs` sections (each CRC-verified, and
held to the same row/row-group caps as data sections) into a `str_id` -> value
map. `Dictionary::resolve(str_id)` returns a `Resolved`: a `String` for a
`dict.strings` value, or a `Blob` carrying `full_len` and `truncated`, so a
caller never mistakes a stored prefix for the whole value. When the same id was
upgraded from a string to a blob across parts, the blob wins. The dictionary is
loaded into memory — the segment's string table by design, bounded by the
writer's dictionary cap.

## Not Implemented Yet

Time-range and drill-down queries, the cross-segment `str_id` cache, on-demand
(rather than eager) `dict.blobs` reads, and the S3 range-read path arrive in
later steps.
