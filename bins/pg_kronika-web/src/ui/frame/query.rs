use std::cmp::Ordering;
use std::fmt::Write as _;

use super::FrameRequest;
use super::cursor::SortKey;
use super::dto::FrameValue;
use super::projection::{ProjectedRow, ProjectionError, cursor_for};
use crate::api_error::{
    ApiError, ExpectedValue, InvalidParameterLocation, LimitResource, count_u64,
};
use crate::ui::catalog::{ValueType, ViewSpec};

pub(crate) const MAX_FILTER_TERMS: usize = 16;

#[derive(Debug, Clone)]
pub(crate) struct FrameFilter {
    terms: Vec<FilterTerm>,
    canonical: String,
}

#[derive(Debug, Clone)]
enum FilterTerm {
    Any(Glob),
    Field {
        column: &'static str,
        value: TypedFilter,
    },
}

#[derive(Debug, Clone)]
enum TypedFilter {
    Text(Glob),
    Signed(i64),
    Unsigned(u64),
    Float(u64),
    Boolean(bool),
    Timestamp(i64),
}

#[derive(Debug, Clone)]
struct Glob {
    atoms: Vec<GlobAtom>,
}

#[derive(Debug, Clone, Copy)]
enum GlobAtom {
    Literal(char),
    Star,
    One,
}

impl FrameFilter {
    pub(crate) fn parse(raw: &str, view: &ViewSpec) -> Result<Self, ApiError> {
        let raw_terms = split_terms(raw)?;
        if raw_terms.is_empty() {
            return Err(invalid_filter());
        }
        if raw_terms.len() > MAX_FILTER_TERMS {
            return Err(ApiError::query_shape_limit_exceeded(
                LimitResource::Cells,
                count_u64(MAX_FILTER_TERMS),
                Some(count_u64(raw_terms.len())),
            ));
        }
        let mut terms = Vec::with_capacity(raw_terms.len());
        for raw_term in raw_terms {
            let (field, raw_glob) = split_field(&raw_term)?;
            let glob = Glob::parse(&raw_glob)?;
            if let Some(field) = field {
                let column = view
                    .columns
                    .iter()
                    .find(|column| column.code == field.as_str() && !column.lazy)
                    .ok_or_else(invalid_filter)?;
                terms.push(FilterTerm::Field {
                    column: column.code,
                    value: TypedFilter::parse(column.value_type, glob)?,
                });
            } else {
                terms.push(FilterTerm::Any(glob));
            }
        }
        let mut canonical = String::new();
        for term in &terms {
            term.write_canonical(&mut canonical);
        }
        Ok(Self { terms, canonical })
    }

    pub(crate) fn canonical(&self) -> &str {
        &self.canonical
    }

    pub(crate) fn field_columns(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.terms.iter().filter_map(|term| match term {
            FilterTerm::Field { column, .. } => Some(*column),
            FilterTerm::Any(_) => None,
        })
    }

    fn matches(&self, row: &ProjectedRow) -> bool {
        self.terms.iter().all(|term| term.matches(row))
    }
}

impl FilterTerm {
    fn matches(&self, row: &ProjectedRow) -> bool {
        match self {
            Self::Any(glob) => {
                glob.matches(&row.label)
                    || row
                        .values
                        .iter()
                        .any(|(_column, value)| glob.matches_frame(value))
            }
            Self::Field { column, value } => row
                .values
                .iter()
                .find(|(candidate, _value)| candidate == column)
                .is_some_and(|(_column, observed)| value.matches(observed)),
        }
    }

    fn write_canonical(&self, output: &mut String) {
        match self {
            Self::Any(glob) => {
                output.push_str("a:");
                glob.write_canonical(output);
            }
            Self::Field { column, value } => {
                write!(output, "f{}:{column}:", column.len())
                    .expect("writing to String cannot fail");
                value.write_canonical(output);
            }
        }
        output.push(';');
    }
}

impl TypedFilter {
    fn parse(value_type: ValueType, glob: Glob) -> Result<Self, ApiError> {
        if value_type == ValueType::Text {
            return Ok(Self::Text(glob));
        }
        let literal = glob.literal().ok_or_else(invalid_filter)?;
        match value_type {
            ValueType::I64 => literal
                .parse()
                .map(Self::Signed)
                .map_err(|_error| invalid_filter()),
            ValueType::U64 => literal
                .parse()
                .map(Self::Unsigned)
                .map_err(|_error| invalid_filter()),
            ValueType::F64 => literal
                .parse::<f64>()
                .ok()
                .filter(|value| value.is_finite())
                .map(|value| Self::Float(value.to_bits()))
                .ok_or_else(invalid_filter),
            ValueType::Bool => match literal.as_str() {
                "true" => Ok(Self::Boolean(true)),
                "false" => Ok(Self::Boolean(false)),
                _ => Err(invalid_filter()),
            },
            ValueType::Timestamp => literal
                .parse()
                .map(Self::Timestamp)
                .map_err(|_error| invalid_filter()),
            ValueType::Text => unreachable!("text returned above"),
        }
    }

    fn matches(&self, observed: &FrameValue) -> bool {
        match (self, observed) {
            (Self::Text(glob), FrameValue::String(value)) => glob.matches(value),
            (Self::Signed(expected) | Self::Timestamp(expected), FrameValue::Number(value)) => {
                value.to_string().parse::<i64>() == Ok(*expected)
            }
            (Self::Signed(expected) | Self::Timestamp(expected), FrameValue::String(value)) => {
                value.parse::<i64>() == Ok(*expected)
            }
            (Self::Unsigned(expected), FrameValue::Number(value)) => {
                value.to_string().parse::<u64>() == Ok(*expected)
            }
            (Self::Unsigned(expected), FrameValue::String(value)) => {
                value.parse::<u64>() == Ok(*expected)
            }
            (Self::Float(expected), FrameValue::Number(value)) => value.to_bits() == *expected,
            (Self::Boolean(expected), FrameValue::Boolean(value)) => value == expected,
            _ => false,
        }
    }

    fn write_canonical(&self, output: &mut String) {
        match self {
            Self::Text(glob) => {
                output.push_str("t:");
                glob.write_canonical(output);
            }
            Self::Signed(value) => {
                write!(output, "i:{value}").expect("writing to String cannot fail");
            }
            Self::Unsigned(value) => {
                write!(output, "u:{value}").expect("writing to String cannot fail");
            }
            Self::Float(bits) => {
                write!(output, "f:{bits:016x}").expect("writing to String cannot fail");
            }
            Self::Boolean(value) => {
                write!(output, "b:{}", u8::from(*value)).expect("writing to String cannot fail");
            }
            Self::Timestamp(value) => {
                write!(output, "z:{value}").expect("writing to String cannot fail");
            }
        }
    }
}

impl Glob {
    fn parse(raw: &str) -> Result<Self, ApiError> {
        if raw.is_empty() {
            return Err(invalid_filter());
        }
        let mut atoms = Vec::new();
        if let Some(quoted) = raw
            .strip_prefix('"')
            .and_then(|value| value.strip_suffix('"'))
        {
            let mut escaped = false;
            for character in quoted.chars() {
                if escaped {
                    if !matches!(character, '"' | '\\' | '*' | '?') {
                        return Err(invalid_filter());
                    }
                    atoms.push(GlobAtom::Literal(character));
                    escaped = false;
                } else {
                    match character {
                        '\\' => escaped = true,
                        '"' => return Err(invalid_filter()),
                        '*' => atoms.push(GlobAtom::Star),
                        '?' => atoms.push(GlobAtom::One),
                        character => push_folded(&mut atoms, character),
                    }
                }
            }
            if escaped || atoms.is_empty() {
                return Err(invalid_filter());
            }
        } else {
            for character in raw.chars() {
                match character {
                    '"' | '\\' | ' ' => return Err(invalid_filter()),
                    '*' => atoms.push(GlobAtom::Star),
                    '?' => atoms.push(GlobAtom::One),
                    character => push_folded(&mut atoms, character),
                }
            }
        }
        Ok(Self { atoms })
    }

    fn literal(&self) -> Option<String> {
        self.atoms
            .iter()
            .map(|atom| match atom {
                GlobAtom::Literal(character) => Some(*character),
                GlobAtom::Star | GlobAtom::One => None,
            })
            .collect()
    }

    fn matches_frame(&self, value: &FrameValue) -> bool {
        match value {
            FrameValue::Null => false,
            FrameValue::Number(value) => self.literal().is_some_and(|expected| {
                expected
                    .parse::<f64>()
                    .is_ok_and(|expected| expected.to_bits() == value.to_bits())
            }),
            FrameValue::Boolean(value) => self
                .literal()
                .is_some_and(|expected| expected == value.to_string()),
            FrameValue::String(value) => self.matches(value),
        }
    }

    fn matches(&self, value: &str) -> bool {
        let value = value.to_lowercase().chars().collect::<Vec<_>>();
        let mut previous = vec![false; value.len() + 1];
        previous[0] = true;
        for atom in &self.atoms {
            let mut current = vec![false; value.len() + 1];
            match atom {
                GlobAtom::Star => {
                    current[0] = previous[0];
                    for index in 1..=value.len() {
                        current[index] = previous[index] || current[index - 1];
                    }
                }
                GlobAtom::One => {
                    current[1..].copy_from_slice(&previous[..value.len()]);
                }
                GlobAtom::Literal(expected) => {
                    for index in 1..=value.len() {
                        current[index] = previous[index - 1] && value[index - 1] == *expected;
                    }
                }
            }
            previous = current;
        }
        previous[value.len()]
    }

    fn write_canonical(&self, output: &mut String) {
        for atom in &self.atoms {
            match atom {
                GlobAtom::Literal(character) => {
                    write!(output, "l{:x},", *character as u32)
                        .expect("writing to String cannot fail");
                }
                GlobAtom::Star => output.push_str("s,"),
                GlobAtom::One => output.push_str("o,"),
            }
        }
    }
}

fn push_folded(atoms: &mut Vec<GlobAtom>, character: char) {
    atoms.extend(character.to_lowercase().map(GlobAtom::Literal));
}

fn split_terms(raw: &str) -> Result<Vec<String>, ApiError> {
    let mut terms = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut escaped = false;
    for character in raw.chars() {
        if character == ' ' && !quoted {
            if !current.is_empty() {
                terms.push(std::mem::take(&mut current));
            }
            continue;
        }
        current.push(character);
        if escaped {
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        }
    }
    if quoted || escaped {
        return Err(invalid_filter());
    }
    if !current.is_empty() {
        terms.push(current);
    }
    Ok(terms)
}

fn split_field(raw: &str) -> Result<(Option<String>, String), ApiError> {
    let mut field = String::new();
    let mut glob = String::new();
    let mut found = false;
    let mut quoted = false;
    let mut escaped = false;
    for character in raw.chars() {
        if escaped {
            escaped = false;
        } else if quoted && character == '\\' {
            escaped = true;
        } else if character == '"' {
            quoted = !quoted;
        } else if character == '=' && !quoted {
            if found {
                return Err(invalid_filter());
            }
            found = true;
            continue;
        }
        if found {
            glob.push(character);
        } else {
            field.push(character);
        }
    }
    if found {
        if field.is_empty() || glob.is_empty() {
            return Err(invalid_filter());
        }
        Ok((Some(field), glob))
    } else {
        Ok((None, field))
    }
}

fn invalid_filter() -> ApiError {
    ApiError::invalid_query_parameter(
        InvalidParameterLocation::Parameter(crate::api_error::QueryParameter::Q),
        ExpectedValue::FilterExpression,
    )
}

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
            .is_none_or(|database| row.database == Some(database.oid))
            && request
                .filter
                .as_ref()
                .is_none_or(|filter| filter.matches(row))
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
