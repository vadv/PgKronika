# kronika-writer

[Русская версия](README.ru.md)

`kronika-writer` is the collector's write path: everything between data
sources and a sealed `.pgm` segment. `kronika-format` defines the byte
layout; this crate keeps the write-time state needed to produce it.

## Implemented Scope

The crate currently exposes:

- `Interner` — the per-segment string interner over
  `kronika_format::SegmentDicts`;
- `Journal` — the file-backed `active.parts` journal.

The interner keeps memory bounded while preserving dictionary placement rules:

- the current window stores full bytes for values that have not yet been
  flushed to the journal;
- the flushed map stores only value identity and placement requirements for
  values already written to `active.parts`;
- `flush_window()` writes the current window through a caller-provided journal
  callback, then keeps only hashes and placement requirements;
- `seal()` returns the residual window plus placement directives for flushed
  values, then starts the next segment empty;
- `stats()` — dictionary sizes for the collector's self-metrics.

A failed interning call (collision, placement conflict) changes neither
the window nor the flushed map.

The journal appends mini-PGM parts as `PGMP` frames. Opening an existing journal
runs the recovery scan from `kronika-format`:

- a torn tail is truncated and writing continues;
- middle damage and a quarantined tail are reported in `OpenReport`;
- `append()` writes one frame and syncs the file before returning;
- `reset()` empties the journal after a successful seal.

## Not Here Yet

Per-type buffers, merge, seal, and Parquet encoding arrive in later steps. The
dictionary and frame-byte rules are defined in `kronika-format`.
