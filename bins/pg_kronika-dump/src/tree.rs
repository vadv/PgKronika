use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};

use kronika_layout::{
    DataRoot, EntryFileType, FileIdentity, LayoutLimits, LayoutSnapshot, SegmentArtifacts, UtcDay,
};
use kronika_reader::PgmUnit;

use crate::journal;
use crate::model::{
    QuarantineOutput, TreeDayOutput, TreeJournalOutput, TreeOutput, TreeSegmentOutput,
    TreeTotalsOutput,
};
use crate::pgm::{checked_add, is_dictionary, timestamp_range};
use crate::{DumpError, Options};

pub(crate) fn inspect_path(path: &Path, options: Options) -> Result<TreeOutput, DumpError> {
    debug_assert!(
        !options.rows,
        "tree inspection must reject --rows before traversal"
    );
    let limits = LayoutLimits::default();
    let selection = select_tree(path, limits)?;
    let root = selection.root;
    let snapshot = selection.snapshot;
    let quarantine = root
        .scan_quarantine(limits)
        .map_err(|error| DumpError::input("scan quarantine", error))?
        .into_iter()
        .map(|entry| QuarantineOutput {
            id: entry.file_name().to_owned(),
            reason: entry.reason().code(),
            bytes: entry.identity().file.len,
            file_type: file_type_name(entry.identity().file_type),
        })
        .collect();
    let journal = inspect_active_journal(&root, root.diagnostic_path(), options)?;

    let mut by_day: BTreeMap<UtcDay, Vec<TreeSegmentOutput>> = snapshot
        .days
        .iter()
        .copied()
        .filter(|day| selection.scope.includes(*day))
        .map(|day| (day, Vec::new()))
        .collect();
    let mut segment_count = 0_u64;
    let mut pgm_bytes = 0_u64;
    let mut stored_bytes = 0_u64;
    let mut decoded_bytes = Some(0_u64);

    for segment in snapshot
        .segments
        .into_iter()
        .filter(|segment| selection.scope.includes(segment.address.day))
    {
        let output = inspect_segment(&root, segment)?;
        segment_count = checked_add(segment_count, 1, "tree segment count")?;
        pgm_bytes = checked_add(pgm_bytes, output.pgm_bytes, "tree PGM bytes")?;
        stored_bytes = checked_add(stored_bytes, output.stored_bytes, "tree stored bytes")?;
        decoded_bytes = add_optional(decoded_bytes, output.decoded_bytes, "tree decoded bytes")?;
        by_day.entry(segment.address.day).or_default().push(output);
    }

    let days = by_day
        .into_iter()
        .map(|(day, segments)| TreeDayOutput {
            day: day.to_string(),
            segments,
        })
        .collect();
    Ok(TreeOutput {
        kind: "tree",
        root: root.diagnostic_path().display().to_string(),
        scope: selection.scope.label(),
        journal,
        quarantine,
        days,
        totals: TreeTotalsOutput {
            segments: segment_count,
            pgm_bytes,
            stored_bytes,
            decoded_bytes,
            ratio: None,
        },
    })
}

#[derive(Debug, Clone, Copy)]
enum TreeScope {
    All,
    Year(u16),
    Month { year: u16, month: u8 },
    Day(UtcDay),
}

impl TreeScope {
    fn includes(self, day: UtcDay) -> bool {
        match self {
            Self::All => true,
            Self::Year(year) => day.year == year,
            Self::Month { year, month } => day.year == year && day.month == month,
            Self::Day(expected) => day == expected,
        }
    }

    fn label(self) -> Option<String> {
        match self {
            Self::All => None,
            Self::Year(year) => Some(format!("{year:04}")),
            Self::Month { year, month } => Some(format!("{year:04}/{month:02}")),
            Self::Day(day) => Some(day.to_string()),
        }
    }
}

struct TreeSelection {
    root: DataRoot,
    snapshot: LayoutSnapshot,
    scope: TreeScope,
}

fn select_tree(path: &Path, limits: LayoutLimits) -> Result<TreeSelection, DumpError> {
    if let Some((root_path, scope)) = calendar_scope(path) {
        let root = DataRoot::open(&root_path)
            .map_err(|error| DumpError::input("open data root", error))?;
        let snapshot = root
            .scan(limits)
            .map_err(|error| DumpError::input("scan data root", error))?;
        if snapshot.days.iter().copied().any(|day| scope.includes(day)) {
            return Ok(TreeSelection {
                root,
                snapshot,
                scope,
            });
        }
    }

    let root = DataRoot::open(path).map_err(|error| DumpError::input("open data root", error))?;
    let snapshot = root
        .scan(limits)
        .map_err(|error| DumpError::input("scan data root", error))?;
    Ok(TreeSelection {
        root,
        snapshot,
        scope: TreeScope::All,
    })
}

fn calendar_scope(path: &Path) -> Option<(PathBuf, TreeScope)> {
    day_scope(path)
        .or_else(|| month_scope(path))
        .or_else(|| year_scope(path))
}

fn day_scope(path: &Path) -> Option<(PathBuf, TreeScope)> {
    let leaf = path.file_name()?.to_str()?;
    let day = parse_component(leaf, 2).and_then(|value| u8::try_from(value).ok())?;
    let month_path = path.parent()?;
    let month = parse_component(month_path.file_name()?.to_str()?, 2)
        .and_then(|value| u8::try_from(value).ok())?;
    let year_path = month_path.parent()?;
    let year = parse_component(year_path.file_name()?.to_str()?, 4)?;
    let root = parent_path(year_path)?;
    let day = UtcDay::new(year, month, day).ok()?;
    Some((root, TreeScope::Day(day)))
}

fn month_scope(path: &Path) -> Option<(PathBuf, TreeScope)> {
    let leaf = path.file_name()?.to_str()?;
    let month = parse_component(leaf, 2).and_then(|value| u8::try_from(value).ok())?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let year_path = path.parent()?;
    let year = parse_component(year_path.file_name()?.to_str()?, 4)?;
    Some((parent_path(year_path)?, TreeScope::Month { year, month }))
}

fn year_scope(path: &Path) -> Option<(PathBuf, TreeScope)> {
    let leaf = path.file_name()?.to_str()?;
    let year = parse_component(leaf, 4)?;
    Some((parent_path(path)?, TreeScope::Year(year)))
}

fn parent_path(path: &Path) -> Option<PathBuf> {
    let parent = path.parent()?;
    if parent.as_os_str().is_empty() {
        Some(PathBuf::from("."))
    } else {
        Some(parent.to_path_buf())
    }
}

fn parse_component(value: &str, width: usize) -> Option<u16> {
    if value.len() == width && value.bytes().all(|byte| byte.is_ascii_digit()) {
        value.parse().ok()
    } else {
        None
    }
}

const fn file_type_name(file_type: EntryFileType) -> &'static str {
    match file_type {
        EntryFileType::RegularFile => "regular_file",
        EntryFileType::Directory => "directory",
        EntryFileType::Symlink => "symlink",
        EntryFileType::Other => "other",
    }
}

fn inspect_active_journal(
    root: &DataRoot,
    path: &Path,
    options: Options,
) -> Result<TreeJournalOutput, DumpError> {
    let Some(file) = root
        .open_active_journal()
        .map_err(|error| DumpError::input("open active journal", error))?
    else {
        return Ok(TreeJournalOutput {
            state: "absent",
            segment_id: None,
            frames: 0,
            bytes: 0,
            damage: None,
        });
    };
    let diagnostic_path = path.join(kronika_layout::ACTIVE_JOURNAL_NAME);
    let output = journal::inspect_file(&file, &diagnostic_path, options)?;
    Ok(TreeJournalOutput {
        state: output.header.state,
        segment_id: output.header.segment_id,
        frames: output.recoverable.frames,
        bytes: output.physical_bytes,
        damage: output.damage.map(|damage| damage.kind),
    })
}

fn inspect_segment(
    root: &DataRoot,
    segment: SegmentArtifacts,
) -> Result<TreeSegmentOutput, DumpError> {
    let file = root
        .open_pgm(segment.address)
        .map_err(|error| DumpError::input("open tree PGM", error))?;
    verify_identity(&file, segment.pgm_identity)?;
    let identity_file = file
        .try_clone()
        .map_err(|error| DumpError::input("clone tree PGM descriptor", error))?;
    let unit =
        PgmUnit::open(file).map_err(|error| DumpError::input("open tree PGM catalog", error))?;
    let catalog = unit.catalog();
    let mut sections = 0_u64;
    let mut stored_bytes = 0_u64;
    for entry in catalog.entries.iter().copied() {
        stored_bytes = checked_add(stored_bytes, entry.len, "tree stored bytes")?;
        if !is_dictionary(entry.type_id) {
            sections = checked_add(sections, 1, "tree section count")?;
        }
    }
    let (first_window_us, last_window_us) = timestamp_range(catalog.min_ts, catalog.max_ts);
    verify_identity(&identity_file, segment.pgm_identity)?;

    Ok(TreeSegmentOutput {
        segment_id: segment.address.id.get(),
        pgm_bytes: segment.pgm_bytes,
        ovf: segment.ovf_bytes.is_some(),
        sections,
        stored_bytes,
        decoded_bytes: None,
        ratio: None,
        windows: (catalog.window_count != 0).then_some(u64::from(catalog.window_count)),
        first_window_us,
        last_window_us,
    })
}

fn verify_identity(file: &File, expected: FileIdentity) -> Result<(), DumpError> {
    let observed =
        FileIdentity::from_file(file).map_err(|error| DumpError::input("stat tree PGM", error))?;
    if observed == expected {
        Ok(())
    } else {
        Err(DumpError::message(
            "tree PGM changed between discovery and inspection",
        ))
    }
}

fn add_optional(
    left: Option<u64>,
    right: Option<u64>,
    quantity: &'static str,
) -> Result<Option<u64>, DumpError> {
    match (left, right) {
        (Some(left), Some(right)) => checked_add(left, right, quantity).map(Some),
        _ => Ok(None),
    }
}
