# kronika-writer

[Русская версия](README.ru.md)

`kronika-writer` is the collector's write path: everything between data
sources and a sealed `.pgm` segment. The byte layout it produces is owned
by `kronika-format`; this crate owns the runtime state of writing.

## Implemented Scope

The crate currently exposes:

- `Interner` — the per-segment string interner over
  `kronika_format::SegmentDicts`.

The interner adds what only the write path needs on top of the dictionary
model:

- `take_new()` — ids first interned since the previous call. This is the
  dictionary content of the next mini-part: a mini-part dictionary holds
  the strings first seen in its flush window.
- `seal()` — hands the segment dictionaries to the seal path and starts
  the next segment empty. The collector seals early under interner growth
  pressure precisely so the next segment starts with an empty interner.
- `stats()` — dictionary sizes for the collector's self-metrics.

A failed interning call (collision, placement conflict) changes neither
the dictionaries nor the novelty list.

## Not Here Yet

Per-type buffers, the `active.parts` journal, merge, seal, and Parquet
encoding arrive in later steps. The dictionary contract itself — routing,
truncation, collision rules — lives in `kronika-format` and is documented
there.
