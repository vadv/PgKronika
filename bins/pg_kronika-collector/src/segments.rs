use crate::config::Config;
use crate::logging::{
    LogLevel, duration_ms, field, log_event, log_flush_summary, log_journal_append, summary_rows,
};
use anyhow::{Context, Result};
use kronika_writer::{
    FlushedPart, Interner, Journal, JournalConfig, JournalError, SealAdmission, SealError,
    SealOptions, SectionBuffers, dict, seal_with_options,
};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Fail before opening or mutating `active.parts` when the owned directory
/// still contains a rejected pre-compaction sealed segment.
pub(crate) fn verify_output_contract(out_dir: &Path) -> Result<()> {
    kronika_store::LocalDir::open(out_dir)
        .context("verify the output directory uses the current PGM contract")?;
    Ok(())
}

/// The open (not yet sealed) segment: its file name comes from the first
/// window's timestamp, its age from the moment that window was appended.
#[derive(Debug, Default, Clone)]
pub(crate) struct SegmentState {
    first_ts: Option<i64>,
    opened_at: Option<Instant>,
    admission: SealAdmission,
}

impl SegmentState {
    /// Register the appended window; the first one opens the segment.
    pub(crate) fn on_window_appended(&mut self, ts: i64, now: Instant, admission: SealAdmission) {
        if self.first_ts.is_none() {
            self.first_ts = Some(ts);
            self.opened_at = Some(now);
        }
        self.admission = admission;
    }

    /// Whether the open segment has reached `max_age`.
    pub(crate) fn age_expired(&self, now: Instant, max_age: Duration) -> bool {
        self.opened_at
            .is_some_and(|opened| now.duration_since(opened) >= max_age)
    }

    pub(crate) fn time_until_age(&self, now: Instant, max_age: Duration) -> Option<Duration> {
        Some(max_age.saturating_sub(now.saturating_duration_since(self.opened_at?)))
    }

    #[cfg(test)]
    pub(crate) const fn first_ts(&self) -> Option<i64> {
        self.first_ts
    }
}

/// Why the open segment must seal now, or `None` to keep collecting.
///
/// Forced ticks seal immediately, `max_bytes = 0` keeps the legacy one-tick
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
    config: &Config,
    segment: &mut SegmentState,
    reason: &'static str,
) -> Result<PathBuf> {
    let first_ts = segment
        .first_ts
        .context("sealing an open segment requires an appended window")?;
    let dest = config.out_dir.join(format!("{first_ts}.pgm"));
    let journal_bytes = journal.bytes();
    let journal_parts = journal.parts().len();
    let started = Instant::now();
    let summary = seal_with_options(
        journal,
        &dest,
        SealOptions {
            limits: config.seal_limits,
            cancelled: None,
        },
    )
    .context("seal the segment")?;
    log_event(
        LogLevel::Info,
        "segment_seal_finish",
        &[
            field("segment_path", dest.display()),
            field("segment_id", first_ts),
            field("source_id", config.source_id),
            field("reason", reason),
            field("sections", summary.sections),
            field("rows", summary.rows),
            field("segment_bytes", summary.bytes),
            field("journal_bytes", journal_bytes),
            field("journal_parts", journal_parts),
            field("spill_bytes", summary.spill_bytes),
            field("write_bytes", summary.write_bytes),
            field("admitted_memory_bytes", summary.admitted_memory_bytes),
            field("publication", format!("{:?}", summary.publication)),
            field("min_ts", summary.min_ts),
            field("max_ts", summary.max_ts),
            field("elapsed_ms", duration_ms(started.elapsed())),
        ],
    );
    // Leave active.parts intact if seal() fails.
    journal.reset().context("reset the journal after seal")?;
    *segment = SegmentState::default();
    Ok(dest)
}

/// Open the journal under the output directory and seal windows a previous
/// process left behind, so a restart loses no collected data.
pub(crate) fn open_collector_journal(
    out_dir: &Path,
    journal_max_bytes: u64,
    seal_limits: kronika_writer::SealLimits,
) -> Result<(Journal, Option<PathBuf>)> {
    let journal_config = JournalConfig {
        max_journal_len: usize::try_from(journal_max_bytes)
            .context("KRONIKA_JOURNAL_MAX_BYTES exceeds usize")?,
        ..JournalConfig::default()
    };
    let (mut journal, report) =
        Journal::open(&out_dir.join("active.parts"), journal_config).context("open the journal")?;
    if report.has_media_damage() {
        log_event(
            LogLevel::Warn,
            "journal_recovery_damaged",
            &[
                field("damaged_regions", report.damages.len()),
                field("truncated_torn_tail", report.truncated_torn_tail),
            ],
        );
        anyhow::bail!(
            "active.parts contains non-torn damage; preserving it and refusing collection"
        );
    }
    if journal.parts().is_empty() {
        return Ok((journal, None));
    }
    let dest = seal_recovered_journal(&mut journal, out_dir, seal_limits).inspect_err(|err| {
        log_event(
            LogLevel::Error,
            "journal_recovery_seal_failure",
            &[
                field("journal_bytes", journal.bytes()),
                field("journal_parts", journal.parts().len()),
                field("error", format!("{err:#}")),
            ],
        );
    })?;
    Ok((journal, dest))
}

/// Seal recovered windows under the earliest data timestamp they carry.
///
/// Parts without a data timestamp hold no rows (a dictionary needs a data
/// section to be referenced from), so a journal made only of those is reset
/// without producing a segment.
fn seal_recovered_journal(
    journal: &mut Journal,
    out_dir: &Path,
    seal_limits: kronika_writer::SealLimits,
) -> Result<Option<PathBuf>> {
    let mut first_ts: Option<i64> = None;
    for part in journal.parts().to_vec() {
        let body = journal.read_part(part).context("read a recovered part")?;
        let catalog = kronika_format::validate_part(&body).context("validate a recovered part")?;
        if catalog.min_ts != i64::MAX {
            first_ts = Some(first_ts.map_or(catalog.min_ts, |ts| ts.min(catalog.min_ts)));
        }
    }
    let Some(first_ts) = first_ts else {
        log_event(
            LogLevel::Info,
            "journal_recovery_empty",
            &[
                field("journal_bytes", journal.bytes()),
                field("journal_parts", journal.parts().len()),
                field("reason", "no_timestamped_sections"),
            ],
        );
        journal
            .reset()
            .context("reset a recovered journal with no data windows")?;
        return Ok(None);
    };
    let dest = out_dir.join(format!("{first_ts}.pgm"));
    let journal_bytes = journal.bytes();
    let journal_parts = journal.parts().len();
    let started = Instant::now();
    let summary = seal_with_options(
        journal,
        &dest,
        SealOptions {
            limits: seal_limits,
            cancelled: None,
        },
    )
    .context("seal the recovered segment")?;
    log_event(
        LogLevel::Info,
        "segment_seal_finish",
        &[
            field("segment_path", dest.display()),
            field("segment_id", first_ts),
            field("reason", "recovered"),
            field("sections", summary.sections),
            field("rows", summary.rows),
            field("segment_bytes", summary.bytes),
            field("journal_bytes", journal_bytes),
            field("journal_parts", journal_parts),
            field("spill_bytes", summary.spill_bytes),
            field("write_bytes", summary.write_bytes),
            field("admitted_memory_bytes", summary.admitted_memory_bytes),
            field("publication", format!("{:?}", summary.publication)),
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
    config: &Config,
    segment: &mut SegmentState,
    ts: i64,
    forced: bool,
    flushed: &FlushedPart,
) -> Result<Vec<(PathBuf, &'static str)>> {
    let mut sealed = Vec::new();
    let mut next_admission = match segment
        .admission
        .with_part(&flushed.body, config.seal_limits)
    {
        Ok(admission) => admission,
        Err(error) if error.is_admission_boundary() && segment.first_ts.is_some() => {
            let reason = admission_reason(&error);
            sealed.push((seal_open_segment(journal, config, segment, reason)?, reason));
            segment
                .admission
                .with_part(&flushed.body, config.seal_limits)
                .context("one collection window exceeds seal admission")?
        }
        Err(error) => return Err(anyhow::Error::new(error).context("admit collection window")),
    };

    let projected = projected_journal_bytes(journal, flushed)?;
    if segment.first_ts.is_some()
        && config.segment_max_bytes > 0
        && projected > config.segment_max_bytes
    {
        sealed.push((seal_open_segment(journal, config, segment, "size")?, "size"));
        next_admission = segment
            .admission
            .with_part(&flushed.body, config.seal_limits)
            .context("admit collection window after size seal")?;
    }

    if let Err(error) = journal.admit(&flushed.body) {
        let boundary = matches!(
            error,
            JournalError::Full { .. } | JournalError::TooManyParts { .. }
        );
        if boundary && segment.first_ts.is_some() {
            let reason = match error {
                JournalError::Full { .. } => "journal-full",
                JournalError::TooManyParts { .. } => "journal-parts",
                _ => unreachable!("guarded journal boundary"),
            };
            log_event(
                LogLevel::Warn,
                "journal_admission_boundary",
                &[
                    field("reason", reason),
                    field("journal_bytes", journal.bytes()),
                    field("journal_parts", journal.parts().len()),
                    field("part_bytes", flushed.summary.part_bytes),
                ],
            );
            sealed.push((seal_open_segment(journal, config, segment, reason)?, reason));
            next_admission = segment
                .admission
                .with_part(&flushed.body, config.seal_limits)
                .context("admit collection window after journal seal")?;
            journal
                .admit(&flushed.body)
                .context("one collection window exceeds the journal hard limits")?;
        } else {
            return Err(anyhow::Error::new(error).context("admit the part to the journal"));
        }
    }

    let append_started = Instant::now();
    let journal_bytes_before = journal.bytes();
    match journal.append(&flushed.body) {
        Ok(part_ref) => log_journal_append(
            &flushed.summary,
            part_ref.offset,
            part_ref.len,
            journal_bytes_before,
            journal.bytes(),
            append_started.elapsed(),
            false,
        ),
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
    segment.on_window_appended(ts, now, next_admission);
    let age = Duration::from_secs(config.segment_max_age_secs);
    if let Some(reason) = seal_reason(
        forced,
        journal.bytes(),
        config.segment_max_bytes,
        segment.age_expired(now, age),
    ) {
        sealed.push((seal_open_segment(journal, config, segment, reason)?, reason));
    }
    Ok(sealed)
}

fn projected_journal_bytes(journal: &Journal, flushed: &FlushedPart) -> Result<u64> {
    let current = u64::try_from(journal.bytes()).context("journal bytes exceed u64")?;
    let frame = u64::try_from(kronika_format::FRAME_HEADER_LEN)
        .context("frame header length exceeds u64")?
        .checked_add(u64::try_from(flushed.body.len()).context("collection part bytes exceed u64")?)
        .context("collection frame length overflows u64")?;
    current
        .checked_add(frame)
        .context("projected journal length overflows u64")
}

const fn admission_reason(error: &SealError) -> &'static str {
    match error {
        SealError::Resource {
            resource: kronika_writer::SealResource::Memory,
            ..
        } => "memory-limit",
        SealError::Resource {
            resource: kronika_writer::SealResource::InputSections,
            ..
        } => "section-limit",
        SealError::Resource {
            resource: kronika_writer::SealResource::ColumnPage,
            ..
        } => "page-limit",
        SealError::Resource {
            resource: kronika_writer::SealResource::SectionBody,
            ..
        } => "body-limit",
        SealError::Resource {
            resource: kronika_writer::SealResource::ReaderMemory,
            ..
        } => "reader-memory-limit",
        SealError::Codec(kronika_registry::CodecError::TooManyRows { .. }) => "row-limit",
        SealError::Dictionary(kronika_writer::DictionaryError::TooManyEntries { .. }) => {
            "dictionary-limit"
        }
        _ => "admission-limit",
    }
}
