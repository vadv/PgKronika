# kronika-writer

[Русская версия](README.ru.md)

`kronika-writer` turns one or more bounded collection windows into a durable
PGM segment. It owns in-memory section buffers, per-segment string interning,
the version-1 `active.parts` journal, recovery, and sealing. Source queries,
format bytes, and the data-directory grammar remain in other crates.

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

`Journal::open(&WriterOwner, config)` opens the root-level `active.parts`
through a capability from `kronika-layout`. A new journal is initialized as a
durable 36-byte version-1 empty header; it is never represented by a zero-length
file.

Journal version 1 uses the magic `PGKJNL1\0`. Its checksummed header records
whether the journal is empty or active, the active [`SegmentId`][layout], and
the exact number of following frame bytes. `append(segment_id, part)` validates
the PGM part and writes its `PGMP` frame. The first append makes the segment id
and first frame durable at the same synchronization boundary. Later appends
must use the same id.

Open validates the complete header and frame body without loading the whole
file. A headerless, differently versioned, torn, or damaged journal is rejected
and left unchanged; a zero-length file provably holds no data and is
re-initialized as the empty header. Version 1 is the first and only
supported journal format. PgKronika has not had a public release, and there is
no alternate journal format or migration path.

`JournalConfig::max_journal_len` caps the physical file, including the
temporary 32-byte reset marker. Every append, including the first one, reserves
space for that marker. A frame that would exceed the cap returns
`JournalError::Full`, allowing the collector to seal first. Version 1 admits at
most 1 GiB per journal, 1,000,000 frames per journal, and 64 MiB per PGM part.
Configuration may only lower those absolute limits.

`reset` is valid only after successful segment publication. It first appends
and synchronizes a marker containing the pre-reset length, `SegmentId`, and
header checksum. It then writes `JournalHeader::EMPTY` and calls `sync_data`
while the marker and frame body are still present. Only after that
synchronization does it truncate the file to 36 bytes and call `sync_data` a
second time. If the process exits after committing the marker, the next
`Journal::open` validates that marker and completes the reset. A failed rollback
or a failure after marker commit poisons the open journal, so collection cannot
continue through an indeterminate persistence state.

## Sealing

`seal(journal, owner, SegmentAddress)` streams each part, copies section bodies
into a temporary file in the segment's UTC day, and writes the combined end
catalog. PGM publication synchronizes the file, adds the canonical
`YYYY/MM/DD/N.pgm` name with a hard link, synchronizes the day, removes the
temporary name, and synchronizes the day again. An existing destination is
never overwritten; recovery succeeds only when it can prove that the existing
PGM is structurally valid and byte-identical.

After acquiring the writer owner lock, collector startup removes only
recognized stale PGM publication temporaries. It leaves OVF and overview-probe
temporaries to the overview owner.

The writer preserves repeated section entries in catalog order. It does not
compact or re-encode them into one section. It also does not reset the journal,
choose the `SegmentId`, or implement retention; those lifecycle decisions
belong to the collector. `SegmentAddress` derives the only valid path from the
id, and the writer accepts only that strict calendar-tree address.

Failures distinguish journal I/O/framing/full conditions from seal validation,
destination, and synchronization errors. See [`src/lib.rs`](src/lib.rs) for the
canonical API, [`../kronika-format/`](../kronika-format/) for on-disk framing,
and [`../kronika-layout/`](../kronika-layout/) for paths and ownership.

[layout]: ../kronika-layout/src/time.rs
