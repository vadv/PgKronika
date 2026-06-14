# kronika-writer

[Русская версия](README.ru.md)

`kronika-writer` keeps collector state while a segment is being written. It sits
between data sources and a finished `.pgm` segment. `kronika-format` defines
the bytes on disk; this crate decides when writer state is flushed to disk and
what must remain in memory while the segment is being built.

## Current Contents

The crate currently exposes:

- `SectionBuffers`, per-type row buffers that encode a collection window into one
  PGM part;
- `Interner`, the per-segment string interner over
  `kronika_format::SegmentDicts`;
- `Journal`, the file-backed `active.parts` journal.

## Section Buffers

`SectionBuffers` is the collection window. A collection step pushes typed rows
with `push::<T>(row)` for any `T: Section`; the buffers hold one type-erased
buffer per `type_id`, not a field per type, so a new section type costs one
`push` and no change here — the property the registry's hundreds of types need.

`flush(dict_sections, source_id)` encodes every buffered type to a Parquet body
(`Section::encode`), reads each type's time range (`Section::ts_range`), and
calls `kronika_format::build_part` to assemble one PGM part: the data sections in
`type_id` order, then the `dict_sections` (the COLD-then-WARM segment layout),
the catalog time range spanning the buffered rows. It returns the part bytes —
or `None` when nothing is buffered — for the caller to append to the journal, and
clears the data buffers.

## Dictionary Sections

`dict::encode(window)` turns an interner flush window into `dict.strings` and
`dict.blobs` Parquet section bodies — one per placement that has entries, sorted
by `str_id`. These are not registry `Section` types (their columns are
variable-length binary), so they are encoded directly, but as ordinary section
bodies a part carries beside the data sections, so snapshot `str_id` columns
resolve to bytes within the same segment.

## Interner

`Interner` has two layers.

The window is the current dictionary batch in memory. It keeps full bytes for
values that have not yet been written to `active.parts`.

Flushed entries track values already written to the journal. They keep length,
a SHA-256 prefix, and placement requirements, but not the original text. That
lets repeated SQL texts and plans be deduplicated without keeping every byte in
memory until the segment is finished.

`flush_window()` calls the provided write function with the current window. If
the write succeeds, the window is converted into flushed entries and cleared.
If the write fails, the window is left unchanged.

`seal()` returns the remaining window and final placement directives for values
already written to the journal. The next segment starts with an empty interner.
`stats()` reports dictionary sizes for collector metrics.

A failed interning call (collision, placement conflict) changes neither
the window nor the flushed entries.

## Journal

`Journal` appends PGM parts as `PGMP` frames and syncs each frame before
returning. Opening an existing journal runs the recovery scan.

Recovery streams the file frame by frame: peak memory is one part plus its
decoded catalog (at most twice `max_part_len`, reached only by a
pathological all-catalog part), a small sliding window during
resynchronization, and 16 bytes of directory per recovered frame — never
the whole journal. The streaming scanner is pinned to the in-memory
scanner of `kronika-format` by an equivalence test.

If the last frame was only partly written, the file is truncated to the last
valid frame and writing continues. Damage in the middle of the file, or damage
at the end that is not a partial write, is reported in `OpenReport` and left on
disk for diagnostics.

`append()` writes one frame. The journal size is capped by
`JournalConfig::max_journal_len` (default 1 GiB, a starting value): when an
append would exceed the cap it fails with `JournalError::Full`, which is the
signal to merge the journal into a segment early. `reset()` clears the
journal after a segment has been completed successfully.

## Segment Completion

`seal(journal, dest)` merges the journal's parts into one immutable segment.
It streams the journal one part at a time — peak memory is one part plus the
growing catalog, never the whole segment — copies each part's section bodies
into a sibling `*.tmp`, writes the end catalog, fsyncs, and publishes with a
hard link so an existing segment is never overwritten. The caller calls
`Journal::reset` only after `seal` returns `Ok`.

A section type that appears in several parts is kept as repeated catalog entries
— valid multi-part sections the reader processes in order. Collapsing them into
one sorted, recompressed section is an optimization of this same path; it
changes how bodies are written, not the segment format.

## Not Implemented Yet

Three optimizations of the seal path remain, none changing the segment format:
the sort-merge that recompresses repeated sections of one type into one sorted
section; the dictionary merge that deduplicates a strict-hot value repeated
across parts (parts are otherwise disjoint, so seal copies their dictionaries as
is); and the `str_id` bloom filter on `dict.strings` for cross-segment lookup.
Dictionary placement and journal frame validation are defined in
`kronika-format`.
