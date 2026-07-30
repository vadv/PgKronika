use std::cmp::Ordering;

use super::FrameRequest;
use super::cursor::SortKey;
use super::dto::FrameValue;
use super::projection::{ProjectedRow, ProjectionError, cursor_for};

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
        let position = rows
            .iter()
            .position(|row| row.entity == cursor.entity())
            .filter(|position| {
                value_sort_key(cell(&rows[*position], request.sort)) == *cursor.sort_key()
            })
            .ok_or(ProjectionError::Cursor)?;
        rows.drain(..=position);
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
    let order = compare_values(left_value, right_value);
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

fn compare_values(left: &FrameValue, right: &FrameValue) -> Ordering {
    match (left, right) {
        (FrameValue::Null, FrameValue::Null) => Ordering::Equal,
        (FrameValue::Null, _) => Ordering::Greater,
        (_, FrameValue::Null) => Ordering::Less,
        (FrameValue::Number(left), FrameValue::Number(right)) => left.total_cmp(right),
        (FrameValue::Boolean(left), FrameValue::Boolean(right)) => left.cmp(right),
        (FrameValue::String(left), FrameValue::String(right)) => left.cmp(right),
        (left, right) => frame_value_tag(left).cmp(&frame_value_tag(right)),
    }
}

const fn frame_value_tag(value: &FrameValue) -> u8 {
    match value {
        FrameValue::Null => 0,
        FrameValue::Number(_) => 1,
        FrameValue::Boolean(_) => 2,
        FrameValue::String(_) => 3,
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
