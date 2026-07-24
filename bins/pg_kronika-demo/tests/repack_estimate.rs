//! Manual repack experiment: how much a sealed `.pgm` shrinks when every
//! per-window section of one type merges into a single Parquet body and the
//! dictionary parts deduplicate.
//!
//! `KRONIKA_REPACK_DIR=<segments dir> cargo test -p pg_kronika-demo --test repack_estimate -- --nocapture --ignored`

#![allow(
    unused_crate_dependencies,
    reason = "an integration test binary uses a subset of the package dependencies"
)]
#![allow(
    clippy::cast_precision_loss,
    reason = "byte counts far below 2^52 rendered as ratios in a research report"
)]

use std::collections::{BTreeMap, HashSet};

use arrow_array::{Array, RecordBatch, UInt32Array};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;

// kronika_registry::DICT_{STRINGS,BLOBS}_TYPE_ID; the registry crate is not
// a demo dependency.
const DICT_STRINGS: u32 = 3_001_001;
const DICT_BLOBS: u32 = 3_002_001;

fn writer_props() -> WriterProperties {
    WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(3).expect("zstd level 3 is valid"),
        ))
        .set_max_row_group_size(1_000_000)
        .set_created_by(String::new())
        .build()
}

fn read_batches(body: &[u8]) -> Option<Vec<RecordBatch>> {
    let bytes = bytes::Bytes::copy_from_slice(body);
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes).ok()?;
    let reader = builder.build().ok()?;
    reader.collect::<Result<Vec<_>, _>>().ok()
}

fn write_merged(batches: &[RecordBatch]) -> Option<u64> {
    let schema = batches.first()?.schema();
    let mut out = Vec::new();
    let mut writer = ArrowWriter::try_new(&mut out, schema, Some(writer_props())).ok()?;
    for batch in batches {
        writer.write(batch).ok()?;
    }
    writer.close().ok()?;
    Some(out.len() as u64)
}

/// Keeps the first row per unique `str_id`, preserving every column.
fn dedup_by_str_id(batches: &[RecordBatch]) -> Option<(Vec<RecordBatch>, usize, usize)> {
    let mut seen: HashSet<u64> = HashSet::new();
    let mut kept = Vec::new();
    let mut total_rows = 0_usize;
    for batch in batches {
        total_rows += batch.num_rows();
        let ids = batch
            .column_by_name("str_id")?
            .as_any()
            .downcast_ref::<arrow_array::UInt64Array>()?;
        let mut keep_rows: Vec<u32> = Vec::new();
        for row in 0..batch.num_rows() {
            if seen.insert(ids.value(row)) {
                keep_rows.push(u32::try_from(row).ok()?);
            }
        }
        if keep_rows.is_empty() {
            continue;
        }
        let indices = UInt32Array::from(keep_rows);
        let columns = batch
            .columns()
            .iter()
            .map(|column| arrow_select::take::take(column, &indices, None))
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        kept.push(RecordBatch::try_new(batch.schema(), columns).ok()?);
    }
    let unique = seen.len();
    Some((kept, total_rows, unique))
}

#[test]
#[ignore = "manual repack experiment, needs KRONIKA_REPACK_DIR"]
fn repack_estimate() {
    let dir = std::env::var("KRONIKA_REPACK_DIR").expect("KRONIKA_REPACK_DIR");
    for entry in std::fs::read_dir(&dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "pgm") {
            continue;
        }
        let file_len = std::fs::metadata(&path).expect("stat").len();
        let file = std::fs::File::open(&path).expect("open pgm");
        let unit = kronika_reader::PgmUnit::open(file).expect("parse pgm");

        // type_id -> raw section bodies, in catalog order.
        let mut groups: BTreeMap<u32, (u64, Vec<Vec<u8>>)> = BTreeMap::new();
        for (ordinal, entry) in unit.catalog().entries.iter().enumerate() {
            let body = unit
                .read_overview_section(u32::try_from(ordinal).expect("ordinal fits u32"))
                .expect("read section")
                .into_body();
            let slot = groups.entry(entry.type_id).or_default();
            slot.0 += entry.len;
            slot.1.push(body.as_ref().to_vec());
        }

        let mut old_total = 0_u64;
        let mut new_total = 0_u64;
        let mut unparsed = 0_u64;
        let mut report: Vec<(u32, u64, u64, usize)> = Vec::new();
        for (type_id, (old_len, bodies)) in &groups {
            old_total += old_len;
            let parts = bodies.len();
            let batches: Option<Vec<RecordBatch>> = bodies
                .iter()
                .map(|body| read_batches(body))
                .collect::<Option<Vec<_>>>()
                .map(|nested| nested.into_iter().flatten().collect());
            let Some(batches) = batches else {
                unparsed += old_len;
                new_total += old_len;
                report.push((*type_id, *old_len, *old_len, parts));
                continue;
            };
            let new_len = if *type_id == DICT_STRINGS || *type_id == DICT_BLOBS {
                let (kept, total_rows, unique) =
                    dedup_by_str_id(&batches).expect("dictionary schema has str_id");
                let merged = write_merged(&kept).unwrap_or(*old_len);
                println!(
                    "  dict {type_id}: {total_rows} rows -> {unique} unique str_id ({}x duplication)",
                    total_rows.checked_div(unique).unwrap_or(0),
                );
                merged
            } else if batches.is_empty() {
                0
            } else {
                write_merged(&batches).unwrap_or(*old_len)
            };
            new_total += new_len;
            report.push((*type_id, *old_len, new_len, parts));
        }

        report.sort_by_key(|(_, old_len, _, _)| std::cmp::Reverse(*old_len));
        println!("== {} ==", path.file_name().unwrap().to_string_lossy());
        println!(
            "{:>10} {:>12} {:>12} {:>7} {:>6}  type_id",
            "old", "new", "saved", "parts", "x"
        );
        for (type_id, old_len, new_len, parts) in report.iter().take(18) {
            let ratio = if *new_len > 0 {
                *old_len as f64 / *new_len as f64
            } else {
                0.0
            };
            println!(
                "{old_len:>10} {new_len:>12} {:>12} {parts:>7} {ratio:>6.1}  {type_id}",
                old_len.saturating_sub(*new_len),
            );
        }
        let catalog_overhead = file_len - old_total;
        println!(
            "bodies: {old_total} -> {new_total}; file: {file_len} -> ~{} ({:.1}x); unparsed {unparsed}",
            new_total + catalog_overhead,
            file_len as f64 / (new_total + catalog_overhead) as f64,
        );
    }
}
