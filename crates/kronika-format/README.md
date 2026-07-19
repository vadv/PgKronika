# kronika-format

[Русская версия](README.ru.md)

`kronika-format` owns the byte structures shared by every PGM writer and
reader. It contains no PostgreSQL meaning, Parquet codec, storage policy, or
filesystem orchestration.

## Owned contracts

- `MAGIC = "PGM1"` and container `FORMAT_VERSION = 1`;
- fixed-size catalog entries, catalog metadata, and the final tail index;
- CRC32C for catalogs, section bodies, and journal headers;
- non-zero `StrId` values and bounded per-segment dictionaries;
- self-contained PGM parts and `PGMP` frames in `active.parts`;
- buffer and streaming journal scanners with typed damage regions.

All integers are little-endian. A sealed segment is:

```text
"PGM1" | section bodies | catalog entries | catalog metadata | tail index
```

Each 32-byte catalog entry carries `type_id`, flags, absolute body offset,
body length, row count, and body CRC. Forty bytes of catalog metadata carry
the segment time range, source id, entry count, format version, catalog CRC,
and a reserved field. The final eight bytes carry catalog length and `PGM1`,
so a reader opens the file from the end.

The same `type_id` may occur in several catalog entries; readers preserve
catalog order. Schema meaning and logical layout versions come from
`kronika-registry`.

## Dictionaries

`StrId` is `xxh3_64(original_bytes)` with zero reserved. `SegmentDicts`
deduplicates values, detects collisions, and assigns them to strings, blobs,
or the optional hot-string subset according to accumulated placement
requirements. Placement does not depend on call order.

`DictLimits` controls the blob threshold, truncation length, and total stored
bytes. A truncated blob keeps its original length and SHA-256 identity.
Collisions and incompatible placement requirements are typed errors. A full
dictionary reports `DictError::Full`; the writer can flush the current window
instead of growing without limit.

## Journal frames and recovery

An `active.parts` frame contains a 16-byte `PGMP` header and one self-contained
PGM part. `JournalLimits` caps accepted part length and resynchronization buffer
size. Validation checks framing, catalog bounds and CRC, section bounds, and,
when requested, every section CRC.

Streaming scan keeps one part body plus bounded scan state in memory. A torn
final frame is distinguishable from middle or terminal media damage. Valid
parts around damage remain in the report; callers decide whether to truncate,
retain, or stop.

## Failure and trust model

Decode errors report bad magic, length, count, reserved fields, and checksums
without interpreting an invalid buffer. Checked arithmetic rejects offset and
length overflow. CRC32C detects accidental corruption, not deliberate
tampering; PGM provides no signature or encryption.

Start with [`src/lib.rs`](src/lib.rs) for the canonical API and
[`../../docs/segment-format.md`](../../docs/segment-format.md) only for
historical design context.
