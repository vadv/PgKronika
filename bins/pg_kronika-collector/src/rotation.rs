#![allow(
    clippy::missing_const_for_fn,
    clippy::ptr_arg,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::absurd_extreme_comparisons,
    clippy::collapsible_if,
    reason = "placeholder implementation for retention; will be rewritten for production"
)]

use crate::config::RetentionPolicy;
use crate::logging::{LogLevel, field, log_event};
use anyhow::Result;
use kronika_layout::SegmentId;
use kronika_source_os::statvfs;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

pub(crate) struct RotationState {
    retention: RetentionPolicy,
    current_size_bytes: u64,
    last_degradation_event: Option<Instant>,
    out_dir: PathBuf,
}

impl RotationState {
    #[allow(dead_code, reason = "placeholder for full implementation")]
    pub(crate) fn new(retention: RetentionPolicy, out_dir: PathBuf, initial_size: u64) -> Self {
        Self {
            retention,
            current_size_bytes: initial_size,
            last_degradation_event: None,
            out_dir,
        }
    }

    #[allow(dead_code, reason = "placeholder for full implementation")]
    pub(crate) fn update_size(&mut self, delta: i64) {
        if delta > 0 {
            self.current_size_bytes = self.current_size_bytes.saturating_add(delta.unsigned_abs());
        } else {
            self.current_size_bytes = self.current_size_bytes.saturating_sub(delta.unsigned_abs());
        }
    }

    pub(crate) fn should_rotate(&self) -> bool {
        match self.retention {
            RetentionPolicy::Disabled => false,
            RetentionPolicy::Fixed(budget) => self.current_size_bytes > budget,
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                reason = "used_percent clamped to 100, safe cast to u8"
            )]
            RetentionPolicy::Auto(percent) => {
                if let Some(path_str) = self.out_dir.to_str()
                    && let Some(space) = statvfs(path_str)
                {
                    let total = space.total_bytes;
                    let used = total.saturating_sub(space.free_bytes);
                    if total > 0 {
                        let used_percent = ((used * 100) / total).min(100) as u8;
                        return used_percent >= percent;
                    }
                }
                false
            }
        }
    }

    #[allow(dead_code, reason = "placeholder for full implementation")]
    pub(crate) fn get_current_size(&self) -> u64 {
        self.current_size_bytes
    }

    #[allow(dead_code, reason = "placeholder for full implementation")]
    pub(crate) fn get_budget(&self) -> Option<u64> {
        match self.retention {
            RetentionPolicy::Fixed(budget) => Some(budget),
            _ => None,
        }
    }

    #[allow(
        clippy::missing_const_for_fn,
        reason = "placeholder for full implementation"
    )]
    pub(crate) fn can_emit_degradation(&mut self, now: Instant) -> bool {
        match self.last_degradation_event {
            None => {
                self.last_degradation_event = Some(now);
                true
            }
            Some(last) => {
                let threshold = Duration::from_mins(1);
                if now.duration_since(last) >= threshold {
                    self.last_degradation_event = Some(now);
                    true
                } else {
                    false
                }
            }
        }
    }
}

pub(crate) fn scan_and_count_tree(
    _root: &kronika_layout::DataRoot,
    out_dir: &PathBuf,
) -> Result<u64> {
    let mut total = 0_u64;

    scan_dir(out_dir, &mut total)?;

    Ok(total)
}

fn scan_dir(dir: &std::path::Path, total: &mut u64) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            scan_dir(&path, total)?;
        } else if is_tracked_file(&path) {
            if let Ok(meta) = fs::metadata(&path) {
                *total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(())
}

fn is_tracked_file(path: &std::path::Path) -> bool {
    let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let is_pgm = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("pgm"));
    let is_ovf = path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ovf"));
    is_pgm || is_ovf || file_name == "active.parts" || file_name.starts_with("active.parts.damaged")
}

#[allow(dead_code, reason = "placeholder for full implementation")]
pub(crate) fn log_deletion(
    path: &std::path::Path,
    segment_id: Option<SegmentId>,
    bytes: u64,
    reason: &str,
    current_size: u64,
    threshold: Option<u64>,
) {
    let mut fields = vec![
        field("path", path.display().to_string()),
        field("bytes", bytes),
        field("reason", reason),
        field("current_size", current_size),
    ];
    if let Some(id) = segment_id {
        fields.push(field("segment_id", format!("{id:?}")));
    }
    if let Some(t) = threshold {
        fields.push(field("threshold", t));
    }
    log_event(LogLevel::Info, "retention_deleted", &fields);
}

pub(crate) fn log_degradation(current_size: u64, min_required: u64) {
    log_event(
        LogLevel::Warn,
        "retention_degradation",
        &[
            field("reason", "reached_minimum_liveness"),
            field("current_size", current_size),
            field("min_required", min_required),
            field(
                "message",
                "rotation stopped to preserve active journal and latest segment",
            ),
        ],
    );
}
