# kronika-writer

[Русская версия](README.ru.md)

`kronika-writer` keeps collector state while a segment is being written. It sits
between data sources and a finished `.pgm` segment. `kronika-format` defines
the bytes on disk; this crate decides when writer state is flushed to disk and
what must remain in memory while the segment is being built.

## Current Contents

The crate currently exposes:

- `Interner`, the per-segment string interner over
  `kronika_format::SegmentDicts`;
- `Journal`, the file-backed `active.parts` journal.

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

## Not Implemented Yet

Per-type buffers, part merging, segment completion, and Parquet encoding arrive
in later steps. Dictionary placement and journal frame validation are defined in
`kronika-format`.
