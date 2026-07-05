//! Byte-level file tailer with `PostgreSQL` log caps.

use std::fs::{self, File};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::{Duration, Instant};

use memchr::memchr;

use crate::parser::ParserKind;
use crate::state::TailState;

/// Maximum number of complete records returned in one read.
pub(crate) const MAX_LINES_PER_READ: usize = 4096;
/// Maximum bytes scanned in one read.
pub(crate) const MAX_BYTES_PER_READ: usize = 1_048_576;
/// Maximum time spent scanning one file.
pub(crate) const MAX_READ_DURATION: Duration = Duration::from_millis(50);
/// Fixed read buffer size.
pub(crate) const READ_BUF_SIZE: usize = 65_536;
/// Maximum stored prefix of one physical line.
pub(crate) const MAX_LINE_LEN: usize = 65_536;
/// Backlog threshold before skip-ahead.
pub(crate) const MAX_BACKLOG_BYTES: u64 = 64 * 1_048_576;
/// Tail window kept after backlog skip.
pub(crate) const BACKLOG_TAIL_BYTES: u64 = 1_048_576;

/// Tailer caps; defaults match the log-domain contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TailCaps {
    /// Maximum complete lines returned in one read.
    pub max_lines: usize,
    /// Maximum bytes scanned in one read.
    pub max_bytes: usize,
    /// Maximum read duration.
    pub max_duration: Duration,
    /// Maximum stored prefix of one physical line.
    pub max_line_len: usize,
    /// Backlog threshold before skip-ahead.
    pub max_backlog_bytes: u64,
    /// Tail window kept after backlog skip.
    pub backlog_tail_bytes: u64,
}

impl Default for TailCaps {
    fn default() -> Self {
        Self {
            max_lines: MAX_LINES_PER_READ,
            max_bytes: MAX_BYTES_PER_READ,
            max_duration: MAX_READ_DURATION,
            max_line_len: MAX_LINE_LEN,
            max_backlog_bytes: MAX_BACKLOG_BYTES,
            backlog_tail_bytes: BACKLOG_TAIL_BYTES,
        }
    }
}

/// Counters for bounded-tail degradation in one read.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TailGaps {
    /// Bytes skipped by backlog guard.
    pub backlog_bytes_skipped: u64,
    /// Bytes skipped by sparse-hole handling.
    pub sparse_bytes_skipped: u64,
    /// Physical lines truncated to a bounded prefix.
    pub truncated_lines: u32,
    /// Physical lines dropped because they contained NUL bytes.
    pub binary_lines_dropped: u32,
    /// Rotation/copytruncate detections.
    pub rotations: u32,
    /// Missing-file observations.
    pub missing_files: u32,
    /// Read cycles stopped by a line, byte, or time budget.
    pub budget_exhaustions: u32,
}

impl TailGaps {
    /// Whether any degradation counter is non-zero.
    #[must_use]
    pub const fn has_any(self) -> bool {
        self.backlog_bytes_skipped != 0
            || self.sparse_bytes_skipped != 0
            || self.truncated_lines != 0
            || self.binary_lines_dropped != 0
            || self.rotations != 0
            || self.missing_files != 0
            || self.budget_exhaustions != 0
    }
}

/// One bounded physical line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TailLine {
    pub(crate) offset: u64,
    pub(crate) bytes: Vec<u8>,
    pub(crate) truncated: bool,
}

/// Result of one tail read.
#[derive(Debug, Clone)]
pub(crate) struct TailBatch {
    pub(crate) lines: Vec<TailLine>,
    pub(crate) gaps: TailGaps,
    pub(crate) next_state: Option<TailState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    dev: u64,
    inode: u64,
}

/// Read a bounded batch from `path`.
///
/// # Errors
///
/// Returns filesystem errors except a missing file, which is reported as a
/// degradation counter so collection can retry later.
#[allow(
    clippy::too_many_lines,
    reason = "the tailer keeps offset, budgets, and newline state in one pass to make commit semantics auditable"
)]
pub(crate) fn read_batch(
    path: &Path,
    parser_kind: ParserKind,
    state: Option<&TailState>,
    start_at_beginning: bool,
    caps: TailCaps,
) -> io::Result<TailBatch> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(TailBatch {
                lines: Vec::new(),
                gaps: TailGaps {
                    missing_files: 1,
                    ..TailGaps::default()
                },
                next_state: state.cloned(),
            });
        }
        Err(err) => return Err(err),
    };
    let identity = file_identity(&metadata);
    let file_size = metadata.len();
    let mut gaps = TailGaps::default();
    let mut skip_until_newline = false;
    let mut cursor = initial_offset(
        path,
        parser_kind,
        state,
        start_at_beginning,
        file_size,
        identity,
        &mut gaps,
        &mut skip_until_newline,
    );

    if file_size <= cursor {
        return Ok(TailBatch {
            lines: Vec::new(),
            gaps,
            next_state: Some(next_state(
                path,
                parser_kind,
                identity,
                cursor,
                skip_until_newline,
            )),
        });
    }

    apply_backlog_guard(
        file_size,
        &mut cursor,
        &mut skip_until_newline,
        caps,
        &mut gaps,
    );

    let mut file = File::open(path)?;
    if let Some(data_offset) = seek_next_data(&file, cursor, file_size)? {
        if data_offset > cursor {
            gaps.sparse_bytes_skipped = gaps
                .sparse_bytes_skipped
                .saturating_add(data_offset - cursor);
            cursor = data_offset;
            skip_until_newline = true;
        }
    } else {
        gaps.sparse_bytes_skipped = gaps.sparse_bytes_skipped.saturating_add(file_size - cursor);
        return Ok(TailBatch {
            lines: Vec::new(),
            gaps,
            next_state: Some(next_state(path, parser_kind, identity, file_size, true)),
        });
    }
    file.seek(SeekFrom::Start(cursor))?;

    let started = Instant::now();
    let mut buf = vec![0_u8; READ_BUF_SIZE];
    let mut remaining = caps.max_bytes;
    let mut lines = Vec::new();
    let mut line = Vec::with_capacity(caps.max_line_len.min(READ_BUF_SIZE));
    let mut line_start = cursor;
    let mut committed = cursor;
    let mut truncated_current = false;

    while remaining > 0
        && lines.len() < caps.max_lines
        && started.elapsed() < caps.max_duration
        && cursor < file_size
    {
        let file_left = usize::try_from(file_size.saturating_sub(cursor)).unwrap_or(usize::MAX);
        let to_read = buf.len().min(remaining).min(file_left);
        if to_read == 0 {
            break;
        }
        let n = file.read(&mut buf[..to_read])?;
        if n == 0 {
            break;
        }
        remaining -= n;

        let mut consumed = 0_usize;
        while consumed < n && lines.len() < caps.max_lines {
            if started.elapsed() >= caps.max_duration {
                break;
            }
            let chunk = &buf[consumed..n];
            let newline = memchr(b'\n', chunk);
            let end = newline.unwrap_or(chunk.len());
            let segment = &chunk[..end];
            let segment_len = u64::try_from(segment.len()).unwrap_or(u64::MAX);
            consume_segment(
                segment,
                &mut line,
                line_start,
                &mut lines,
                &mut skip_until_newline,
                &mut truncated_current,
                caps,
                &mut gaps,
            );
            cursor = cursor.saturating_add(segment_len);
            consumed += end;

            if newline.is_some() {
                cursor = cursor.saturating_add(1);
                consumed += 1;
                if !skip_until_newline && !line.is_empty() {
                    lines.push(TailLine {
                        offset: line_start,
                        bytes: std::mem::take(&mut line),
                        truncated: truncated_current,
                    });
                }
                line.clear();
                truncated_current = false;
                skip_until_newline = false;
                committed = cursor;
                line_start = cursor;
            } else if skip_until_newline || truncated_current {
                committed = cursor;
            }
        }

        if consumed < n {
            gaps.budget_exhaustions = gaps.budget_exhaustions.saturating_add(1);
            break;
        }
    }

    if remaining == 0 || lines.len() >= caps.max_lines || started.elapsed() >= caps.max_duration {
        gaps.budget_exhaustions = gaps.budget_exhaustions.saturating_add(1);
    }

    let resume_offset = if skip_until_newline || truncated_current {
        committed.max(cursor)
    } else if line.is_empty() {
        cursor
    } else {
        committed
    };

    Ok(TailBatch {
        lines,
        gaps,
        next_state: Some(next_state(
            path,
            parser_kind,
            identity,
            resume_offset.min(file_size),
            skip_until_newline || truncated_current,
        )),
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "passing the hot parser state directly avoids allocating an intermediate state object per segment"
)]
fn consume_segment(
    segment: &[u8],
    line: &mut Vec<u8>,
    line_start: u64,
    lines: &mut Vec<TailLine>,
    skip_until_newline: &mut bool,
    truncated_current: &mut bool,
    caps: TailCaps,
    gaps: &mut TailGaps,
) {
    if segment.is_empty() {
        return;
    }
    if *skip_until_newline {
        return;
    }
    if memchr(0, segment).is_some() {
        line.clear();
        *skip_until_newline = true;
        *truncated_current = false;
        gaps.binary_lines_dropped = gaps.binary_lines_dropped.saturating_add(1);
        return;
    }
    let available = caps.max_line_len.saturating_sub(line.len());
    if segment.len() <= available {
        line.extend_from_slice(segment);
        return;
    }
    if available > 0 {
        line.extend_from_slice(&segment[..available]);
    }
    lines.push(TailLine {
        offset: line_start,
        bytes: std::mem::take(line),
        truncated: true,
    });
    *skip_until_newline = true;
    *truncated_current = true;
    gaps.truncated_lines = gaps.truncated_lines.saturating_add(1);
}

#[allow(
    clippy::too_many_arguments,
    reason = "the resume decision needs all persisted identity fields and emits gap counters in one place"
)]
fn initial_offset(
    path: &Path,
    parser_kind: ParserKind,
    state: Option<&TailState>,
    start_at_beginning: bool,
    file_size: u64,
    identity: FileIdentity,
    gaps: &mut TailGaps,
    skip_until_newline: &mut bool,
) -> u64 {
    let Some(state) = state else {
        return if start_at_beginning { 0 } else { file_size };
    };
    if state.path != path || state.parser_kind != parser_kind {
        gaps.rotations = gaps.rotations.saturating_add(1);
        return 0;
    }
    *skip_until_newline = state.skip_until_newline;
    if state.dev == identity.dev && state.inode == identity.inode && file_size >= state.offset {
        state.offset
    } else {
        gaps.rotations = gaps.rotations.saturating_add(1);
        0
    }
}

#[allow(
    clippy::missing_const_for_fn,
    reason = "keeping this non-const avoids widening the public const-surface of mutable helper logic"
)]
fn apply_backlog_guard(
    file_size: u64,
    cursor: &mut u64,
    skip_until_newline: &mut bool,
    caps: TailCaps,
    gaps: &mut TailGaps,
) {
    let lag = file_size.saturating_sub(*cursor);
    if lag <= caps.max_backlog_bytes {
        return;
    }
    let new_offset = file_size.saturating_sub(caps.backlog_tail_bytes);
    if new_offset > *cursor {
        gaps.backlog_bytes_skipped = gaps
            .backlog_bytes_skipped
            .saturating_add(new_offset - *cursor);
        *cursor = new_offset;
        *skip_until_newline = true;
    }
}

fn next_state(
    path: &Path,
    parser_kind: ParserKind,
    identity: FileIdentity,
    offset: u64,
    skip_until_newline: bool,
) -> TailState {
    TailState {
        path: path.to_owned(),
        dev: identity.dev,
        inode: identity.inode,
        offset,
        parser_kind,
        skip_until_newline,
    }
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> FileIdentity {
    use std::os::unix::fs::MetadataExt;
    FileIdentity {
        dev: metadata.dev(),
        inode: metadata.ino(),
    }
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> FileIdentity {
    FileIdentity { dev: 0, inode: 0 }
}

#[cfg(target_os = "linux")]
fn seek_next_data(file: &File, offset: u64, file_size: u64) -> io::Result<Option<u64>> {
    use std::os::fd::AsRawFd;

    use nix::errno::Errno;
    use nix::unistd::{Whence, lseek};

    if offset >= file_size {
        return Ok(None);
    }
    let offset_i64 = i64::try_from(offset)
        .map_err(|_err| io::Error::new(io::ErrorKind::InvalidInput, "offset exceeds i64"))?;
    match lseek(file.as_raw_fd(), offset_i64, Whence::SeekData) {
        Ok(found) => u64::try_from(found)
            .map(Some)
            .map_err(|_err| io::Error::other("SEEK_DATA returned a negative offset")),
        Err(Errno::ENXIO) => Ok(None),
        Err(Errno::EINVAL | Errno::ENOTTY) => Ok(Some(offset)),
        Err(err) => Err(io::Error::from_raw_os_error(err as i32)),
    }
}

#[cfg(not(target_os = "linux"))]
fn seek_next_data(_file: &File, offset: u64, file_size: u64) -> io::Result<Option<u64>> {
    if offset >= file_size {
        Ok(None)
    } else {
        Ok(Some(offset))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::{MAX_LINE_LEN, TailCaps, read_batch};
    use crate::ParserKind;

    #[test]
    fn starts_at_end_without_state_by_default() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("postgresql.log");
        std::fs::write(&path, "old\n").expect("write");
        let batch =
            read_batch(&path, ParserKind::Stderr, None, false, TailCaps::default()).expect("read");
        assert!(batch.lines.is_empty());
        assert_eq!(
            batch.next_state.expect("state").offset,
            std::fs::metadata(&path).expect("metadata").len()
        );
    }

    #[test]
    fn reads_new_complete_lines_from_committed_state() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("postgresql.log");
        std::fs::write(&path, "old\n").expect("write");
        let initial =
            read_batch(&path, ParserKind::Stderr, None, false, TailCaps::default()).expect("read");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .expect("open");
        writeln!(file, "new 1").expect("append");
        writeln!(file, "new 2").expect("append");
        let batch = read_batch(
            &path,
            ParserKind::Stderr,
            initial.next_state.as_ref(),
            false,
            TailCaps::default(),
        )
        .expect("read");
        let lines: Vec<_> = batch
            .lines
            .iter()
            .map(|line| std::str::from_utf8(&line.bytes).expect("utf8"))
            .collect();
        assert_eq!(lines, ["new 1", "new 2"]);
    }

    #[test]
    fn truncates_long_lines_before_unbounded_allocation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("postgresql.log");
        let long = "x".repeat(MAX_LINE_LEN + 100);
        std::fs::write(&path, format!("{long}\n")).expect("write");
        let batch =
            read_batch(&path, ParserKind::Stderr, None, true, TailCaps::default()).expect("read");
        assert_eq!(batch.lines.len(), 1);
        assert_eq!(batch.lines[0].bytes.len(), MAX_LINE_LEN);
        assert!(batch.lines[0].truncated);
        assert_eq!(batch.gaps.truncated_lines, 1);
    }

    #[test]
    fn drops_binary_lines_and_resumes_after_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("postgresql.log");
        std::fs::write(&path, b"bad\0line\nok\n").expect("write");
        let batch =
            read_batch(&path, ParserKind::Stderr, None, true, TailCaps::default()).expect("read");
        assert_eq!(batch.gaps.binary_lines_dropped, 1);
        assert_eq!(batch.lines.len(), 1);
        assert_eq!(batch.lines[0].bytes, b"ok");
    }

    #[test]
    fn backlog_skip_emits_gap_and_keeps_tail_window() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("postgresql.log");
        std::fs::write(&path, "a\nb\nc\n").expect("write");
        let caps = TailCaps {
            max_backlog_bytes: 3,
            backlog_tail_bytes: 2,
            ..TailCaps::default()
        };
        let batch = read_batch(&path, ParserKind::Stderr, None, true, caps).expect("read");
        assert!(batch.gaps.backlog_bytes_skipped > 0);
    }

    #[test]
    fn copytruncate_resets_to_file_start() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("postgresql.log");
        std::fs::write(&path, "old line\n").expect("write");
        let first =
            read_batch(&path, ParserKind::Stderr, None, false, TailCaps::default()).expect("read");
        std::fs::write(&path, "new\n").expect("truncate");
        let batch = read_batch(
            &path,
            ParserKind::Stderr,
            first.next_state.as_ref(),
            false,
            TailCaps::default(),
        )
        .expect("read");
        assert_eq!(batch.gaps.rotations, 1);
        assert_eq!(batch.lines[0].bytes, b"new");
    }

    #[test]
    fn partial_line_is_not_committed_until_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("postgresql.log");
        std::fs::write(&path, "partial").expect("write");
        let batch =
            read_batch(&path, ParserKind::Stderr, None, true, TailCaps::default()).expect("read");
        assert!(batch.lines.is_empty());
        assert_eq!(batch.next_state.expect("state").offset, 0);
    }

    #[test]
    fn max_line_budget_limits_one_read() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("postgresql.log");
        std::fs::write(&path, "one\ntwo\n").expect("write");
        let caps = TailCaps {
            max_lines: 1,
            ..TailCaps::default()
        };
        let batch = read_batch(&path, ParserKind::Stderr, None, true, caps).expect("read");
        assert_eq!(batch.lines.len(), 1);
        assert!(batch.gaps.budget_exhaustions > 0);
    }

    #[test]
    fn missing_file_is_a_gap_not_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("missing.log");
        let batch =
            read_batch(&path, ParserKind::Stderr, None, true, TailCaps::default()).expect("read");
        assert!(batch.lines.is_empty());
        assert_eq!(batch.gaps.missing_files, 1);
    }

    #[test]
    fn helper_uses_saturating_u32_conversion() {
        assert_eq!(crate::u32_saturating(u64::from(u32::MAX) + 1), u32::MAX);
    }
}
