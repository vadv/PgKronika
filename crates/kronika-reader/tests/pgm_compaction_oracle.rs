//! Maintained losslessness oracle for production PGM compaction.
//!
//! This fixture is deliberately registry-driven. Adding a layout without
//! reaching this test changes the expected family count and fails. Four input
//! windows exercise repeated section overhead; dense, reset-heavy,
//! nullable-heavy, and short-tail distributions verify exact Arrow values,
//! every physical column kind, dictionary values, segment metadata, and
//! deterministic bytes through the real journal/seal/reader path.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::File;
use std::sync::Arc;

use arrow_array::types::Int32Type;
use arrow_array::{
    Array, ArrayRef, BinaryArray, BooleanArray, FixedSizeBinaryArray, Float32Array, Float64Array,
    Int8Array, Int16Array, Int32Array, Int64Array, ListArray, RecordBatch, UInt8Array, UInt16Array,
    UInt32Array, UInt64Array,
};
use arrow_schema::{DataType, Field, Schema};
use kronika_format::{PartMeta, SectionInput, StrId, try_build_part};
use kronika_reader::{PgmUnit, Resolved};
use kronika_registry::{
    ColumnClass, ColumnType, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, MAX_SECTION_ROWS,
    TypeContract, arrow_schema, canonicalize_batches, registry,
};
use kronika_writer::{Journal, JournalConfig, Publication, seal};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_writer::ArrowWriterOptions;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

// This integration target shares the crate's full dev-dependency set.
use criterion as _;
use kronika_analytics as _;
use kronika_store as _;
use mimalloc as _;
use proptest as _;
use rustix as _;
use serde as _;
use serde_json as _;
use sha2 as _;

type TestResult<T = ()> = Result<T, Box<dyn Error>>;

const EXPECTED_LAYOUTS: usize = 75;
const SOURCE_ID: u64 = 0x7067_6d5f_6c61_6201;
const MIN_TS: i64 = 1_780_000_000_000_000;

#[derive(Debug, Clone, Copy)]
enum FixtureClass {
    Dense,
    ResetHeavy,
    NullableHeavy,
    ShortTail,
}

impl FixtureClass {
    const ALL: [Self; 4] = [
        Self::Dense,
        Self::ResetHeavy,
        Self::NullableHeavy,
        Self::ShortTail,
    ];

    const fn windows(self) -> usize {
        match self {
            Self::ShortTail => 1,
            Self::Dense | Self::ResetHeavy | Self::NullableHeavy => 4,
        }
    }

    const fn rows_per_window(self) -> usize {
        match self {
            Self::ShortTail => 3,
            Self::Dense | Self::ResetHeavy | Self::NullableHeavy => 8,
        }
    }
}

#[test]
fn every_registered_layout_roundtrips_all_fixture_classes_exactly() -> TestResult {
    assert_eq!(
        registry().len(),
        EXPECTED_LAYOUTS,
        "update the oracle deliberately when the registry changes"
    );
    for class in FixtureClass::ALL {
        roundtrip_fixture(class, false)?;
    }
    Ok(())
}

#[test]
fn all_layout_bytes_are_deterministic_under_window_reordering() -> TestResult {
    let forward = roundtrip_fixture(FixtureClass::Dense, false)?;
    let reverse = roundtrip_fixture(FixtureClass::Dense, true)?;
    assert_eq!(
        forward, reverse,
        "canonical data/dictionary/catalog bytes cannot depend on part order"
    );
    Ok(())
}

fn roundtrip_fixture(class: FixtureClass, reverse: bool) -> TestResult<Vec<u8>> {
    let dir = tempfile::tempdir()?;
    let journal_path = dir.path().join("active.parts");
    let output = dir.path().join("fixture.pgm");
    let (mut journal, report) = Journal::open(&journal_path, JournalConfig::default())?;
    assert!(report.is_clean());

    let windows = class.windows();
    let rows_per_window = class.rows_per_window();
    let order: Vec<usize> = if reverse {
        (0..windows).rev().collect()
    } else {
        (0..windows).collect()
    };
    let mut expected = BTreeMap::<u32, Vec<RecordBatch>>::new();
    for window in order {
        let row_start = window * rows_per_window;
        let mut bodies = Vec::<(u32, u32, Vec<u8>)>::new();
        for contract in registry() {
            let batch =
                synthetic_contract_batch(contract, class, row_start, rows_per_window)?;
            expected
                .entry(contract.type_id.get())
                .or_default()
                .push(batch.clone());
            bodies.push((
                contract.type_id.get(),
                u32::try_from(batch.num_rows())?,
                encode_collection_batch(&batch)?,
            ));
        }
        bodies.extend(dictionary_bodies()?);
        let inputs: Vec<SectionInput<'_>> = bodies
            .iter()
            .map(|(type_id, rows, body)| SectionInput {
                type_id: *type_id,
                rows: *rows,
                body,
            })
            .collect();
        let min_ts = MIN_TS + i64::try_from(row_start)? * 5_000_000;
        let max_ts = min_ts + i64::try_from(rows_per_window.saturating_sub(1))? * 5_000_000;
        let part = try_build_part(
            &inputs,
            PartMeta {
                min_ts,
                max_ts,
                source_id: SOURCE_ID,
            },
        )?;
        journal.append(&part)?;
    }

    let summary = seal(&journal, &output)?;
    assert_eq!(summary.publication, Publication::Created);
    assert_eq!(
        summary.sections,
        EXPECTED_LAYOUTS + 2,
        "75 data families plus two dictionaries"
    );
    assert_eq!(
        summary.rows,
        u64::try_from(EXPECTED_LAYOUTS * windows * rows_per_window)?
    );

    let unit = PgmUnit::open(File::open(&output)?)?;
    assert_eq!(unit.catalog().source_id, SOURCE_ID);
    assert_eq!(unit.catalog().min_ts, MIN_TS);
    let expected_max =
        MIN_TS + i64::try_from(windows * rows_per_window - 1)? * 5_000_000;
    assert_eq!(unit.catalog().max_ts, expected_max);
    assert_eq!(unit.catalog().entries.len(), EXPECTED_LAYOUTS + 2);

    let mut seen = 0_usize;
    for entry in &unit.catalog().entries {
        if matches!(
            entry.type_id,
            DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID
        ) {
            continue;
        }
        let source = expected.get(&entry.type_id).expect("expected family");
        let expected_batch = canonicalize_batches(entry.type_id, source)?;
        let decoded = unit.decode(entry)?;
        let actual_batch = canonicalize_batches(entry.type_id, &decoded.batches)?;
        assert_eq!(
            actual_batch, expected_batch,
            "exact Arrow mismatch for type {} in {class:?}",
            entry.type_id
        );
        assert_eq!(entry.rows as usize, expected_batch.num_rows());
        seen += 1;
    }
    assert_eq!(seen, EXPECTED_LAYOUTS, "no registered family may disappear");

    let dictionary = unit.dictionary()?;
    assert_eq!(dictionary.len(), 8);
    for slot in 1..=7 {
        let bytes = dictionary_bytes(slot);
        let id = dictionary_id(&bytes);
        assert_eq!(dictionary.resolve(id), Some(Resolved::String(&bytes)));
    }
    let blob = vec![0x5a_u8; 256];
    let blob_id = dictionary_id(&blob);
    assert_eq!(
        dictionary.resolve(blob_id),
        Some(Resolved::Blob {
            bytes: &blob,
            full_len: 256,
            truncated: false,
        })
    );
    Ok(std::fs::read(output)?)
}

fn encode_collection_batch(batch: &RecordBatch) -> TestResult<Vec<u8>> {
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3)?))
        .set_max_row_group_size(MAX_SECTION_ROWS)
        .set_created_by(String::new())
        .build();
    let options = ArrowWriterOptions::new()
        .with_properties(properties)
        .with_skip_arrow_metadata(true);
    let mut body = Vec::new();
    let mut writer = ArrowWriter::try_new_with_options(&mut body, batch.schema(), options)?;
    writer.write(batch)?;
    writer.close()?;
    Ok(body)
}

fn synthetic_contract_batch(
    contract: &TypeContract,
    class: FixtureClass,
    row_start: usize,
    rows: usize,
) -> TestResult<RecordBatch> {
    let columns = contract
        .columns
        .iter()
        .enumerate()
        .map(|(column_index, column)| {
            synthetic_column(
                column.ty,
                column.class,
                column.nullable,
                column_index,
                class,
                row_start,
                rows,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(RecordBatch::try_new(arrow_schema(contract), columns)?)
}

#[allow(
    clippy::too_many_arguments,
    reason = "the registry-driven fixture supplies every physical column dimension explicitly"
)]
fn synthetic_column(
    ty: ColumnType,
    column_class: ColumnClass,
    nullable: bool,
    column_index: usize,
    fixture: FixtureClass,
    row_start: usize,
    rows: usize,
) -> TestResult<ArrayRef> {
    let null_at = |row: usize| {
        if !nullable {
            return false;
        }
        let modulus = match fixture {
            FixtureClass::NullableHeavy => 2,
            FixtureClass::Dense | FixtureClass::ResetHeavy | FixtureClass::ShortTail => 7,
        };
        (row_start + row + column_index).is_multiple_of(modulus)
    };
    let signed_value = |row: usize| {
        let global = row_start + row;
        if matches!(fixture, FixtureClass::ResetHeavy)
            && column_class == ColumnClass::Cumulative
            && global >= 16
        {
            i64::try_from(global - 16).unwrap_or(0)
        } else {
            i64::try_from((global + column_index * 3) % 101).unwrap_or(0)
        }
    };
    let unsigned_value = |row: usize| u64::try_from(signed_value(row).max(0)).unwrap_or(0);
    let array: ArrayRef = match ty {
        ColumnType::I8 => Arc::new(Int8Array::from_iter(
            (0..rows).map(|row| maybe(null_at(row), signed_value(row) as i8)),
        )),
        ColumnType::I16 => Arc::new(Int16Array::from_iter(
            (0..rows).map(|row| maybe(null_at(row), signed_value(row) as i16)),
        )),
        ColumnType::I32 => Arc::new(Int32Array::from_iter(
            (0..rows).map(|row| maybe(null_at(row), signed_value(row) as i32)),
        )),
        ColumnType::I64 => Arc::new(Int64Array::from_iter(
            (0..rows).map(|row| maybe(null_at(row), signed_value(row))),
        )),
        ColumnType::U8 => Arc::new(UInt8Array::from_iter(
            (0..rows).map(|row| maybe(null_at(row), unsigned_value(row) as u8)),
        )),
        ColumnType::U16 => Arc::new(UInt16Array::from_iter(
            (0..rows).map(|row| maybe(null_at(row), unsigned_value(row) as u16)),
        )),
        ColumnType::U32 => Arc::new(UInt32Array::from_iter(
            (0..rows).map(|row| maybe(null_at(row), unsigned_value(row) as u32)),
        )),
        ColumnType::U64 => Arc::new(UInt64Array::from_iter(
            (0..rows).map(|row| maybe(null_at(row), unsigned_value(row))),
        )),
        ColumnType::F32 => Arc::new(Float32Array::from_iter((0..rows).map(|row| {
            let value = if row_start + row == 0 {
                -0.0
            } else {
                signed_value(row) as f32 / 3.0
            };
            maybe(null_at(row), value)
        }))),
        ColumnType::F64 => Arc::new(Float64Array::from_iter((0..rows).map(|row| {
            let value = match row_start + row {
                0 => -0.0,
                1 => f64::from_bits(0x7ff8_0000_0000_0001),
                _ => signed_value(row) as f64 / 7.0,
            };
            maybe(null_at(row), value)
        }))),
        ColumnType::Bool => Arc::new(BooleanArray::from_iter((0..rows).map(|row| {
            maybe(
                null_at(row),
                (row_start + row + column_index).is_multiple_of(2),
            )
        }))),
        ColumnType::Ts => Arc::new(Int64Array::from_iter((0..rows).map(|row| {
            // Repeated adjacent timestamps force complete non-key tie-breaks.
            let global = (row_start + row) / 2;
            maybe(
                null_at(row),
                MIN_TS + i64::try_from(global).unwrap_or(0) * 10_000_000,
            )
        }))),
        ColumnType::StrId => Arc::new(UInt64Array::from_iter((0..rows).map(|row| {
            maybe(
                null_at(row),
                dictionary_id(&dictionary_bytes(
                    (row_start + row + column_index) % 7 + 1,
                )),
            )
        }))),
        ColumnType::ListI32 => {
            let list = ListArray::from_iter_primitive::<Int32Type, _, _>((0..rows).map(|row| {
                if null_at(row) {
                    None
                } else if (row_start + row).is_multiple_of(3) {
                    Some(Vec::<Option<i32>>::new())
                } else {
                    let value = signed_value(row) as i32;
                    Some(vec![Some(value), Some(-value)])
                }
            }));
            Arc::new(ListArray::new(
                Arc::new(Field::new("item", DataType::Int32, false)),
                list.offsets().clone(),
                Arc::clone(list.values()),
                list.nulls().cloned(),
            ))
        }
    };
    Ok(array)
}

fn maybe<T>(is_null: bool, value: T) -> Option<T> {
    if is_null { None } else { Some(value) }
}

fn dictionary_bytes(slot: usize) -> Vec<u8> {
    format!("contract-value-{slot}").into_bytes()
}

fn dictionary_id(bytes: &[u8]) -> u64 {
    StrId::of(bytes)
        .expect("fixture value must not hash to zero")
        .get()
}

fn dictionary_bodies() -> TestResult<Vec<(u32, u32, Vec<u8>)>> {
    let mut strings: Vec<(u64, Vec<u8>)> = (1..=7)
        .map(|slot| {
            let bytes = dictionary_bytes(slot);
            (dictionary_id(&bytes), bytes)
        })
        .collect();
    strings.sort_unstable_by_key(|(id, _bytes)| *id);
    let ids = UInt64Array::from_iter_values(strings.iter().map(|(id, _bytes)| *id));
    let bytes = BinaryArray::from_iter_values(strings.iter().map(|(_id, bytes)| bytes.as_slice()));
    let string_batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("str_id", DataType::UInt64, false),
            Field::new("bytes", DataType::Binary, false),
        ])),
        vec![Arc::new(ids), Arc::new(bytes)],
    )?;

    let blob = vec![0x5a_u8; 256];
    let blob_id = dictionary_id(&blob);
    let ids = UInt64Array::from_iter_values([blob_id]);
    let stored = BinaryArray::from_iter_values([blob.as_slice()]);
    let full_len = UInt64Array::from_iter_values([blob.len() as u64]);
    let truncated = BooleanArray::from(vec![false]);
    let hash = FixedSizeBinaryArray::try_from_sparse_iter_with_size(
        [None::<[u8; 32]>].into_iter(),
        32,
    )?;
    let blob_batch = RecordBatch::try_new(
        Arc::new(Schema::new(vec![
            Field::new("str_id", DataType::UInt64, false),
            Field::new("stored_bytes", DataType::Binary, false),
            Field::new("full_len", DataType::UInt64, false),
            Field::new("truncated", DataType::Boolean, false),
            Field::new("full_sha256", DataType::FixedSizeBinary(32), true),
        ])),
        vec![
            Arc::new(ids),
            Arc::new(stored),
            Arc::new(full_len),
            Arc::new(truncated),
            Arc::new(hash),
        ],
    )?;
    Ok(vec![
        (
            DICT_STRINGS_TYPE_ID,
            u32::try_from(string_batch.num_rows())?,
            encode_collection_batch(&string_batch)?,
        ),
        (
            DICT_BLOBS_TYPE_ID,
            u32::try_from(blob_batch.num_rows())?,
            encode_collection_batch(&blob_batch)?,
        ),
    ])
}
