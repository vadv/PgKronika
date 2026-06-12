# kronika-format

[Русская версия](README.ru.md)

`kronika-format` contains the byte-level primitives for the PGM container:

- file magic `PGM1`;
- catalog entries;
- catalog metadata;
- tail index;
- CRC32C.

It does not know how section bodies are encoded. Parquet sections, dictionaries,
events, charts, storage backends, and I/O live in other crates. This README is
the local contract for the container subset implemented by this crate.

## Implemented Scope

The crate currently exposes:

- `MAGIC` and `FORMAT_VERSION`;
- `ENTRY_LEN`, `META_LEN`, and `TAIL_INDEX_LEN` size constants;
- `Entry`, one 32-byte catalog row;
- `Catalog`, the decoded end catalog and metadata;
- `TailIndex`, the final 8-byte pointer to the catalog;
- `DecodeError`, typed catalog and tail-index decode errors;
- `crc32c`, the checksum used by the container.

Later implementation steps add dictionaries, HOT block headers, and
`active.parts` frames.

## File Layout

```text
segment.pgm
┌─────────────────────────────────────────┐
│ MAGIC "PGM1"                 4 B        │
│ section bodies               ...        │  <- opaque to this crate
│ catalog entries              32 B each  │
│ catalog metadata             40 B       │
│ tail index                   8 B        │
└─────────────────────────────────────────┘
```

All integer fields are little-endian. `offset` and `len` in a catalog entry are
absolute offsets from the beginning of the segment file to the section body.

## Catalog Entry

Each catalog entry is exactly 32 bytes.

| Offset | Field | Type | Meaning |
| --- | --- | --- | --- |
| 0 | `type_id` | `u32` | section type from the registry |
| 4 | `flags` | `u32` | reserved, written as zero |
| 8 | `offset` | `u64` | absolute offset of the section body |
| 16 | `len` | `u64` | section body length in bytes |
| 24 | `rows` | `u32` | rows or records in the section |
| 28 | `crc32c` | `u32` | CRC32C of the section body |

`type_id` may repeat. Repeated entries are parts of one logical section in
catalog order. The exception is chart sections (class 10): there repeated
entries describe different entities, and splitting one chart into parts is
forbidden. `Catalog` preserves the on-disk order.

## Catalog Metadata

The metadata block is exactly 40 bytes and follows all entries.

| Offset | Field | Type | Meaning |
| --- | --- | --- | --- |
| 0 | `min_ts` | `i64` | minimum segment timestamp, unix microseconds |
| 8 | `max_ts` | `i64` | maximum segment timestamp, unix microseconds |
| 16 | `source_id` | `u64` | `str_id` of `{cluster_id}/{pg_system_identifier}`, or `0` |
| 24 | `entry_count` | `u32` | number of catalog entries |
| 28 | `format_version` | `u32` | container version |
| 32 | `crc32c` | `u32` | CRC32C over entries and metadata with this field zeroed |
| 36 | `reserved` | `u32` | reserved, written as zero |

`format_version` versions the container layout only. Data schemas evolve through
new `type_id` values in the registry.

## Tail Index

The tail index is the final 8 bytes of the segment.

| Offset | Field | Type | Meaning |
| --- | --- | --- | --- |
| 0 | `catalog_len` | `u32` | length of entries plus metadata |
| 4 | `magic` | 4 bytes | ASCII `PGM1` |

Opening a segment starts from these 8 bytes.

## Encode Path

`Catalog::encode` produces everything that follows the last section body in a
segment file: entries, metadata with the CRC patched in, and the 8-byte tail
index, as one buffer. It panics if the encoded catalog does not fit `u32`;
that is treated as a writer bug, not a recoverable error.

`Catalog::encoded_len` returns the length of entries plus metadata — the
`catalog_len` value stored in the tail index. `TailIndex::encode` produces the
final 8 bytes on its own.

## Decode Path

1. Read the last 8 bytes and call `TailIndex::decode`.
2. Use `catalog_len` to read the bytes immediately before the tail index.
3. Call `Catalog::decode`.
4. Read section bodies using `offset` and `len` from the decoded entries.

`Catalog::decode` checks:

- byte length: `entry_count * 32 + 40`;
- stored `entry_count` against the length-derived count;
- catalog CRC32C before parsing entries.

Decode errors include the observed values where that helps debugging: bad tail
magic, invalid catalog length, entry-count mismatch, and CRC mismatch.

## CRC32C

All container checksums use CRC32C, the Castagnoli polynomial
(`CRC_32_ISCSI`). The catalog checksum is computed over catalog entries and
metadata with the metadata `crc32c` field set to zero.

## Tests

- `tests/fixture.rs` decodes and re-encodes a byte-exact minimal segment fixture.
  The fixture catches drift between the implementation and the documented byte
  layout.
- `tests/property.rs` checks encode/decode roundtrips and single-byte corruption
  detection for generated catalogs.

## Crate Boundaries

This crate contains only the byte layouts shared by writers and readers. Today
that means the end catalog, tail index, and CRC32C. Later steps add dictionary
sections, HOT block headers, and `active.parts` journal frames here.

Code that needs collector state, domain knowledge, or external I/O stays above
this layer:

- `kronika-writer` manages the string interner, buffers, merge, and seal logic;
- `kronika-reader` opens segments, reads sections, and manages caches;
- `kronika-registry` defines `type_id` meaning and section schemas;
- `kronika-derive` and `kronika-writer` handle Parquet schemas and encoding;
- `kronika-store*` owns local, HTTP, and S3 storage access;
- `kronika-source-*` collects data from PostgreSQL, OS, cgroup, and logs.
