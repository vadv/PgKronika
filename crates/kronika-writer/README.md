# kronika-writer

[Русская версия](README.ru.md)

`kronika-writer` turns bounded collection windows into one durable compact PGM
segment. It owns in-memory section buffers, string interning, the
`active.parts` journal, crash recovery, compaction, and immutable publication.
Source queries and format byte definitions remain in other crates.

## Collection window

`SectionBuffers::push<T: Section>` stores rows by registered type. A type buffer
stops at `MAX_SECTION_ROWS` and returns the rejected row so the caller can flush
and retry without loss. `flush` encodes data sections in type-id order, appends
dictionary sections, derives the catalog time range, and returns one
self-contained PGM part. Successful flush empties the row buffers.

`dict::encode` converts the current interner window into sorted
`dict.strings` and `dict.blobs` sections. Snapshot rows refer to those values by
`str_id`.

## Interner

`Interner` owns dictionary identity under `DictLimits`. The current window
keeps full stored bytes. A caller that retains one interner across windows can
use `flush_window` after a durable append to replace those bytes with compact
metadata needed for collision detection, deduplication, and final placement.

Interning is transactional on collision, placement conflict, or byte-cap
failure: prior state remains valid. The caller seals or flushes when it receives
`DictError::Full`.

## Journal

`Journal::open(path, config)` scans `active.parts` without loading the whole
file. Peak scan memory is one capped part body, decoded catalog state, a bounded
resynchronization buffer, and one compact reference per valid part.

A recoverable final frame is truncated to the last valid boundary. This
classification covers an incomplete frame and a frame whose valid header ends
at EOF but whose inner PGM fails validation. Other middle or terminal damage
remains on disk and appears in `OpenReport`. `append` validates a PGM part,
writes its journal frame, synchronizes the file, then returns its reference.
`JournalConfig::max_journal_len` and `max_parts` reject the next frame before
either hard bound is crossed.

`reset` truncates the journal. The caller must invoke it only after a segment
was successfully published.

## Sealing

`seal(journal, destination)` first validates and admits every part. For each
registered `type_id`, it decodes rows, applies the registry's complete
canonical order, and writes bounded external runs. Runs are merged with a
fixed fan-in of 32. Dictionary rows are normalized by `str_id`; exact
duplicates coalesce, while collisions, placement conflicts, and inconsistent
truncation metadata fail the seal.

The result contains one Parquet body for every present data type and at most
one body for each dictionary type. All final bodies use PLAIN values,
Zstandard level 6, one bounded row group, no Parquet dictionary pages,
statistics, or page indexes. Memory, spill bytes, output bytes, input section
count, row count, page size, section size, and reader work are admitted before
their bounds are crossed.

The sibling temporary PGM is synchronized before publication. Publication uses
a hard link, never replaces an existing destination, and synchronizes the
parent directory. Exact existing bytes are idempotent; different existing
bytes return `PublicationConflict`. A failure leaves `active.parts` intact.
The caller resets the journal only after success and owns filename selection
and retention.

See [`src/lib.rs`](src/lib.rs) for the API and
[`../kronika-format/`](../kronika-format/) for the physical contract.
