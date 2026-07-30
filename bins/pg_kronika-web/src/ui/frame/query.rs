use std::cmp::Ordering;

use super::FrameRequest;
use super::cursor::SortKey;
use super::dto::FrameValue;
use super::projection::{ProjectedRow, ProjectionError, compare_frame_values, cursor_for};

pub(crate) struct PagedRows {
    pub rows: Vec<ProjectedRow>,
    pub matched: usize,
    pub next: Option<String>,
}

pub(crate) fn filter_sort_page(
    request: &FrameRequest,
    snapshot_ts_us: i64,
    mut rows: Vec<ProjectedRow>,
) -> Result<PagedRows, ProjectionError> {
    rows.retain(|row| {
        request
            .database
            .as_ref()
            .is_none_or(|database| row.database.as_ref() == Some(database))
            && request
                .filter
                .as_ref()
                .is_none_or(|filter| row.searchable.contains(filter))
    });
    rows.sort_by(|left, right| compare_rows(request, left, right));
    let matched = rows.len();

    if let Some(cursor) = &request.cursor {
        rows.retain(|row| compare_cursor(request, row, cursor.sort_key(), cursor.entity()).is_gt());
    }
    let has_more = rows.len() > request.limit;
    rows.truncate(request.limit);
    let next = if has_more {
        rows.last()
            .map(|row| cursor_for(request, snapshot_ts_us, row))
            .transpose()?
    } else {
        None
    };
    Ok(PagedRows {
        rows,
        matched,
        next,
    })
}

fn compare_rows(request: &FrameRequest, left: &ProjectedRow, right: &ProjectedRow) -> Ordering {
    let left_value = cell(left, request.sort);
    let right_value = cell(right, request.sort);
    let order = compare_frame_values(left_value, right_value);
    let order = if request.descending
        && !matches!(left_value, FrameValue::Null)
        && !matches!(right_value, FrameValue::Null)
    {
        order.reverse()
    } else {
        order
    };
    order.then_with(|| left.entity.cmp(&right.entity))
}

fn compare_cursor(
    request: &FrameRequest,
    row: &ProjectedRow,
    cursor_key: &SortKey,
    cursor_entity: &[u8],
) -> Ordering {
    let row_key = value_sort_key(cell(row, request.sort));
    let order = compare_sort_keys(&row_key, cursor_key);
    let order = if request.descending
        && !matches!(row_key, SortKey::Null)
        && !matches!(cursor_key, SortKey::Null)
    {
        order.reverse()
    } else {
        order
    };
    order.then_with(|| row.entity.as_slice().cmp(cursor_entity))
}

fn cell<'a>(row: &'a ProjectedRow, code: &str) -> &'a FrameValue {
    row.values
        .iter()
        .find(|(column, _)| *column == code)
        .map_or(&FrameValue::Null, |(_, value)| value)
}

pub(crate) fn value_sort_key(value: &FrameValue) -> SortKey {
    match value {
        FrameValue::Null => SortKey::Null,
        FrameValue::Number(value) => SortKey::Float(*value),
        FrameValue::Boolean(value) => SortKey::Boolean(*value),
        FrameValue::String(value) => SortKey::text_prefix(value),
    }
}

fn compare_sort_keys(left: &SortKey, right: &SortKey) -> Ordering {
    match (left, right) {
        (SortKey::Null, SortKey::Null) => Ordering::Equal,
        (SortKey::Null, _) => Ordering::Greater,
        (_, SortKey::Null) => Ordering::Less,
        (SortKey::Signed(left), SortKey::Signed(right))
        | (SortKey::Timestamp(left), SortKey::Timestamp(right)) => left.cmp(right),
        (SortKey::Unsigned(left), SortKey::Unsigned(right)) => left.cmp(right),
        (SortKey::Float(left), SortKey::Float(right)) => left.total_cmp(right),
        (SortKey::Boolean(left), SortKey::Boolean(right)) => left.cmp(right),
        (SortKey::TextPrefix(left), SortKey::TextPrefix(right)) => left.cmp(right),
        (left, right) => left.tag().cmp(&right.tag()),
    }
}

impl SortKey {
    pub(crate) const fn tag(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Signed(_) => 1,
            Self::Unsigned(_) => 2,
            Self::Float(_) => 3,
            Self::Boolean(_) => 4,
            Self::Timestamp(_) => 5,
            Self::TextPrefix(_) => 6,
        }
    }
}
