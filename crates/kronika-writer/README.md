# kronika-writer

[Русская версия](README.ru.md)

`kronika-writer` turns one or more bounded collection windows into a durable
PGM segment. It owns in-memory section buffers, per-segment string interning,
the `active.parts` file, recovery, and sealing. Source queries and format byte
definitions remain in other crates.

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

`Interner` owns dictionary identity for one open segment. The current window
keeps full stored bytes under `DictLimits`. After the caller successfully
writes a window, `flush_window` replaces those bytes with compact metadata
needed for collision detection, deduplication, and final placement. Repeated
SQL or plans therefore do not remain fully duplicated in memory until seal.

Interning is transactional on collision, placement conflict, or byte-cap
failure: prior state remains valid. The caller seals or flushes when it receives
`DictError::Full`.

## Journal

`Journal::open(path, config)` scans `active.parts` without loading the whole
file. Peak scan memory is one capped part body, decoded catalog state, a bounded
resynchronization buffer, and one compact reference per valid part.

An incomplete final frame is truncated to the last valid boundary. Middle or
non-torn terminal damage remains on disk and appears in `OpenReport`. `append`
validates a PGM part, writes its `PGMP` frame, synchronizes the file, then
returns its reference. `JournalConfig::max_journal_len` is a hard file-size
cap; the next frame returns `JournalError::Full` before unbounded growth.

`reset` truncates the journal. The caller must invoke it only after a segment
was successfully published.

## Sealing

`seal(journal, destination)` streams each part, copies section bodies into a
sibling temporary file, writes the combined end catalog, synchronizes the file,
and publishes it with a hard link. An existing destination is never
overwritten. The temporary file is cleaned up on return.

The writer preserves repeated section entries in catalog order. It does not
compact or re-encode them into one section. It also does not reset the journal,
select a filename, or implement retention; those lifecycle decisions belong to
the collector.

Failures distinguish journal I/O/framing/full conditions from seal validation,
destination, and synchronization errors. See [`src/lib.rs`](src/lib.rs) for
the canonical API and [`../kronika-format/`](../kronika-format/) for on-disk
framing.
