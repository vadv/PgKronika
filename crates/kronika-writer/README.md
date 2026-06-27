# kronika-writer

[Русская версия](README.ru.md)

`kronika-writer` keeps the state needed to build a `.pgm` segment. Data sources
push rows and strings into this crate; `kronika-format` still defines the byte
layout. The writer decides when in-memory state becomes a journal part and when
the journal can be sealed into a segment.

## Current Contents

The crate currently exposes:

- `SectionBuffers`, per-type row buffers that encode a collection window into one
  PGM part;
- `Interner`, the per-segment string interner over
  `kronika_format::SegmentDicts`;
- `Journal`, the file-backed `active.parts` journal.

## Section Buffers

`SectionBuffers` holds typed rows until they are written into a part.
`push::<T>(row)` works for any `T: Section`; internally there is one erased
buffer per `type_id`. Adding a registry type does not add a new field or branch
here.

A buffer for one type stops at `MAX_SECTION_ROWS`. When it is full, `push`
returns the row as `Err(row)`; the caller flushes and tries that row again.

`flush(dict_sections, source_id)` encodes buffered rows, appends dictionary
sections after data sections, and returns one PGM part. Data sections are ordered
by `type_id`; the catalog time range comes from the rows that carry timestamps.
`None` means there was nothing to write. After a successful flush the data
buffers are empty.

## Dictionary Sections

`dict::encode(window)` turns the current interner window into `dict.strings` and
`dict.blobs` section bodies, sorted by `str_id`. These dictionary bodies are not
registry `Section` types because they store variable-length binary values, but a
part carries them beside the data sections. Snapshot `str_id` columns then
resolve within the same segment.

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
returning. Opening an existing journal scans it for recovery.

Recovery reads the file frame by frame. Peak memory is one part plus its decoded
catalog, a small resync buffer, and 16 bytes per recovered frame. The whole
journal is never loaded. A test keeps the streaming scanner equivalent to the
in-memory scanner in `kronika-format`.

If the last frame was only partly written, the file is truncated to the last
valid frame and writing continues. Damage in the middle of the file, or damage
at the end that is not a partial write, is reported in `OpenReport` and left on
disk for diagnostics.

`append()` writes one frame. `JournalConfig::max_journal_len` caps the journal
size (default 1 GiB). If the next frame would exceed it, `append` returns
`JournalError::Full`; the caller should seal the current journal first.
`reset()` clears the journal after the segment has been written successfully.

## Segment Completion

`seal(journal, dest)` writes the journal parts into one immutable segment. It
streams one part at a time, copies section bodies into a sibling `*.tmp`, writes
the end catalog, fsyncs, and publishes with a hard link. An existing destination
is not overwritten. The caller calls `Journal::reset` only after `seal` returns
`Ok`.

A section type that appears in several parts is kept as repeated catalog entries
— valid multi-part sections the reader processes in order. Collapsing them into
one sorted, recompressed section is an optimization of this same path; it
changes how bodies are written, not the segment format.

## Not Implemented Yet

Remaining work in this crate:

- sort-merge repeated sections of one type into one sorted section;
- merge duplicate strict-hot dictionary values across parts;
- add a `str_id` Bloom filter for `dict.strings`.

These changes should not change the segment format. Dictionary placement and
journal frame validation stay in `kronika-format`.
