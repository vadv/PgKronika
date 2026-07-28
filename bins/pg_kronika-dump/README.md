# pg_kronika-dump

[Русская версия](README.ru.md)

`pg_kronika-dump` reads PgKronika data without PostgreSQL or the web process.
It is intended for operators and developers who need to:

- inventory the segments and quarantined evidence in a data root;
- find which sections occupy a PGM and how well they compress;
- inspect an OVF header, block directory, and decoded web index;
- print stored rows with dictionary references resolved;
- identify intact `active.parts` frames and the first damaged region.

The command is read-only. It does not repair journals, publish segments,
delete evidence, or acquire writer/overview ownership. A successful invocation
writes exactly one JSON object to stdout; diagnostics go to stderr.

## Build

From the repository root:

```sh
make dump
```

The binary is `target/<TARGET>/debug/pg_kronika-dump`. `make build` builds
`collector`, `web`, and `dump`; `make collector`, `make web`, and `make dump`
build them separately. Without `TARGET`, the Makefile selects the current
computer's host target. To cross-compile, install the Rust target first and
pass it explicitly, for example
`make dump TARGET=x86_64-unknown-linux-gnu`.

## Syntax

```text
pg_kronika-dump <path> [--rows] [--limit N]
```

| Argument | Meaning |
| --- | --- |
| `<path>` | A PgKronika data root, a `YYYY[/MM[/DD]]` calendar directory, one `.pgm` or `.ovf`, or an `active.parts`/quarantine journal. |
| `--rows` | Add decoded PGM rows, journal-frame rows, or OVF web-index contents. It is rejected for a data root. |
| `--limit N` | Emit at most `N` PGM rows per section or OVF series per metric. The default is `1000`; the option requires `--rows`. |

Regular-file mode is selected by the exact, case-sensitive final suffix:

| Input | Mode |
| --- | --- |
| Data root or its `YYYY`, `YYYY/MM`, `YYYY/MM/DD` directory | Strict bounded tree traversal with an optional calendar filter. |
| `*.pgm` | One sealed PGM or self-contained PGM part. |
| `*.ovf` | One standalone overview/web-index file. |
| Any other regular-file name | Version-1 journal forensics. |

The selected decoder validates its own magic and framing; the command does not
fall back to a different mode based on file contents. A symbolic-link `<path>`
is rejected.

## Data root

```sh
pg_kronika-dump /var/lib/pg_kronika | jq .
pg_kronika-dump /var/lib/pg_kronika/2026/07/27 | jq .
```

This mode uses the same closed `kronika-layout` grammar as collector and web.
It does not follow symbolic links and applies the normal traversal, metadata,
and segment limits.

The result contains:

| Field | Meaning |
| --- | --- |
| `root` | The actual PgKronika data root. For a calendar-directory input, the command derives it from the parent components. |
| `scope` | The `YYYY`, `YYYY/MM`, or `YYYY/MM/DD` filter. It is omitted for the data root itself. |
| `journal` | Root `active.parts` state, verified-frame count, physical bytes, and first damage kind. `state: "absent"` means the file does not exist. |
| `quarantine` | Canonical `qv1` evidence: opaque `id`, quarantine reason, bytes, and filesystem object type. Unknown names inside quarantine are not interpreted. |
| `days` | Valid UTC day directories in the selected scope, including empty days, with sealed PGMs ordered by `SegmentId`. |
| `totals` | Segment count and byte totals for the selected scope. |

With a `scope`, only `days` and `totals` are filtered. The root `journal` and
`quarantine` still describe the whole data root because those objects do not
belong to an individual calendar directory.

Each segment reports its `segment_id`, full `pgm_bytes`, whether a sibling OVF
exists, data-section count, stored section bytes, collection-window count, and
catalog time range. `decoded_bytes` and `ratio` are `null` in this mode.

`first_window_us` and `last_window_us` are the PGM catalog's `min_ts` and
`max_ts`. They describe content, not the directory bucket. A segment crossing
midnight remains under the UTC day derived from its starting `segment_id`.

Tree mode reads only PGM tail catalogs. It provides a fast inventory and exact
stored sizes, but does not verify section-body CRCs or calculate decompressed
sizes. Inspect an individual PGM to obtain exact `decoded_bytes` and `ratio`.

## One PGM

```sh
pg_kronika-dump \
  /var/lib/pg_kronika/2026/07/27/1785100000000000.pgm |
  jq '{windows, dictionary, sections, totals}'
```

`windows.count` is `1` for a normal one-window part and the exact number of
journal parts coalesced by `seal` for a finished segment. A zero in a
low-level hand-built container means unknown and is emitted as `null`.

Dictionary sections are summarized in `dictionary`; `sections` contains data
sections only:

| Section field | Meaning |
| --- | --- |
| `type_id` | Registry id such as `S_105_001`; an unknown class is a decimal string. |
| `type_name` | Logical name from the current `kronika-registry`, or `null` for an unknown type. |
| `rows` | Catalog row count, checked against Parquet. |
| `stored_bytes` | Physical Parquet-body bytes. PGM magic, catalog, and tail are excluded. |
| `decoded_bytes` | Exact validated sum of page headers and uncompressed page payloads. It is not an Arrow-memory estimate. |
| `ratio` | `decoded_bytes / stored_bytes`. A value of `4.0` means the validated uncompressed work is four times the stored body. |
| `share_of_file` | `stored_bytes / file_bytes`. |

`totals` includes both data and dictionary bodies. If a section declares more
than the 128 MiB decode-work limit, it is not admitted to Arrow:
`decoded_bytes` and the aggregate ratio become `null`, and `decode_skipped` is
`"limit"`.

An unknown `type_id` still gets byte and Parquet-profile statistics. With
`--rows`, it reports `rows_skipped: "unknown_type"` because no registry
contract exists to name and type its columns safely.

## Rows

```sh
pg_kronika-dump \
  /var/lib/pg_kronika/2026/07/27/1785100000000000.pgm \
  --rows --limit 20 |
  jq '.sections[] | {type_name, rows, rows_data, truncated}'
```

`rows_data` is an array of objects using registry column names. `StrId(0)` and
PostgreSQL `NULL` become JSON `null`. Every other `str_id` must resolve through
the same PGM dictionary; a missing reference is corrupt input and exits 1.

A normal dictionary value is a JSON string. A `dict.blobs` value is:

```json
{
  "text": "stored prefix",
  "full_len": 42000,
  "truncated": true
}
```

Invalid UTF-8 is decoded with `U+FFFD` replacement. Floating-point values that
JSON cannot represent as numbers are emitted as `"NaN"`, `"Infinity"`, and
`"-Infinity"` rather than being confused with `null`.

Section-level `truncated: true` only means `--limit` hid decoded rows. It is
independent of the `truncated` flag inside a `dict.blobs` value.

## One OVF

```sh
pg_kronika-dump \
  /var/lib/pg_kronika/2026/07/28/1785200000000000.ovf |
  jq '{header, blocks}'

pg_kronika-dump \
  /var/lib/pg_kronika/2026/07/28/1785200000000000.ovf \
  --rows --limit 10 |
  jq '.blocks[] | select(.content != null)'
```

Without `--rows`, the command reads only the fixed OVF header and bounded block
directory. `header` reports the source and contract identity. `blocks` preserves
directory order and reports kind/code, logical id, schema, flags, codec, stored
and decoded bytes, item count, and optional time range. Unknown optional kinds
remain visible with `kind: null`.

Metadata-only inspection validates header and directory CRCs, framing, versions,
flags, lengths, ordering, and embedded fact/lineage identities. It does not read
block bodies and therefore does not verify their CRCs.

With `--rows`, known web-index blocks gain `content`:

- `ui_summary` contains the shared grid, snapshot timestamps, per-view status,
  nullable populations and notable state, and bucket coverage;
- each `entity_series` contains its view identity, observed range, grid,
  coverage, complete typed-identity dictionary, metric metadata, cutoff score,
  and retained series;
- each series contains the exact score, dictionary key/label, and one value per
  bucket. A missing observation is JSON `null`; an observed zero is `0.0`.

`--limit N` applies independently to the series array of every metric.
`truncated: true` means additional retained series exist. The dictionary is
always complete so every returned `entity_ref`, key, and label remains
self-contained.

Body reads use the canonical `kronika-reader` decoder and verify CRC,
decompression bounds, logical ordering, and agreement with the directory
descriptor. Other OVF block kinds remain metadata-only.

Standalone OVF inspection proves internal integrity. It does not open the
sibling PGM and therefore does not prove that a copied OVF belongs to a
particular neighboring PGM.

## Journal damage

```sh
pg_kronika-dump /var/lib/pg_kronika/active.parts | jq .
pg_kronika-dump /path/to/qv1-evidence --rows --limit 20 | jq .
```

The command validates the 36-byte root header and runs the bounded
`kronika-format` resynchronization scanner over the physical body. Every
`frames` item has a valid frame header, PGM, catalog, and section CRCs.

| Field | Meaning |
| --- | --- |
| `header.recorded_body_len` | Body bytes recorded in the root header. |
| `physical_bytes` | Current complete file size, including the header. |
| `valid_prefix_bytes` | Continuous valid prefix from byte zero. Frames found after damage do not extend it. |
| `damage` | First detected damage, or `null`: offset, stable kind, and detail. |
| `recoverable` | Every complete frame found by bounded scanning, including frames after resynchronization. |

A damaged journal is a successful forensic result, so the process exits 0 and
describes damage in JSON. It has not repaired or published the discovered
frames. A header without a supported `SegmentId` does not provide a trusted
segment identity.

With `--rows`, each verified frame also contains `dictionary` and `sections`
under the same rules as a standalone PGM. `--limit` applies independently to
each section in each frame.

## Exit codes and limits

| Code | Meaning |
| ---: | --- |
| 0 | JSON was produced, including a journal with reported damage. |
| 1 | Input cannot be read or violates its structural, CRC, or dictionary-reference contract. |
| 2 | Invalid arguments or an unsupported option combination. |

The command inherits the normal layout, format, registry, and reader bounds,
including all OVF directory, block, dictionary, metric, series, and bucket
limits. A PGM catalog is at most 64 MiB, an encoded section at most 8 MiB, and
admitted decode work at most 128 MiB per section. `--rows` output can contain
SQL, plans, object names, process arguments, entity labels, and PostgreSQL log
text; protect it as you protect the source data root.

`pg_kronika-dump` does not filter by value or time, export CSV/Parquet, compare
segments, or modify files.
