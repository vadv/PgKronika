//! Bounded storage-root accounting and capacity forecast.

use std::fmt;
use std::fs;
use std::path::Path;

use kronika_layout::{
    DataRoot, EntryFileType, FileIdentity, LayoutError, LayoutLimits, OVERVIEW_OWNER_LOCK_NAME,
    PRODUCER_STATUS_NAME, PRODUCER_STATUS_TEMP_NAME, ProducerStatus, RetentionStatus,
    WRITER_OWNER_LOCK_NAME,
};
use serde::Serialize;
use utoipa::ToSchema;

const DAY_US: i64 = 86_400_000_000;

#[derive(Debug, Clone, Copy)]
pub(crate) struct StorageLimits {
    pub(crate) layout: LayoutLimits,
    pub(crate) forecast_window_us: i64,
}

impl Default for StorageLimits {
    fn default() -> Self {
        Self {
            layout: LayoutLimits::default(),
            forecast_window_us: DAY_US,
        }
    }
}

#[derive(Debug, Serialize, ToSchema)]
pub(crate) struct StorageResponse {
    used_bytes: UsedBytesDto,
    filesystem: FilesystemDto,
    retention: RetentionDto,
    forecast: ForecastDto,
    integrity: IntegrityDto,
    quality: QualityDto,
}

#[derive(Debug, Serialize, ToSchema)]
struct UsedBytesDto {
    pgm: u64,
    ovf: u64,
    journal: u64,
    quarantine: u64,
    other: u64,
}

impl UsedBytesDto {
    const fn total(&self) -> u64 {
        self.pgm
            .saturating_add(self.ovf)
            .saturating_add(self.journal)
            .saturating_add(self.quarantine)
            .saturating_add(self.other)
    }
}

#[derive(Debug, Serialize, ToSchema)]
struct FilesystemDto {
    total_bytes: u64,
    available_bytes: u64,
    used_fraction: f64,
}

#[derive(Debug, Serialize, ToSchema)]
struct RetentionDto {
    #[schema(required = true)]
    mode: Option<&'static str>,
    #[schema(required = true)]
    configured_limit: Option<u64>,
    #[schema(required = true)]
    effective_limit_bytes: Option<u64>,
    status: &'static str,
    #[schema(required = true)]
    reason: Option<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
struct ForecastDto {
    #[schema(required = true)]
    write_rate_bytes_per_day: Option<u64>,
    window_us: String,
    #[schema(required = true)]
    full_in_days: Option<f64>,
    #[schema(required = true)]
    full_in_days_reason: Option<&'static str>,
}

#[derive(Debug, Serialize, ToSchema)]
struct IntegrityDto {
    readable_segments: usize,
    orphan_overviews: usize,
    quarantined_entries: usize,
}

#[derive(Debug, Serialize, ToSchema)]
struct QualityDto {
    status: &'static str,
    gated: Vec<&'static str>,
}

#[derive(Debug)]
pub(crate) enum StorageError {
    Layout(LayoutError),
    Io(std::io::Error),
    OrphanOverviewDisappeared,
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Layout(error) => write!(f, "storage layout read failed: {error}"),
            Self::Io(error) => write!(f, "storage metadata read failed: {error}"),
            Self::OrphanOverviewDisappeared => {
                f.write_str("orphan overview disappeared during bounded inventory")
            }
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Layout(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::OrphanOverviewDisappeared => None,
        }
    }
}

impl From<LayoutError> for StorageError {
    fn from(error: LayoutError) -> Self {
        Self::Layout(error)
    }
}

impl From<std::io::Error> for StorageError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the bounded inventory is assembled once before the dependent retention and forecast DTOs"
)]
pub(crate) fn build_storage(
    path: &Path,
    producer_status: Option<&ProducerStatus>,
    limits: StorageLimits,
) -> Result<StorageResponse, StorageError> {
    let root = DataRoot::open(path)?;
    let inventory = root.scan(limits.layout)?;
    let quarantine = root.scan_quarantine(limits.layout)?;
    let filesystem = root.filesystem_usage()?;

    let pgm = inventory
        .segments
        .iter()
        .fold(0_u64, |sum, segment| sum.saturating_add(segment.pgm_bytes));
    let mut ovf = inventory.segments.iter().fold(0_u64, |sum, segment| {
        sum.saturating_add(segment.ovf_bytes.unwrap_or(0))
    });
    for address in &inventory.orphan_overviews {
        let file = root
            .open_ovf(*address)?
            .ok_or(StorageError::OrphanOverviewDisappeared)?;
        ovf = ovf.saturating_add(FileIdentity::from_file(&file)?.len);
    }
    let journal = root
        .open_active_journal()?
        .map(|file| FileIdentity::from_file(&file).map(|identity| identity.len))
        .transpose()?
        .unwrap_or(0);
    let quarantine_bytes = quarantine.iter().fold(0_u64, |sum, entry| {
        sum.saturating_add(entry.identity().file.len)
    });
    let foreign_bytes = inventory
        .foreign_entries
        .iter()
        .filter_map(|entry| {
            let path = entry.diagnostic().path;
            (path.file_type == EntryFileType::RegularFile).then_some(path.file.len)
        })
        .fold(0_u64, u64::saturating_add);
    let temporary_bytes = inventory
        .temporaries
        .iter()
        .fold(0_u64, |sum, entry| sum.saturating_add(entry.identity.len));
    let pending_bytes = inventory
        .pending_root_entries
        .iter()
        .fold(0_u64, |sum, entry| {
            sum.saturating_add(entry.identity().file.len)
        });
    let control_bytes = [
        PRODUCER_STATUS_NAME,
        PRODUCER_STATUS_TEMP_NAME,
        WRITER_OWNER_LOCK_NAME,
        OVERVIEW_OWNER_LOCK_NAME,
    ]
    .into_iter()
    .map(|name| regular_file_len(path, name))
    .collect::<Result<Vec<_>, _>>()?
    .into_iter()
    .fold(0_u64, u64::saturating_add);
    let used_bytes = UsedBytesDto {
        pgm,
        ovf,
        journal,
        quarantine: quarantine_bytes,
        other: foreign_bytes
            .saturating_add(temporary_bytes)
            .saturating_add(pending_bytes)
            .saturating_add(control_bytes),
    };

    let available_bytes = filesystem.total_bytes.saturating_sub(filesystem.used_bytes);
    let used_fraction = fraction(filesystem.used_bytes, filesystem.total_bytes);
    let retention = retention_dto(producer_status, filesystem.total_bytes);
    let write_rate = sealed_write_rate(&inventory, limits.forecast_window_us);
    let (full_in_days, full_in_days_reason) = full_forecast(
        write_rate,
        available_bytes,
        used_bytes.total(),
        filesystem.used_bytes,
        &retention,
    );

    Ok(StorageResponse {
        used_bytes,
        filesystem: FilesystemDto {
            total_bytes: filesystem.total_bytes,
            available_bytes,
            used_fraction,
        },
        retention,
        forecast: ForecastDto {
            write_rate_bytes_per_day: write_rate,
            window_us: limits.forecast_window_us.to_string(),
            full_in_days,
            full_in_days_reason,
        },
        integrity: IntegrityDto {
            readable_segments: inventory.segments.len(),
            orphan_overviews: inventory.orphan_overviews.len(),
            quarantined_entries: quarantine.len(),
        },
        quality: QualityDto {
            status: "complete",
            gated: Vec::new(),
        },
    })
}

fn regular_file_len(root: &Path, name: &str) -> Result<u64, std::io::Error> {
    match fs::symlink_metadata(root.join(name)) {
        Ok(metadata) if metadata.file_type().is_file() => Ok(metadata.len()),
        Ok(_) => Ok(0),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error),
    }
}

fn retention_dto(status: Option<&ProducerStatus>, filesystem_bytes: u64) -> RetentionDto {
    let Some(status) = status else {
        return RetentionDto {
            mode: None,
            configured_limit: None,
            effective_limit_bytes: None,
            status: "unknown",
            reason: Some("producer_status_unavailable"),
        };
    };
    match status.retention {
        Some(RetentionStatus::Fixed { target_bytes }) => RetentionDto {
            mode: Some("fixed_bytes"),
            configured_limit: Some(target_bytes),
            effective_limit_bytes: Some(target_bytes),
            status: "known",
            reason: None,
        },
        Some(RetentionStatus::Auto { target_percent }) => RetentionDto {
            mode: Some("auto_percent"),
            configured_limit: Some(u64::from(target_percent)),
            effective_limit_bytes: Some(percent_of(filesystem_bytes, target_percent)),
            status: "known",
            reason: None,
        },
        None => RetentionDto {
            mode: Some("disabled"),
            configured_limit: None,
            effective_limit_bytes: None,
            status: "known",
            reason: None,
        },
    }
}

fn percent_of(total: u64, percent: u8) -> u64 {
    u64::try_from(u128::from(total) * u128::from(percent) / 100).unwrap_or(u64::MAX)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "the API exposes an informational fraction while exact byte counters remain u64"
)]
fn fraction(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

fn sealed_write_rate(inventory: &kronika_layout::LayoutSnapshot, window_us: i64) -> Option<u64> {
    if window_us <= 0 {
        return None;
    }
    let mut points = inventory
        .segments
        .iter()
        .filter_map(|segment| {
            Some((
                identity_mtime_us(segment.pgm_identity)?,
                segment
                    .pgm_bytes
                    .saturating_add(segment.ovf_bytes.unwrap_or(0)),
            ))
        })
        .collect::<Vec<_>>();
    points.sort_unstable_by_key(|point| point.0);
    let newest = points.last()?.0;
    let oldest_allowed = newest.checked_sub(window_us)?;
    let mut retained = points
        .into_iter()
        .filter(|(timestamp, _bytes)| *timestamp >= oldest_allowed);
    let (first_at, _first_bytes) = retained.next()?;
    let mut last_at = first_at;
    let mut growth = 0_u64;
    for (timestamp, bytes) in retained {
        last_at = timestamp;
        growth = growth.saturating_add(bytes);
    }
    let elapsed = last_at.checked_sub(first_at)?;
    if elapsed <= 0 {
        return None;
    }
    let daily = u128::from(growth).saturating_mul(u128::try_from(DAY_US).ok()?)
        / u128::try_from(elapsed).ok()?;
    u64::try_from(daily).ok()
}

fn identity_mtime_us(identity: FileIdentity) -> Option<i64> {
    identity
        .mtime_seconds
        .checked_mul(1_000_000)?
        .checked_add(identity.mtime_nanoseconds / 1_000)
}

#[allow(
    clippy::cast_precision_loss,
    reason = "capacity days are informational floats derived from exact u64 byte counters"
)]
fn full_forecast(
    write_rate: Option<u64>,
    filesystem_available: u64,
    tree_used: u64,
    filesystem_used: u64,
    retention: &RetentionDto,
) -> (Option<f64>, Option<&'static str>) {
    let Some(rate) = write_rate else {
        return (None, Some("insufficient_history"));
    };
    if rate == 0 {
        return (None, Some("non_positive_rate"));
    }
    let filesystem_days = filesystem_available as f64 / rate as f64;
    let retention_headroom = match retention.mode {
        Some("fixed_bytes") => retention
            .effective_limit_bytes
            .map(|limit| limit.saturating_sub(tree_used)),
        Some("auto_percent") => retention
            .effective_limit_bytes
            .map(|limit| limit.saturating_sub(filesystem_used)),
        _ => None,
    };
    if retention_headroom.is_some_and(|bytes| bytes as f64 / rate as f64 <= filesystem_days) {
        (None, Some("retention_precedes_exhaustion"))
    } else {
        (Some(filesystem_days), None)
    }
}

#[cfg(test)]
mod tests {
    use kronika_layout::{
        FileIdentity, LayoutSnapshot, QuarantineDirectoryState, SegmentArtifacts,
    };

    use super::{StorageLimits, full_forecast, retention_dto, sealed_write_rate};

    fn identity(len: u64, mtime_seconds: i64) -> FileIdentity {
        FileIdentity {
            device: 1,
            inode: len,
            len,
            mtime_seconds,
            mtime_nanoseconds: 0,
            ctime_seconds: mtime_seconds,
            ctime_nanoseconds: 0,
        }
    }

    #[test]
    fn sealed_rate_requires_two_times_and_counts_only_later_growth() {
        let segments = [(1_000, 100_u64, 1_i64), (2_000, 200_u64, 2_i64)]
            .into_iter()
            .map(|(raw_id, bytes, mtime)| SegmentArtifacts {
                address: crate::test_layout::address(raw_id),
                pgm_identity: identity(bytes, mtime),
                pgm_bytes: bytes,
                ovf_bytes: None,
            })
            .collect();
        let inventory = LayoutSnapshot {
            days: Vec::new(),
            segments,
            orphan_overviews: Vec::new(),
            temporaries: Vec::new(),
            foreign_entries: Vec::new(),
            pending_root_entries: Vec::new(),
            quarantine_directory: QuarantineDirectoryState::Absent,
            visited_entries: 0,
            metadata_bytes: 0,
        };

        assert_eq!(
            sealed_write_rate(&inventory, StorageLimits::default().forecast_window_us),
            Some(17_280_000)
        );
    }

    #[test]
    fn configured_retention_suppresses_a_later_filesystem_exhaustion_forecast() {
        let status = kronika_layout::ProducerStatus::running(
            42,
            1,
            2,
            Some(kronika_layout::RetentionStatus::fixed(200)),
        );
        let retention = retention_dto(Some(&status), 10_000);

        assert_eq!(
            full_forecast(Some(100), 1_000, 150, 9_000, &retention),
            (None, Some("retention_precedes_exhaustion"))
        );
    }
}
