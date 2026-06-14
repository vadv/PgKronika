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

## Not Implemented Yet

String resolution (reading `dict.strings` / `dict.blobs` to map `str_id` to its
bytes), time-range and drill-down queries, the cross-segment `str_id` cache, and
the S3 range-read path arrive in later steps.
