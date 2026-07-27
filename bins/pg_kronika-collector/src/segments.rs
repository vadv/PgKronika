use crate::config::Config;
use crate::logging::{
    LogLevel, duration_ms, field, log_event, log_flush_summary, log_journal_append, summary_rows,
};
use anyhow::{Context, Result};
use kronika_format::{Catalog, FORMAT_VERSION, MAGIC, TAIL_INDEX_LEN, TailIndex};
use kronika_layout::{DataRoot, FileKind, LayoutLimits, SegmentAddress, SegmentId, WriterOwner};
use kronika_writer::{
    FlushedPart, Interner, Journal, JournalConfig, JournalError, SectionBuffers, dict, seal,
};
use std::fs::File;
use std::os::unix::fs::FileExt as _;
use std::path::PathBuf;
use std::time::{Duration, Instant};

const MAX_CATALOG_BYTES: u64 = 64 * 1024 * 1024;

/// The open (not yet sealed) segment: its file name comes from the first
/// window's timestamp, its age from the moment that window was appended.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct SegmentState {
    first_id: Option<SegmentId>,
    opened_at: Option<Instant>,
    published_pending_reset: bool,
}

impl SegmentState {
    /// Register the appended window; the first one opens the segment.
    pub(crate) const fn on_window_appended(&mut self, id: SegmentId, now: Instant) {
        if self.first_id.is_none() {
            self.first_id = Some(id);
            self.opened_at = Some(now);
        }
    }

    /// Whether the open segment has reached `max_age`.
    pub(crate) fn age_expired(&self, now: Instant, max_age: Duration) -> bool {
        self.opened_at
            .is_some_and(|opened| now.duration_since(opened) >= max_age)
    }

    pub(crate) fn time_until_age(&self, now: Instant, max_age: Duration) -> Option<Duration> {
        Some(max_age.saturating_sub(now.saturating_duration_since(self.opened_at?)))
    }

    pub(crate) fn ensure_append_allowed(&self) -> Result<()> {
        anyhow::ensure!(
            !self.published_pending_reset,
            "a PGM was published but active.parts was not reset; restart recovery is required"
        );
        Ok(())
    }

    pub(crate) const fn requires_restart(&self) -> bool {
        self.published_pending_reset
    }

    #[cfg(test)]
    pub(crate) const fn first_ts(&self) -> Option<i64> {
        match self.first_id {
            Some(id) => Some(id.get()),
            None => None,
        }
    }
}

/// Why the open segment must seal now, or `None` to keep collecting.
///
/// Forced ticks seal immediately, `max_bytes = 0` keeps the single-window
/// segment mode, and otherwise the raw journal size or segment age closes the
/// segment.
pub(crate) const fn seal_reason(
    forced: bool,
    journal_bytes: usize,
    max_bytes: u64,
    age_expired: bool,
) -> Option<&'static str> {
    if forced {
        Some("forced")
    } else if max_bytes == 0 {
        Some("tick")
    } else if journal_bytes as u64 >= max_bytes {
        Some("size")
    } else if age_expired {
        Some("age")
    } else {
        None
    }
}

/// Encode the buffered window into one journal-ready part.
pub(crate) fn encode_window(
    mut buffers: SectionBuffers,
    interner: &Interner,
    config: &Config,
) -> Result<FlushedPart> {
    let started = Instant::now();
    let dict_sections = dict::encode(interner.window()).context("encode the segment dictionary")?;
    let flushed = buffers
        .flush_with_summary(&dict_sections, config.source_id)
        .context("encode the collection window")?
        .context("a buffered row must yield a part")?;
    log_flush_summary(&flushed.summary, config.source_id, started.elapsed());
    Ok(flushed)
}

/// Seal the open segment into `<first_ts>.pgm` and reset the journal.
pub(crate) fn seal_open_segment(
    journal: &mut Journal,
    owner: &WriterOwner,
    config: &Config,
    segment: &mut SegmentState,
    reason: &'static str,
) -> Result<PathBuf> {
    seal_open_segment_with_reset(
        journal,
        owner,
        config.source_id,
        segment,
        reason,
        Journal::reset,
    )
}

pub(crate) fn seal_open_segment_with_reset<F>(
    journal: &mut Journal,
    owner: &WriterOwner,
    source_id: u64,
    segment: &mut SegmentState,
    reason: &'static str,
    reset: F,
) -> Result<PathBuf>
where
    F: FnOnce(&mut Journal) -> Result<(), JournalError>,
{
    let segment_id = segment
        .first_id
        .context("sealing an open segment requires an appended window")?;
    let address = SegmentAddress::new(segment_id).context("derive the segment UTC address")?;
    let dest = owner.root().diagnostic_file_path(address, FileKind::Pgm);
    let journal_bytes = journal.bytes();
    let journal_parts = journal.parts().len();
    let started = Instant::now();
    let summary = seal(journal, owner, address).context("seal the segment")?;
    log_event(
        LogLevel::Info,
        "segment_seal_finish",
        &[
            field("segment_path", dest.display()),
            field("segment_id", segment_id.get()),
            field("source_id", source_id),
            field("reason", reason),
            field("sections", summary.sections),
            field("segment_bytes", summary.bytes),
            field("journal_bytes", journal_bytes),
            field("journal_parts", journal_parts),
            field("min_ts", summary.min_ts),
            field("max_ts", summary.max_ts),
            field("elapsed_ms", duration_ms(started.elapsed())),
        ],
    );
    // Leave active.parts intact if seal() fails.
    segment.published_pending_reset = true;
    reset(journal).context("reset the journal after seal")?;
    *segment = SegmentState::default();
    Ok(dest)
}

/// Structurally validates every canonical PGM before startup may open or mutate
/// `active.parts`.
///
/// Section bodies remain lazy: readers verify their CRCs when a query selects
/// them. Startup reads only the magic, tail, and bounded catalog.
pub(crate) fn validate_existing_segments(root: &DataRoot, limits: LayoutLimits) -> Result<usize> {
    let snapshot = root
        .scan(limits)
        .context("scan existing segments before journal recovery")?;
    for segment in &snapshot.segments {
        let file = root
            .open_pgm(segment.address)
            .with_context(|| format!("open existing segment {}", segment.address.id))?;
        validate_existing_segment(&file)
            .with_context(|| format!("validate existing segment {}", segment.address.id))?;
    }
    Ok(snapshot.segments.len())
}

fn validate_existing_segment(file: &File) -> Result<()> {
    let file_len = file.metadata().context("stat PGM")?.len();
    let tail_at = file_len
        .checked_sub(TAIL_INDEX_LEN as u64)
        .context("PGM is shorter than its tail index")?;
    let mut tail_bytes = [0_u8; TAIL_INDEX_LEN];
    file.read_exact_at(&mut tail_bytes, tail_at)
        .context("read PGM tail index")?;
    let tail = TailIndex::decode(tail_bytes).context("decode PGM tail index")?;
    let catalog_len = u64::from(tail.catalog_len);
    anyhow::ensure!(
        catalog_len <= MAX_CATALOG_BYTES,
        "PGM catalog exceeds the {MAX_CATALOG_BYTES}-byte validation limit"
    );
    let catalog_at = tail_at
        .checked_sub(catalog_len)
        .context("PGM catalog length exceeds the file body")?;
    anyhow::ensure!(
        catalog_at >= MAGIC.len() as u64,
        "PGM catalog overlaps the file magic"
    );

    let mut magic = [0_u8; MAGIC.len()];
    file.read_exact_at(&mut magic, 0)
        .context("read PGM magic")?;
    anyhow::ensure!(magic == MAGIC, "invalid PGM magic");

    let catalog_size = usize::try_from(catalog_len).context("PGM catalog does not fit memory")?;
    let mut catalog_bytes = vec![0_u8; catalog_size];
    file.read_exact_at(&mut catalog_bytes, catalog_at)
        .context("read PGM catalog")?;
    let catalog = Catalog::view(&catalog_bytes).context("decode PGM catalog")?;
    anyhow::ensure!(
        catalog.format_version == FORMAT_VERSION,
        "unsupported PGM format version {}",
        catalog.format_version
    );

    for entry in catalog.entries() {
        let end = entry
            .offset
            .checked_add(entry.len)
            .context("PGM section range overflow")?;
        anyhow::ensure!(
            entry.offset >= MAGIC.len() as u64 && end <= catalog_at,
            "PGM section {} points outside the body",
            entry.type_id
        );
    }
    Ok(())
}

/// Open the journal under the output directory and seal windows a previous
/// process left behind, so a restart loses no collected data.
pub(crate) fn open_collector_journal(
    owner: &WriterOwner,
    journal_max_bytes: u64,
) -> Result<(Journal, Option<PathBuf>)> {
    let journal_config = JournalConfig {
        max_journal_len: usize::try_from(journal_max_bytes)
            .context("KRONIKA_JOURNAL_MAX_BYTES exceeds usize")?,
        ..JournalConfig::default()
    };
    let mut journal = Journal::open(owner, journal_config).context("open the journal")?;
    if journal.parts().is_empty() {
        return Ok((journal, None));
    }
    match seal_recovered_journal(&mut journal, owner) {
        Ok(dest) => Ok((journal, dest)),
        Err(err) => Err(err.context("recover the active journal without discarding it")),
    }
}

/// Seal recovered windows under the exact identity persisted in journal v1.
fn seal_recovered_journal(journal: &mut Journal, owner: &WriterOwner) -> Result<Option<PathBuf>> {
    let segment_id = journal
        .segment_id()
        .context("an active journal must carry SegmentId")?;
    let address = SegmentAddress::new(segment_id).context("derive the recovered UTC address")?;
    let dest = owner.root().diagnostic_file_path(address, FileKind::Pgm);
    let journal_bytes = journal.bytes();
    let journal_parts = journal.parts().len();
    let started = Instant::now();
    let summary = seal(journal, owner, address).context("seal the recovered segment")?;
    log_event(
        LogLevel::Info,
        "segment_seal_finish",
        &[
            field("segment_path", dest.display()),
            field("segment_id", segment_id.get()),
            field("reason", "recovered"),
            field("sections", summary.sections),
            field("segment_bytes", summary.bytes),
            field("journal_bytes", journal_bytes),
            field("journal_parts", journal_parts),
            field("min_ts", summary.min_ts),
            field("max_ts", summary.max_ts),
            field("elapsed_ms", duration_ms(started.elapsed())),
        ],
    );
    journal
        .reset()
        .context("reset the journal after the recovery seal")?;
    Ok(Some(dest))
}

pub(crate) fn append_window_and_maybe_seal(
    journal: &mut Journal,
    owner: &WriterOwner,
    config: &Config,
    segment: &mut SegmentState,
    ts: i64,
    forced: bool,
    flushed: &FlushedPart,
) -> Result<Vec<(PathBuf, &'static str)>> {
    segment.ensure_append_allowed()?;
    let mut sealed = Vec::new();
    let segment_id = match segment.first_id {
        Some(segment_id) => segment_id,
        None => SegmentId::new(ts).context("collection timestamp is outside the layout range")?,
    };
    let append_started = Instant::now();
    let journal_bytes_before = journal.bytes();
    match journal.append(segment_id, &flushed.body) {
        Ok(part_ref) => log_journal_append(
            &flushed.summary,
            part_ref.offset(),
            part_ref.len(),
            journal_bytes_before,
            journal.bytes(),
            append_started.elapsed(),
            false,
        ),
        Err(JournalError::Full { len, max }) if segment.first_id.is_some() => {
            log_event(
                LogLevel::Warn,
                "journal_full",
                &[
                    field("journal_bytes", len),
                    field("journal_max_bytes", max),
                    field("part_bytes", flushed.summary.part_bytes),
                    field("sections", flushed.summary.sections.len()),
                    field("section_rows", summary_rows(&flushed.summary)),
                ],
            );
            sealed.push((
                seal_open_segment(journal, owner, config, segment, "journal-full")?,
                "journal-full",
            ));
            let retry_started = Instant::now();
            let journal_bytes_before = journal.bytes();
            let part_ref = journal
                .append(
                    SegmentId::new(ts)
                        .context("collection timestamp is outside the layout range")?,
                    &flushed.body,
                )
                .context("append the window after an early seal")?;
            log_journal_append(
                &flushed.summary,
                part_ref.offset(),
                part_ref.len(),
                journal_bytes_before,
                journal.bytes(),
                retry_started.elapsed(),
                true,
            );
        }
        Err(other) => {
            log_event(
                LogLevel::Error,
                "journal_append_failure",
                &[
                    field("part_bytes", flushed.summary.part_bytes),
                    field("sections", flushed.summary.sections.len()),
                    field("section_rows", summary_rows(&flushed.summary)),
                    field("journal_bytes_before", journal_bytes_before),
                    field("error", &other),
                    field("elapsed_ms", duration_ms(append_started.elapsed())),
                ],
            );
            return Err(anyhow::Error::new(other).context("append the part to the journal"));
        }
    }
    let now = Instant::now();
    let active_id = journal
        .segment_id()
        .context("a successful journal append must persist SegmentId")?;
    segment.on_window_appended(active_id, now);
    let age = Duration::from_secs(config.segment_max_age_secs);
    if let Some(reason) = seal_reason(
        forced,
        journal.bytes(),
        config.segment_max_bytes,
        segment.age_expired(now, age),
    ) {
        sealed.push((
            seal_open_segment(journal, owner, config, segment, reason)?,
            reason,
        ));
    }
    Ok(sealed)
}
