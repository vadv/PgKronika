use std::fs::File;
use std::path::Path;

use kronika_format::{Entry, ReadAt, crc32c};
use kronika_layout::FileIdentity;
use kronika_reader::{Dictionary, PgmUnit, Resolved};
use kronika_registry::{
    Bytes, Cell, CodecError, ColumnType, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID,
    MAX_DECODED_SECTION_BYTES, VerifiedSection, decode_rows, parquet_decode_profile,
    plain_parquet_decode_profile, registry, section_name,
};
use serde_json::{Map, Number, Value};

use crate::model::{
    DictionaryOutput, PgmOutput, SectionOutput, TotalsOutput, UnitStats, WindowsOutput,
};
use crate::{DumpError, Options};

pub(crate) fn inspect_file(
    file: File,
    path: &Path,
    options: Options,
) -> Result<PgmOutput, DumpError> {
    let identity =
        FileIdentity::from_file(&file).map_err(|error| DumpError::input("stat PGM", error))?;
    let identity_file = file
        .try_clone()
        .map_err(|error| DumpError::input("clone PGM descriptor", error))?;
    let unit = PgmUnit::open(file).map_err(|error| DumpError::input("open PGM catalog", error))?;
    let file_bytes = unit.source_file_len();
    let stats = inspect_unit(&unit, file_bytes, options)?;
    verify_identity(&identity_file, identity)?;
    Ok(PgmOutput {
        kind: "pgm",
        path: path.display().to_string(),
        segment_id: segment_id_from_path(path),
        file_bytes,
        windows: stats.windows,
        dictionary: stats.dictionary,
        sections: stats.sections,
        totals: stats.totals,
    })
}

pub(crate) fn inspect_unit<R: ReadAt>(
    unit: &PgmUnit<R>,
    file_bytes: u64,
    options: Options,
) -> Result<UnitStats, DumpError> {
    let dictionary_limited = options.rows && dictionary_exceeds_decode_limit(unit)?;
    let dictionary = if options.rows && !dictionary_limited {
        Some(
            unit.dictionary()
                .map_err(|error| DumpError::input("decode PGM dictionary", error))?,
        )
    } else {
        None
    };
    let mut sections = Vec::new();
    let mut dictionary_entries = 0_u64;
    let mut dictionary_stored = 0_u64;
    let mut dictionary_decoded = Some(0_u64);
    let mut dictionary_skipped = None;
    let mut total_stored = 0_u64;
    let mut total_decoded = Some(0_u64);

    for (ordinal, entry) in unit.catalog().entries.iter().copied().enumerate() {
        total_stored = checked_add(total_stored, entry.len, "stored section bytes")?;
        let ordinal = u32::try_from(ordinal)
            .map_err(|_error| DumpError::message("catalog ordinal does not fit u32"))?;
        let section = unit
            .read_overview_section(ordinal)
            .map_err(|error| DumpError::input("read PGM section", error))?;
        let profile = section_profile(entry, section.body())?;
        let decoded_bytes = profile
            .map(u64::try_from)
            .transpose()
            .map_err(|_error| DumpError::message("decoded section size does not fit u64"))?;
        total_decoded = add_optional(total_decoded, decoded_bytes, "decoded section bytes")?;

        if is_dictionary(entry.type_id) {
            dictionary_entries =
                checked_add(dictionary_entries, u64::from(entry.rows), "dictionary rows")?;
            dictionary_stored =
                checked_add(dictionary_stored, entry.len, "stored dictionary bytes")?;
            dictionary_decoded = add_optional(
                dictionary_decoded,
                decoded_bytes,
                "decoded dictionary bytes",
            )?;
            if decoded_bytes.is_none() {
                dictionary_skipped = Some("limit");
            }
            continue;
        }

        let (rows_data, truncated, rows_skipped) = inspect_rows(
            entry,
            section.body(),
            decoded_bytes,
            dictionary_limited,
            dictionary.as_ref(),
            options,
        )?;

        sections.push(SectionOutput {
            type_id: type_id_label(entry.type_id),
            type_name: section_name(entry.type_id),
            rows: u64::from(entry.rows),
            stored_bytes: entry.len,
            decoded_bytes,
            decode_skipped: decoded_bytes.is_none().then_some("limit"),
            ratio: ratio(decoded_bytes, entry.len),
            share_of_file: share(entry.len, file_bytes),
            rows_data,
            truncated,
            rows_skipped,
        });
    }

    let (first_us, last_us) = timestamp_range(unit.catalog().min_ts, unit.catalog().max_ts);
    Ok(UnitStats {
        windows: WindowsOutput {
            count: (unit.catalog().window_count != 0)
                .then_some(u64::from(unit.catalog().window_count)),
            first_us,
            last_us,
        },
        dictionary: DictionaryOutput {
            entries: dictionary_entries,
            stored_bytes: dictionary_stored,
            decoded_bytes: dictionary_decoded,
            decode_skipped: dictionary_skipped,
            share_of_file: share(dictionary_stored, file_bytes),
        },
        sections,
        totals: TotalsOutput {
            stored_bytes: total_stored,
            decoded_bytes: total_decoded,
            ratio: ratio(total_decoded, total_stored),
        },
    })
}

type RowsOutput = (
    Option<Vec<Map<String, Value>>>,
    Option<bool>,
    Option<&'static str>,
);

fn inspect_rows(
    entry: Entry,
    body: &[u8],
    decoded_bytes: Option<u64>,
    dictionary_limited: bool,
    dictionary: Option<&Dictionary>,
    options: Options,
) -> Result<RowsOutput, DumpError> {
    if !options.rows {
        return Ok((None, None, None));
    }
    if section_name(entry.type_id).is_none() {
        return Ok((None, None, Some("unknown_type")));
    }
    if decoded_bytes.is_none() {
        return Ok((None, None, Some("limit")));
    }
    if dictionary_limited && section_uses_dictionary(entry.type_id) {
        return Ok((None, None, Some("dictionary_limit")));
    }

    let verified = VerifiedSection::verify(Bytes::copy_from_slice(body), entry.crc32c, crc32c)
        .map_err(|error| DumpError::input("verify PGM section", error))?;
    let rows = decode_rows(entry.type_id, verified)
        .map_err(|error| DumpError::input("decode PGM rows", error))?;
    if rows.len() != entry.rows as usize {
        return Err(DumpError::message(format!(
            "section {} declares {} rows but decodes {}",
            entry.type_id,
            entry.rows,
            rows.len()
        )));
    }
    let truncated = rows.len() > options.limit;
    let rows_data = rows
        .iter()
        .take(options.limit)
        .map(|row| row_json(row, dictionary))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((Some(rows_data), Some(truncated), None))
}

fn section_profile(entry: Entry, body: &[u8]) -> Result<Option<usize>, DumpError> {
    let result = if is_dictionary(entry.type_id) {
        plain_parquet_decode_profile(body, MAX_DECODED_SECTION_BYTES)
    } else {
        parquet_decode_profile(body, MAX_DECODED_SECTION_BYTES)
    };
    match result {
        Ok(profile) => {
            if profile.rows != entry.rows as usize {
                return Err(DumpError::message(format!(
                    "section {} declares {} rows but Parquet metadata declares {}",
                    entry.type_id, entry.rows, profile.rows
                )));
            }
            Ok(Some(profile.decoded_bytes))
        }
        Err(CodecError::DecodedSectionTooLarge { .. }) => Ok(None),
        Err(error) => Err(DumpError::input("inspect Parquet section", error)),
    }
}

fn row_json(
    row: &kronika_reader::Row,
    dictionary: Option<&Dictionary>,
) -> Result<Map<String, Value>, DumpError> {
    row.iter()
        .map(|(name, cell)| Ok((name.to_owned(), cell_json(cell, dictionary)?)))
        .collect()
}

fn cell_json(cell: &Cell, dictionary: Option<&Dictionary>) -> Result<Value, DumpError> {
    let value = match cell {
        Cell::I16(value) => Value::from(i64::from(*value)),
        Cell::I32(value) => Value::from(i64::from(*value)),
        Cell::I64(value) | Cell::Ts(value) => Value::from(*value),
        Cell::U32(value) => Value::from(u64::from(*value)),
        Cell::U64(value) => Value::from(*value),
        Cell::F64(value) => float_json(*value),
        Cell::Bool(value) => Value::from(*value),
        Cell::ListI32(values) => Value::Array(
            values
                .iter()
                .map(|value| Value::from(i64::from(*value)))
                .collect(),
        ),
        Cell::Null | Cell::StrId(0) => Value::Null,
        Cell::StrId(id) => {
            let resolved = dictionary
                .and_then(|dictionary| dictionary.resolve(*id))
                .ok_or_else(|| {
                    DumpError::message(format!("section row references missing dictionary id {id}"))
                })?;
            resolved_json(resolved)
        }
    };
    Ok(value)
}

fn float_json(value: f64) -> Value {
    Number::from_f64(value).map_or_else(
        || {
            Value::String(
                if value.is_nan() {
                    "NaN"
                } else if value.is_sign_positive() {
                    "Infinity"
                } else {
                    "-Infinity"
                }
                .to_owned(),
            )
        },
        Value::Number,
    )
}

fn resolved_json(resolved: Resolved<'_>) -> Value {
    match resolved {
        Resolved::String(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
        Resolved::Blob {
            bytes,
            full_len,
            truncated,
        } => {
            let mut object = Map::new();
            object.insert(
                "text".to_owned(),
                Value::String(String::from_utf8_lossy(bytes).into_owned()),
            );
            object.insert("full_len".to_owned(), Value::from(full_len));
            object.insert("truncated".to_owned(), Value::from(truncated));
            Value::Object(object)
        }
    }
}

pub(crate) const fn is_dictionary(type_id: u32) -> bool {
    matches!(type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID)
}

fn dictionary_exceeds_decode_limit<R: ReadAt>(unit: &PgmUnit<R>) -> Result<bool, DumpError> {
    for (ordinal, entry) in unit.catalog().entries.iter().copied().enumerate() {
        if !is_dictionary(entry.type_id) {
            continue;
        }
        let ordinal = u32::try_from(ordinal)
            .map_err(|_error| DumpError::message("catalog ordinal does not fit u32"))?;
        let section = unit
            .read_overview_section(ordinal)
            .map_err(|error| DumpError::input("read PGM dictionary", error))?;
        if section_profile(entry, section.body())?.is_none() {
            return Ok(true);
        }
    }
    Ok(false)
}

fn section_uses_dictionary(type_id: u32) -> bool {
    registry()
        .iter()
        .find(|contract| contract.type_id.get() == type_id)
        .is_some_and(|contract| {
            contract
                .columns
                .iter()
                .any(|column| matches!(column.ty, ColumnType::StrId))
        })
}

pub(crate) const fn timestamp_range(min_ts: i64, max_ts: i64) -> (Option<i64>, Option<i64>) {
    if min_ts <= max_ts {
        (Some(min_ts), Some(max_ts))
    } else {
        (None, None)
    }
}

pub(crate) fn type_id_label(type_id: u32) -> String {
    let class = type_id / 1_000_000;
    let source = (type_id / 1_000) % 1_000;
    let version = type_id % 1_000;
    let prefix = match class {
        1 => "S",
        2 => "E",
        3 => "D",
        10 => "C",
        _ => return type_id.to_string(),
    };
    format!("{prefix}_{source:03}_{version:03}")
}

pub(crate) fn checked_add(left: u64, right: u64, quantity: &'static str) -> Result<u64, DumpError> {
    left.checked_add(right)
        .ok_or_else(|| DumpError::message(format!("{quantity} overflow")))
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

#[expect(
    clippy::cast_precision_loss,
    reason = "JSON compression ratios are descriptive floating-point values"
)]
pub(crate) fn ratio(decoded: Option<u64>, stored: u64) -> Option<f64> {
    decoded
        .filter(|_decoded| stored != 0)
        .map(|decoded| decoded as f64 / stored as f64)
}

#[expect(
    clippy::cast_precision_loss,
    reason = "JSON file shares are descriptive floating-point values"
)]
fn share(bytes: u64, file_bytes: u64) -> f64 {
    if file_bytes == 0 {
        0.0
    } else {
        bytes as f64 / file_bytes as f64
    }
}

fn segment_id_from_path(path: &Path) -> Option<i64> {
    if path.extension()?.to_str()? != "pgm" {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    let value = stem.parse::<i64>().ok()?;
    (value.to_string() == stem && kronika_layout::SegmentId::new(value).is_ok()).then_some(value)
}

fn verify_identity(file: &File, expected: FileIdentity) -> Result<(), DumpError> {
    let observed =
        FileIdentity::from_file(file).map_err(|error| DumpError::input("stat PGM", error))?;
    if observed == expected {
        Ok(())
    } else {
        Err(DumpError::message(
            "PGM changed while it was being inspected",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::float_json;

    #[test]
    fn nonfinite_floats_remain_distinct_from_null() {
        assert_eq!(float_json(f64::NAN), "NaN");
        assert_eq!(float_json(f64::INFINITY), "Infinity");
        assert_eq!(float_json(f64::NEG_INFINITY), "-Infinity");
        assert!(float_json(1.25).is_number());
    }
}
