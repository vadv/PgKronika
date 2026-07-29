# kronika-layout

[Русская версия](README.ru.md)

`kronika-layout` defines the only supported local data-directory grammar for
PgKronika. It maps a stable segment identity to its UTC calendar directory,
performs strict bounded discovery, opens files relative to verified directory
descriptors, and grants the two mutation roles through process-wide locks.
PGM framing and journal bytes remain in `kronika-format`; section encoding and
finalization remain in `kronika-writer`.

## On-disk layout

```text
DATA_ROOT/
├── active.parts
├── .pgkronika-writer.owner.lock
├── .pgkronika-overview.owner.lock
└── YYYY/
    └── MM/
        └── DD/
            ├── N.pgm
            └── N.ovf
```

`N` is the decimal [`SegmentId`](src/time.rs): the Unix timestamp in
microseconds of the first collection window successfully appended to that
segment. `YYYY/MM/DD` is the UTC day derived from that value. The path does not
use the segment catalog's `min_ts` or `max_ts`, finalization time, file
modification time, or the timestamp of a late event.

A segment that remains open across UTC midnight stays under the day on which
its `SegmentId` falls. Time-range queries still use the catalog's `min_ts` and
`max_ts`; the directory is a physical bucket, not a query index.

`N.pgm` is the immutable source segment. When derived overview facts exist,
`N.ovf` is its replaceable sibling with the same stem. An OVF can be rebuilt
from its PGM.

`active.parts` stays at the root. The first supported journal format is
version 1, with magic `PGKJNL1\0`; its header stores the active `SegmentId`.
`kronika-layout` controls access to the file, while `kronika-format` defines
its bytes and `kronika-writer` implements its lifecycle. PgKronika has not had
a public release, so version 1 is also the only journal format; there is no
alternate format or migration mechanism.

## Closed grammar

The data root is PgKronika-owned. At its top level, a strict scan accepts only
the journal, the two owner-lock files, and four-digit year directories. The
calendar tree must use two-digit valid months and days. Day directories accept
only canonical final files and recognized PgKronika publication temporaries.
Symbolic links, unknown entries, misplaced segment ids, and root-level `.pgm`
or `.ovf` files fail the complete scan; no partial result is returned.

This is the first supported layout. The project has not shipped an earlier
public layout, so there is no compatibility reader, flat-layout fallback, or
migration mode. Recreate any pre-release development or test data that does
not follow this tree.

## Types and access

- `SegmentId` validates a Unix-microsecond identity representable by UTC years
  `0000..=9999`.
- `UtcDay` validates and formats one `YYYY/MM/DD` bucket.
- `SegmentAddress` binds one id to the only valid UTC day and returns the
  canonical `N.pgm` and `N.ovf` names.
- `DataRoot` holds an open root descriptor, performs strict discovery, and
  opens final files without following symbolic links.
- `FileIdentity` pins a PGM by device, inode, length, `mtime`, and `ctime`;
  store and reader code recheck it on the opened file descriptor.
- `WriterOwner` is the sole capability for `active.parts` and PGM publication.
  One collector can hold it for a data root.
- `OverviewOwner` is the sole capability for OVF publication and cleanup. One
  web process can hold it for a data root.

The lock files remain in `DATA_ROOT`; they are part of the layout even when no
process currently holds a lock.

## Traversal limits

`LayoutLimits` bounds every strict scan before it returns a snapshot:

| Field | Default | Hard maximum | Meaning |
| --- | ---: | ---: | --- |
| `max_visited_entries` | 1,000,000 | 4,000,000 | All filesystem entries visited in one scan. |
| `max_entries_per_day` | 10,000 | 1,000,000 | Entries inspected in one UTC day directory. |
| `max_segments` | 500,000 | 2,000,000 | Finished PGM segments returned. |
| `max_metadata_bytes` | 134,217,728 | 134,217,728 | Shared cap for names, journal metadata, returned collections, and compact catalog summaries. |

Every value must be non-zero and no greater than its hard maximum. Exceeding a
runtime bound fails the scan instead of returning an incomplete inventory.
The store's sizing regression covers five 365-day years with one segment every
15 minutes: both cold discovery and an unchanged cached refresh fit the default
128 MiB cap. A wholesale same-name replacement is rejected before both complete
sets of summaries can be retained.

## Publication and backup

PGM publication creates and synchronizes a temporary file inside the target
day, adds the final `N.pgm` name without overwriting an existing segment,
synchronizes the day, removes the temporary name, and synchronizes the day
again. OVF publication synchronizes a same-day temporary and atomically
replaces `N.ovf` only after rechecking the input PGM descriptor.

The initial local-filesystem contract targets Linux with ext4 or XFS.
Descriptor-bound PGM publication resolves the already-open temporary through
`/proc/self/fd`, so production containers and sandboxes must mount procfs.
Equivalent durability and lock behavior is not claimed for non-Linux or
network filesystems.

A backup must preserve the complete directory hierarchy. Use a stopped
collector and web process or a filesystem snapshot with equivalent
consistency. OVF files may be rebuilt, but `active.parts` and PGM files are
source data. PostgreSQL log-tail state is deliberately outside `DATA_ROOT`; if
it is needed for recovery, capture it separately at the same consistency
point.

The canonical API and error variants are in [`src/lib.rs`](src/lib.rs).
