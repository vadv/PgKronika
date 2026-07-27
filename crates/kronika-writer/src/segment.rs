//! Segment completion: merge the journal's parts into one immutable segment.
//!
//! Streams journal parts into a temporary file and writes the end catalog last.

use std::error::Error;
use std::fmt;
use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::FileExt as _;

use kronika_format::{
    Catalog, ENTRY_LEN, Entry, FORMAT_VERSION, MAGIC, META_LEN, PartError, TAIL_INDEX_LEN,
    TailIndex, validate_part,
};
use kronika_layout::{FileIdentity, LayoutError, PgmTemp, SegmentAddress, SegmentId, WriterOwner};

use crate::{Journal, JournalError};

const MAX_CATALOG_BYTES: usize = 64 * 1024 * 1024;
const COMPARE_BUFFER_BYTES: usize = 64 * 1024;
const MAX_CATALOG_ENTRIES: usize = (MAX_CATALOG_BYTES - META_LEN) / ENTRY_LEN;

#[cfg(test)]
std::thread_local! {
    static AFTER_FIRST_COMPARISON_CHUNK:
        std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        std::cell::RefCell::new(None);
}

#[cfg(test)]
struct ComparisonHookGuard;

#[cfg(test)]
impl ComparisonHookGuard {
    fn assert_consumed(self) {
        AFTER_FIRST_COMPARISON_CHUNK.with(|hook| {
            assert!(hook.borrow().is_none(), "comparison hook was not exercised");
        });
        drop(self);
    }
}

#[cfg(test)]
impl Drop for ComparisonHookGuard {
    fn drop(&mut self) {
        AFTER_FIRST_COMPARISON_CHUNK.with(|hook| {
            hook.borrow_mut().take();
        });
    }
}

#[cfg(test)]
fn arm_after_first_comparison_chunk(hook: impl FnOnce() + 'static) -> ComparisonHookGuard {
    AFTER_FIRST_COMPARISON_CHUNK.with(|armed| {
        assert!(armed.borrow_mut().replace(Box::new(hook)).is_none());
    });
    ComparisonHookGuard
}

#[cfg(test)]
fn run_after_first_comparison_chunk() {
    let hook = AFTER_FIRST_COMPARISON_CHUNK.with(|armed| armed.borrow_mut().take());
    if let Some(hook) = hook {
        hook();
    }
}

macro_rules! seal_test_hook {
    (AfterFirstComparisonChunk) => {
        #[cfg(test)]
        run_after_first_comparison_chunk();
    };
}

/// What a completed segment contains, for the caller's metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SealSummary {
    /// Number of catalog entries (sections) written.
    pub sections: usize,
    /// Total segment length, bytes.
    pub bytes: u64,
    /// Minimal timestamp across the segment, unix microseconds.
    pub min_ts: i64,
    /// Maximal timestamp across the segment, unix microseconds.
    pub max_ts: i64,
}

/// Why sealing a segment failed.
#[derive(Debug)]
pub enum SealError {
    /// A filesystem operation failed.
    Io(io::Error),
    /// The typed data layout rejected publication.
    Layout(LayoutError),
    /// Reading a part back from the journal failed.
    Journal(JournalError),
    /// A journal part did not validate as a PGM container.
    Part(PartError),
    /// The journal holds no parts, so there is nothing to seal.
    Empty,
    /// The journal and requested destination carry different identities.
    SegmentIdMismatch {
        /// Identity stored in the journal.
        journal: SegmentId,
        /// Requested final address.
        destination: SegmentId,
    },
    /// The writer produced a PGM that failed its own structural checks.
    GeneratedSegmentInvalid,
    /// An existing final PGM at the recovered identity is structurally invalid.
    ExistingSegmentInvalid,
    /// An existing valid PGM differs from the journal's deterministic result.
    ExistingSegmentMismatch,
    /// The combined section catalog exceeds the writer's fixed admission limit.
    CatalogTooLarge {
        /// Number of entries the next journal part would produce.
        attempted_entries: usize,
        /// Maximum supported entries in one segment.
        max_entries: usize,
    },
    /// Reserving bounded memory for the combined section catalog failed.
    CatalogAllocation(std::collections::TryReserveError),
    /// Two parts carry different non-zero `source_id`s.
    SourceIdMismatch {
        /// The first non-zero source id seen.
        expected: u64,
        /// A later, conflicting source id.
        got: u64,
    },
}

impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "segment io: {err}"),
            Self::Layout(err) => write!(f, "segment layout: {err}"),
            Self::Journal(err) => write!(f, "reading a journal part: {err}"),
            Self::Part(err) => write!(f, "invalid journal part: {err}"),
            Self::Empty => write!(f, "the journal holds no parts to seal"),
            Self::SegmentIdMismatch {
                journal,
                destination,
            } => write!(
                f,
                "journal segment id {journal} does not match destination {destination}"
            ),
            Self::GeneratedSegmentInvalid => {
                f.write_str("the generated segment failed structural validation")
            }
            Self::ExistingSegmentInvalid => {
                f.write_str("the existing segment failed structural validation")
            }
            Self::ExistingSegmentMismatch => {
                f.write_str("the existing segment differs from the recovered journal")
            }
            Self::CatalogTooLarge {
                attempted_entries,
                max_entries,
            } => write!(
                f,
                "segment catalog would contain {attempted_entries} entries, limit is {max_entries}"
            ),
            Self::CatalogAllocation(error) => {
                write!(f, "reserving the bounded segment catalog failed: {error}")
            }
            Self::SourceIdMismatch { expected, got } => {
                write!(f, "journal mixes source_id {expected} and {got}")
            }
        }
    }
}

impl Error for SealError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Layout(err) => Some(err),
            Self::Journal(err) => Some(err),
            Self::Part(err) => Some(err),
            Self::CatalogAllocation(error) => Some(error),
            Self::Empty
            | Self::SegmentIdMismatch { .. }
            | Self::GeneratedSegmentInvalid
            | Self::ExistingSegmentInvalid
            | Self::ExistingSegmentMismatch
            | Self::CatalogTooLarge { .. }
            | Self::SourceIdMismatch { .. } => None,
        }
    }
}

impl From<io::Error> for SealError {
    fn from(err: io::Error) -> Self {
        Self::Io(err)
    }
}

impl From<JournalError> for SealError {
    fn from(err: JournalError) -> Self {
        Self::Journal(err)
    }
}

impl From<LayoutError> for SealError {
    fn from(err: LayoutError) -> Self {
        Self::Layout(err)
    }
}

/// Seal journal parts into the immutable segment at `address`.
///
/// The final PGM is never overwritten. Call `Journal::reset` only after `Ok`.
///
/// # Errors
///
/// Returns [`SealError`] when the journal is empty, a part is invalid, I/O
/// fails, or an existing final segment cannot be proven byte-identical.
pub fn seal(
    journal: &Journal,
    owner: &WriterOwner,
    address: SegmentAddress,
) -> Result<SealSummary, SealError> {
    if journal.parts().is_empty() {
        return Err(SealError::Empty);
    }
    if let Some(segment_id) = journal.segment_id()
        && segment_id != address.id
    {
        return Err(SealError::SegmentIdMismatch {
            journal: segment_id,
            destination: address.id,
        });
    }
    let mut temporary = owner.create_pgm_temp(address)?;
    let summary = write_tmp(journal, &mut temporary)?;
    let generated = temporary.try_clone_file()?;
    if !validate_segment(&generated, summary)? {
        return Err(SealError::GeneratedSegmentInvalid);
    }
    match temporary.publish() {
        Ok(()) => Ok(summary),
        Err(LayoutError::SegmentAlreadyExists { .. }) => {
            let existing = owner.root().open_pgm(address)?;
            let existing_identity = FileIdentity::from_file(&existing)?;
            if !validate_segment(&existing, summary)? {
                return Err(SealError::ExistingSegmentInvalid);
            }
            if !files_equal(&generated, &existing)? {
                return Err(SealError::ExistingSegmentMismatch);
            }
            if FileIdentity::from_file(&existing)? != existing_identity {
                return Err(SealError::ExistingSegmentMismatch);
            }
            let named_existing = owner.root().open_pgm(address)?;
            if FileIdentity::from_file(&named_existing)? != existing_identity {
                return Err(SealError::ExistingSegmentMismatch);
            }
            temporary.discard()?;
            Ok(summary)
        }
        Err(error) => Err(SealError::Layout(error)),
    }
}

fn validate_segment(file: &File, expected: SealSummary) -> Result<bool, io::Error> {
    let length = file.metadata()?.len();
    if length != expected.bytes {
        return Ok(false);
    }
    let minimum = MAGIC
        .len()
        .checked_add(META_LEN)
        .and_then(|value| value.checked_add(TAIL_INDEX_LEN))
        .expect("fixed PGM lengths fit usize");
    if length < minimum as u64 {
        return Ok(false);
    }

    let mut magic = [0_u8; MAGIC.len()];
    file.read_exact_at(&mut magic, 0)?;
    if magic != MAGIC {
        return Ok(false);
    }

    let tail_at = length - TAIL_INDEX_LEN as u64;
    let mut tail_bytes = [0_u8; TAIL_INDEX_LEN];
    file.read_exact_at(&mut tail_bytes, tail_at)?;
    let Ok(tail) = TailIndex::decode(tail_bytes) else {
        return Ok(false);
    };
    let expected_catalog_len = expected
        .sections
        .checked_mul(ENTRY_LEN)
        .and_then(|value| value.checked_add(META_LEN));
    let Some(expected_catalog_len) = expected_catalog_len else {
        return Ok(false);
    };
    if expected_catalog_len > MAX_CATALOG_BYTES
        || usize::try_from(tail.catalog_len).ok() != Some(expected_catalog_len)
    {
        return Ok(false);
    }
    let catalog_at = match tail_at.checked_sub(u64::from(tail.catalog_len)) {
        Some(offset) if offset >= MAGIC.len() as u64 => offset,
        _ => return Ok(false),
    };
    let mut catalog_bytes = vec![0_u8; expected_catalog_len];
    file.read_exact_at(&mut catalog_bytes, catalog_at)?;
    let Ok(catalog) = Catalog::view(&catalog_bytes) else {
        return Ok(false);
    };
    if catalog.format_version != FORMAT_VERSION
        || usize::try_from(catalog.entry_count).ok() != Some(expected.sections)
        || catalog.min_ts != expected.min_ts
        || catalog.max_ts != expected.max_ts
    {
        return Ok(false);
    }
    Ok(catalog.entries().all(|entry| {
        entry.offset >= MAGIC.len() as u64
            && entry
                .offset
                .checked_add(entry.len)
                .is_some_and(|end| end <= catalog_at)
    }))
}

fn files_equal(left: &File, right: &File) -> Result<bool, io::Error> {
    let left_identity = FileIdentity::from_file(left)?;
    let right_identity = FileIdentity::from_file(right)?;
    let length = left_identity.len;
    if right_identity.len != length {
        return Ok(false);
    }
    let mut left_buffer = vec![0_u8; COMPARE_BUFFER_BYTES].into_boxed_slice();
    let mut right_buffer = vec![0_u8; COMPARE_BUFFER_BYTES].into_boxed_slice();
    let mut offset = 0_u64;
    while offset < length {
        let remaining = usize::try_from((length - offset).min(COMPARE_BUFFER_BYTES as u64))
            .map_err(|_overflow| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "comparison chunk does not fit the address space",
                )
            })?;
        left.read_exact_at(&mut left_buffer[..remaining], offset)?;
        right.read_exact_at(&mut right_buffer[..remaining], offset)?;
        if left_buffer[..remaining] != right_buffer[..remaining] {
            return Ok(false);
        }
        seal_test_hook!(AfterFirstComparisonChunk);
        offset = offset
            .checked_add(remaining as u64)
            .expect("comparison offset is bounded by file length");
    }
    Ok(FileIdentity::from_file(left)? == left_identity
        && FileIdentity::from_file(right)? == right_identity)
}

fn checked_catalog_entries(current: usize, additional: usize) -> Result<usize, SealError> {
    let attempted_entries = current
        .checked_add(additional)
        .ok_or(SealError::CatalogTooLarge {
            attempted_entries: usize::MAX,
            max_entries: MAX_CATALOG_ENTRIES,
        })?;
    if attempted_entries > MAX_CATALOG_ENTRIES {
        return Err(SealError::CatalogTooLarge {
            attempted_entries,
            max_entries: MAX_CATALOG_ENTRIES,
        });
    }
    Ok(attempted_entries)
}

/// Write the merged segment to `tmp` and flush the encoder.
///
/// Publication synchronizes the file and its parent directories.
fn write_tmp(journal: &Journal, temporary: &mut PgmTemp<'_>) -> Result<SealSummary, SealError> {
    let mut out = BufWriter::new(temporary.file_mut());

    out.write_all(&MAGIC)?;
    let mut offset = MAGIC.len() as u64;
    let mut entries: Vec<Entry> = Vec::new();
    // Dictionary-only parts leave this empty interval unchanged.
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;
    let mut source_id = 0_u64;

    for &part_ref in journal.parts() {
        let part = journal.read_part(part_ref)?;
        // Recheck bodies immediately before publication. The journal may have
        // changed on disk after append even though its frame remained valid.
        let catalog = validate_part(&part).map_err(SealError::Part)?;
        checked_catalog_entries(entries.len(), catalog.entries.len())?;
        entries
            .try_reserve_exact(catalog.entries.len())
            .map_err(SealError::CatalogAllocation)?;
        min_ts = min_ts.min(catalog.min_ts);
        max_ts = max_ts.max(catalog.max_ts);
        if catalog.source_id != 0 {
            if source_id != 0 && source_id != catalog.source_id {
                return Err(SealError::SourceIdMismatch {
                    expected: source_id,
                    got: catalog.source_id,
                });
            }
            source_id = catalog.source_id;
        }
        for entry in &catalog.entries {
            // `validate_part` already bounded and checksummed the body slice.
            #[expect(
                clippy::cast_possible_truncation,
                reason = "validate_part bounds offset and len by the part length, a usize"
            )]
            let body = {
                let start = entry.offset as usize;
                &part[start..start + entry.len as usize]
            };
            out.write_all(body)?;
            entries.push(Entry { offset, ..*entry });
            offset = offset
                .checked_add(entry.len)
                .ok_or_else(|| io::Error::other("segment body length overflow"))?;
        }
    }

    // A segment with no timestamped sections records 0..0.
    if min_ts > max_ts {
        min_ts = 0;
        max_ts = 0;
    }

    let sections = entries.len();
    let catalog = Catalog {
        entries,
        min_ts,
        max_ts,
        source_id,
        format_version: FORMAT_VERSION,
    };
    catalog.write_encoded(&mut out)?;

    let file = out.into_inner().map_err(io::IntoInnerError::into_error)?;
    let bytes = file.metadata()?.len();
    Ok(SealSummary {
        sections,
        bytes,
        min_ts,
        max_ts,
    })
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;
    use std::os::unix::fs::FileExt as _;

    use kronika_format::{DictLimits, MAGIC, validate_part};
    use kronika_layout::{
        ACTIVE_JOURNAL_NAME, DataRoot, FileIdentity, LayoutLimits, SegmentAddress, SegmentId,
        WriterOwner,
    };
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::{Bytes, Ts, VerifiedSection, decode_any};

    use super::{
        MAX_CATALOG_ENTRIES, SealError, arm_after_first_comparison_chunk, checked_catalog_entries,
        seal,
    };
    use crate::{Interner, Journal, JournalConfig, SectionBuffers, dict};

    const SEGMENT_ID: i64 = 1_709_164_800_000_000;

    fn writer(directory: &tempfile::TempDir) -> WriterOwner {
        DataRoot::open(directory.path())
            .unwrap()
            .acquire_writer(LayoutLimits::default())
            .unwrap()
    }

    fn address() -> SegmentAddress {
        SegmentAddress::new(SegmentId::new(SEGMENT_ID).unwrap()).unwrap()
    }

    fn bgwriter(ts: i64) -> BgwriterCheckpointer {
        BgwriterCheckpointer {
            ts: Ts(ts),
            checkpoints_timed: 10,
            checkpoints_req: 2,
            checkpoint_write_time: 1.0,
            checkpoint_sync_time: 2.0,
            buffers_checkpoint: 4096,
            restartpoints_timed: None,
            restartpoints_req: None,
            restartpoints_done: None,
            buffers_clean: 512,
            maxwritten_clean: 3,
            buffers_backend: Some(128),
            buffers_backend_fsync: Some(0),
            buffers_alloc: 9000,
            bgwriter_stats_reset: Ts(ts - 100),
            checkpointer_stats_reset: None,
        }
    }

    /// One collection window: buffer a bgwriter row and append its part.
    fn append_window(journal: &mut Journal, ts: i64) {
        let mut buffers = SectionBuffers::new();
        buffers.push(bgwriter(ts)).expect("buffer not full");
        let part = buffers.flush(&[], 0).expect("encode").expect("a part");
        journal
            .append(address().id, &part)
            .expect("append under the segment identity");
    }

    #[test]
    fn seals_journal_parts_into_a_readable_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = writer(&dir);
        let segment_path = owner
            .root()
            .diagnostic_file_path(address(), kronika_layout::FileKind::Pgm);

        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1_000);
        append_window(&mut journal, 2_000);

        let summary = seal(&journal, &owner, address()).expect("seal");
        assert_eq!(summary.sections, 2, "one bgwriter section per part");
        assert_eq!((summary.min_ts, summary.max_ts), (1_000, 2_000));

        // A chartless segment has the same container shape as a PGM part.
        let segment = std::fs::read(&segment_path).expect("read segment");
        assert_eq!(u64::try_from(segment.len()).unwrap(), summary.bytes);
        let catalog = validate_part(&segment).expect("segment validates");
        assert_eq!(catalog.entries.len(), 2);

        // Repeated sections decode in catalog order.
        for entry in &catalog.entries {
            assert_eq!(entry.type_id, 1_006_001);
            let start = usize::try_from(entry.offset).unwrap();
            let len = usize::try_from(entry.len).unwrap();
            let body = Bytes::copy_from_slice(&segment[start..start + len]);
            let verified = VerifiedSection::verify(body, entry.crc32c, kronika_format::crc32c)
                .expect("section crc matches");
            assert_eq!(
                decode_any(1_006_001, verified).expect("decode").stats.rows,
                1
            );
        }
    }

    #[test]
    fn a_sealed_segment_carries_the_window_dictionary() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = writer(&dir);
        let segment_path = owner
            .root()
            .diagnostic_file_path(address(), kronika_layout::FileKind::Pgm);
        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");

        // Intern two short strings and encode the window dictionary.
        let mut interner = Interner::new(DictLimits::new(4096, 1 << 20).expect("limits"));
        interner.intern(b"db-host-01").expect("intern");
        interner.intern(b"node-7").expect("intern");
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");

        // One data section plus the dictionary in a single part.
        let mut buffers = SectionBuffers::new();
        buffers.push(bgwriter(1_000)).expect("buffer not full");
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush")
            .expect("a part");
        journal.append(address().id, &part).expect("append");

        let summary = seal(&journal, &owner, address()).expect("seal");
        assert_eq!(summary.sections, 2, "bgwriter + dict.strings");

        let segment = std::fs::read(&segment_path).expect("read segment");
        let catalog = validate_part(&segment).expect("segment validates");
        let dict_entry = catalog
            .entries
            .iter()
            .find(|entry| entry.type_id == kronika_registry::DICT_STRINGS_TYPE_ID)
            .expect("the dictionary section reached the segment");
        assert_eq!(dict_entry.rows, 2, "both interned strings");
        let start = usize::try_from(dict_entry.offset).unwrap();
        let end = start + usize::try_from(dict_entry.len).unwrap();
        assert_eq!(&segment[start..start + 4], b"PAR1", "a Parquet dict body");
        assert_eq!(&segment[end - 4..end], b"PAR1", "intact to its last byte");
    }

    #[test]
    fn sealing_an_empty_journal_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = writer(&dir);
        let journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
        assert!(matches!(
            seal(&journal, &owner, address()),
            Err(SealError::Empty)
        ));
    }

    #[test]
    fn identical_existing_segment_completes_recovery_without_overwrite() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = writer(&dir);
        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1);

        let first = seal(&journal, &owner, address()).expect("first seal");
        let second = seal(&journal, &owner, address()).expect("idempotent recovery");
        assert_eq!(second, first);
    }

    #[test]
    fn different_existing_segment_preserves_the_recovery_conflict() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = writer(&dir);
        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1);
        seal(&journal, &owner, address()).expect("first seal");

        let path = owner
            .root()
            .diagnostic_file_path(address(), kronika_layout::FileKind::Pgm);
        let file = std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .expect("open published segment");
        file.write_all_at(&[0xFF], MAGIC.len() as u64)
            .expect("change one body byte without changing the catalog");
        file.sync_all().expect("persist conflicting bytes");

        assert!(matches!(
            seal(&journal, &owner, address()),
            Err(SealError::ExistingSegmentMismatch)
        ));
    }

    #[test]
    fn body_corruption_after_append_prevents_publication() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = writer(&dir);
        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1);

        let part_ref = journal.parts()[0];
        let part = journal.read_part(part_ref).expect("read valid part");
        let catalog = validate_part(&part).expect("valid appended part");
        let body_at = u64::try_from(part_ref.offset()).unwrap() + catalog.entries[0].offset;
        let journal_file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(dir.path().join(ACTIVE_JOURNAL_NAME))
            .expect("open journal for corruption");
        let mut original = [0_u8; 1];
        journal_file
            .read_exact_at(&mut original, body_at)
            .expect("read body byte");
        journal_file
            .write_all_at(&[original[0] ^ 0xFF], body_at)
            .expect("corrupt section body");
        journal_file.sync_all().expect("persist corruption");

        assert!(matches!(
            seal(&journal, &owner, address()),
            Err(SealError::Part(
                kronika_format::PartError::SectionCrc { .. }
            ))
        ));
        assert_eq!(journal.parts().len(), 1, "journal remains recoverable");
        assert!(
            !owner
                .root()
                .diagnostic_file_path(address(), kronika_layout::FileKind::Pgm)
                .exists(),
            "a corrupt journal body must not be published"
        );
    }

    #[test]
    fn same_inode_rewrite_during_recovery_comparison_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = writer(&dir);
        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1);
        seal(&journal, &owner, address()).expect("first seal");

        let path = owner
            .root()
            .diagnostic_file_path(address(), kronika_layout::FileKind::Pgm);
        let before_file = std::fs::OpenOptions::new()
            .read(true)
            .open(&path)
            .expect("open final");
        let before = FileIdentity::from_file(&before_file).expect("initial identity");
        let path_for_hook = path;
        let hook = arm_after_first_comparison_chunk(move || {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path_for_hook)
                .expect("open final for rewrite");
            let original_modified = file.metadata().unwrap().modified().unwrap();
            let mut byte = [0_u8; 1];
            file.read_exact_at(&mut byte, MAGIC.len() as u64)
                .expect("read compared byte");
            file.write_all_at(&[byte[0] ^ 0xFF], MAGIC.len() as u64)
                .expect("rewrite compared byte");
            file.write_all_at(&byte, MAGIC.len() as u64)
                .expect("restore compared byte");
            file.set_times(FileTimes::new().set_modified(original_modified))
                .expect("restore mtime");
            file.sync_all().expect("persist restored content");
            assert_ne!(
                FileIdentity::from_file(&file).expect("changed identity"),
                before,
                "ctime must expose a rewrite even after restoring bytes and mtime"
            );
        });

        assert!(matches!(
            seal(&journal, &owner, address()),
            Err(SealError::ExistingSegmentMismatch)
        ));
        hook.assert_consumed();
        assert_eq!(journal.parts().len(), 1, "journal must not be reset");
    }

    #[test]
    fn final_name_replacement_during_recovery_comparison_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let owner = writer(&dir);
        let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1);
        seal(&journal, &owner, address()).expect("first seal");

        let path = owner
            .root()
            .diagnostic_file_path(address(), kronika_layout::FileKind::Pgm);
        let replacement_bytes = std::fs::read(&path).expect("read final");
        let displaced = path.with_extension("pgm.displaced");
        let path_for_hook = path;
        let hook = arm_after_first_comparison_chunk(move || {
            std::fs::rename(&path_for_hook, &displaced).expect("displace final name");
            std::fs::write(&path_for_hook, &replacement_bytes)
                .expect("replace with byte-identical inode");
            std::fs::OpenOptions::new()
                .read(true)
                .open(&path_for_hook)
                .unwrap()
                .sync_all()
                .expect("persist replacement");
        });

        assert!(matches!(
            seal(&journal, &owner, address()),
            Err(SealError::ExistingSegmentMismatch)
        ));
        hook.assert_consumed();
        assert_eq!(journal.parts().len(), 1, "journal must not be reset");
    }

    #[test]
    fn catalog_entry_limit_is_checked_without_allocating_the_limit() {
        assert_eq!(
            checked_catalog_entries(MAX_CATALOG_ENTRIES - 1, 1).unwrap(),
            MAX_CATALOG_ENTRIES
        );
        assert!(matches!(
            checked_catalog_entries(MAX_CATALOG_ENTRIES, 1),
            Err(SealError::CatalogTooLarge {
                attempted_entries,
                max_entries
            }) if attempted_entries == MAX_CATALOG_ENTRIES + 1
                && max_entries == MAX_CATALOG_ENTRIES
        ));
        assert!(matches!(
            checked_catalog_entries(usize::MAX, 1),
            Err(SealError::CatalogTooLarge {
                attempted_entries: usize::MAX,
                ..
            })
        ));
    }
}
