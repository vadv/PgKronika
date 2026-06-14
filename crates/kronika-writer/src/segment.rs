//! Segment completion: merge the journal's parts into one immutable segment.
//!
//! The merge streams the journal one part at a time (peak memory is one part,
//! bounded by `max_part_len`), copies each part's section bodies into
//! `segment.pgm.tmp`, and writes the end catalog last. The temporary file is
//! linked into place with `O_EXCL` semantics so an existing segment is never
//! overwritten (segment-format.md, "Write and merge").
//!
//! A part may hold a section type already present in an earlier part; the
//! sections are copied verbatim and the catalog keeps them as repeated entries,
//! which the reader processes in order. Merging repeated sections of one type
//! into a single sorted, recompressed section is a later optimization of this
//! same path — it changes how bodies are written here, not the segment format.

use std::error::Error;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use kronika_format::{Catalog, Entry, FORMAT_VERSION, MAGIC, PartError, validate_part};

use crate::{Journal, JournalError};

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
    /// Reading a part back from the journal failed.
    Journal(JournalError),
    /// A journal part did not validate as a PGM container.
    Part(PartError),
    /// The journal holds no parts, so there is nothing to seal.
    Empty,
}

impl fmt::Display for SealError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(err) => write!(f, "segment io: {err}"),
            Self::Journal(err) => write!(f, "reading a journal part: {err}"),
            Self::Part(err) => write!(f, "invalid journal part: {err}"),
            Self::Empty => write!(f, "the journal holds no parts to seal"),
        }
    }
}

impl Error for SealError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            Self::Journal(err) => Some(err),
            Self::Part(err) => Some(err),
            Self::Empty => None,
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

/// Seal the journal's parts into an immutable segment at `dest`.
///
/// Streams the journal one part at a time, so peak memory is one part plus the
/// growing catalog, never the whole segment. The segment is written to a sibling
/// `*.tmp`, flushed, then linked to `dest`; `dest` is never overwritten, so a
/// segment already present at that path makes this fail with
/// [`io::ErrorKind::AlreadyExists`] rather than clobbering it.
///
/// The caller clears the journal (`Journal::reset`) only after this returns
/// `Ok`.
///
/// # Errors
///
/// [`SealError::Empty`] if the journal holds no parts; [`SealError::Part`] if a
/// part fails container validation; [`SealError::Journal`] or [`SealError::Io`]
/// on a read or filesystem failure.
pub fn seal(journal: &Journal, dest: &Path) -> Result<SealSummary, SealError> {
    if journal.parts().is_empty() {
        return Err(SealError::Empty);
    }

    let tmp = tmp_path(dest);
    let summary = write_tmp(journal, &tmp)?;
    // O_EXCL-style publish: hard-link the finished file into place (fails if
    // `dest` exists), then drop the temporary name. The data is already synced.
    if let Err(err) = fs::hard_link(&tmp, dest) {
        // Failed publish (commonly: `dest` already exists). Drop the temporary
        // best-effort and surface the publish error, not the cleanup's.
        fs::remove_file(&tmp).ok();
        return Err(SealError::Io(err));
    }
    fs::remove_file(&tmp)?;
    sync_parent_dir(dest)?;
    Ok(summary)
}

/// Append `.tmp` to the segment file name, keeping it in the same directory.
fn tmp_path(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_owned();
    name.push(".tmp");
    PathBuf::from(name)
}

/// Write the merged segment to `tmp` and fsync it. The caller publishes it.
fn write_tmp(journal: &Journal, tmp: &Path) -> Result<SealSummary, SealError> {
    let file = File::create(tmp)?;
    let mut out = BufWriter::new(file);

    out.write_all(&MAGIC)?;
    let mut offset = MAGIC.len() as u64;
    let mut entries: Vec<Entry> = Vec::new();
    let mut min_ts = i64::MAX;
    let mut max_ts = i64::MIN;
    let mut source_id = 0_u64;

    for &part_ref in journal.parts() {
        let part = journal.read_part(part_ref)?;
        let catalog = validate_part(&part).map_err(SealError::Part)?;
        min_ts = min_ts.min(catalog.min_ts);
        max_ts = max_ts.max(catalog.max_ts);
        if catalog.source_id != 0 {
            source_id = catalog.source_id;
        }
        for entry in &catalog.entries {
            // validate_part bounded every offset and len by the part length, a
            // usize, so the body slice is always in range.
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
            offset += entry.len;
        }
    }

    let sections = entries.len();
    let catalog = Catalog {
        entries,
        min_ts,
        max_ts,
        source_id,
        format_version: FORMAT_VERSION,
    };
    out.write_all(&catalog.encode())?;

    let file = out.into_inner().map_err(io::IntoInnerError::into_error)?;
    let bytes = file.metadata()?.len();
    file.sync_all()?;
    Ok(SealSummary {
        sections,
        bytes,
        min_ts,
        max_ts,
    })
}

/// fsync the directory holding `dest` so the new link survives a crash.
fn sync_parent_dir(dest: &Path) -> io::Result<()> {
    if let Some(dir) = dest.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use kronika_format::{DictLimits, validate_part};
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::{Bytes, Ts, VerifiedSection, decode_any};

    use super::{SealError, seal};
    use crate::{Interner, Journal, JournalConfig, SectionBuffers, dict};

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
        buffers.push(bgwriter(ts));
        let part = buffers.flush(&[], 0).expect("encode").expect("a part");
        journal.append(&part).expect("append");
    }

    #[test]
    fn seals_journal_parts_into_a_readable_segment() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal_path = dir.path().join("active.parts");
        let segment_path = dir.path().join("143000.pgm");

        let (mut journal, _) =
            Journal::open(&journal_path, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1_000);
        append_window(&mut journal, 2_000);

        let summary = seal(&journal, &segment_path).expect("seal");
        assert_eq!(summary.sections, 2, "one bgwriter section per part");
        assert_eq!((summary.min_ts, summary.max_ts), (1_000, 2_000));

        // A chartless segment is structurally a PGM part, so the same validator
        // checks the magic, the catalog CRC, and every section CRC.
        let segment = std::fs::read(&segment_path).expect("read segment");
        assert_eq!(u64::try_from(segment.len()).unwrap(), summary.bytes);
        let catalog = validate_part(&segment).expect("segment validates");
        assert_eq!(catalog.entries.len(), 2);

        // Each repeated bgwriter section decodes back through the registry with
        // the real CRC check, holding one row apiece.
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
        let segment_path = dir.path().join("d.pgm");
        let (mut journal, _) =
            Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                .expect("open journal");

        // Intern two short strings and encode the window dictionary.
        let mut interner = Interner::new(DictLimits::new(4096, 1 << 20).expect("limits"));
        interner.intern(b"db-host-01").expect("intern");
        interner.intern(b"node-7").expect("intern");
        let dict_sections = dict::encode(interner.window()).expect("encode dictionary");

        // One data section plus the dictionary in a single part.
        let mut buffers = SectionBuffers::new();
        buffers.push(bgwriter(1_000));
        let part = buffers
            .flush(&dict_sections, 0)
            .expect("flush")
            .expect("a part");
        journal.append(&part).expect("append");

        let summary = seal(&journal, &segment_path).expect("seal");
        assert_eq!(summary.sections, 2, "bgwriter + dict.strings");

        let segment = std::fs::read(&segment_path).expect("read segment");
        let catalog = validate_part(&segment).expect("segment validates");
        let dict_entry = catalog
            .entries
            .iter()
            .find(|entry| entry.type_id == dict::DICT_STRINGS_TYPE_ID)
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
        let (journal, _) =
            Journal::open(&dir.path().join("active.parts"), JournalConfig::default())
                .expect("open journal");
        assert!(matches!(
            seal(&journal, &dir.path().join("s.pgm")),
            Err(SealError::Empty)
        ));
    }

    #[test]
    fn an_existing_segment_is_never_overwritten() {
        let dir = tempfile::tempdir().expect("tempdir");
        let journal_path = dir.path().join("active.parts");
        let segment_path = dir.path().join("s.pgm");
        let (mut journal, _) =
            Journal::open(&journal_path, JournalConfig::default()).expect("open journal");
        append_window(&mut journal, 1);

        seal(&journal, &segment_path).expect("first seal");
        let err = seal(&journal, &segment_path).expect_err("must not overwrite");
        assert!(matches!(err, SealError::Io(e) if e.kind() == std::io::ErrorKind::AlreadyExists));
    }
}
