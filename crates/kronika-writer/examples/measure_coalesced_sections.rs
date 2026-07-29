//! Deterministic baseline/candidate measurement for sealed PGM section coalescing.

#![allow(
    clippy::cast_possible_truncation,
    reason = "decoded F32 cells are losslessly widened to f64 and narrowed only to recover their original bits"
)]

use std::collections::BTreeMap;
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use arrow_array::builder::{Int32Builder, ListBuilder};
use arrow_array::{
    Array, ArrayRef, BinaryArray, BooleanArray, FixedSizeBinaryArray, Float32Array, Float64Array,
    Int8Array, Int16Array, Int32Array, Int64Array, RecordBatch, UInt8Array, UInt16Array,
    UInt32Array, UInt64Array,
};
use arrow_schema::{DataType, Field};
use kronika_format::{
    Catalog, DictLimits, PartMeta, SectionInput, SegmentDicts, TAIL_INDEX_LEN, TailIndex,
    build_part, crc32c,
};
use kronika_layout::{
    ACTIVE_JOURNAL_NAME, DataRoot, FileKind, LayoutLimits, SegmentAddress, SegmentId,
};
use kronika_reader::{LocalDirSnapshot, Segment, section};
use kronika_registry::{
    Bytes, Cell, Column, ColumnType, DICT_BLOBS_TYPE_ID, DICT_STRINGS_TYPE_ID, MAX_SECTION_BYTES,
    MAX_SECTION_ROWS, Row, TypeContract, VerifiedSection, arrow_schema, decode_rows, registry,
};
use kronika_writer::{Journal, JournalConfig, dict, seal};
use parquet::arrow::ArrowWriter;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::arrow_writer::ArrowWriterOptions;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use sha2::{Digest, Sha256};
use tempfile as _;

const WINDOWS: usize = 4;
const ROWS_PER_WINDOW: usize = 8;
const EXPECTED_REGISTRY_TYPES: usize = 76;
const DATA_SECTIONS_PER_PART: usize = EXPECTED_REGISTRY_TYPES;
const DICTIONARY_SECTIONS_PER_PART: usize = 2;
const INPUT_SECTIONS: usize = WINDOWS * (DATA_SECTIONS_PER_PART + DICTIONARY_SECTIONS_PER_PART);
const EXPECTED_DATA_ROWS: usize = WINDOWS * ROWS_PER_WINDOW * EXPECTED_REGISTRY_TYPES;
const EXPECTED_DICTIONARY_ENTRIES: usize = 33;
const MAX_OUTPUT_SECTIONS: usize = INPUT_SECTIONS;
const SHARED_DICTIONARY_VALUE: &[u8] = b"measurement/shared";
const FIRST_TS_US: i64 = 1_700_000_000_000_000;

type AnyError = Box<dyn Error>;
type Result<T> = std::result::Result<T, AnyError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Baseline,
    Candidate,
}

impl Mode {
    fn parse(value: &str) -> Result<Self> {
        match value {
            "baseline" => Ok(Self::Baseline),
            "candidate" => Ok(Self::Candidate),
            _ => Err(invalid("mode must be \"baseline\" or \"candidate\"")),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Baseline => "baseline",
            Self::Candidate => "candidate",
        }
    }

    const fn expected_sections(self) -> usize {
        match self {
            Self::Baseline => INPUT_SECTIONS,
            Self::Candidate => DATA_SECTIONS_PER_PART + DICTIONARY_SECTIONS_PER_PART,
        }
    }

    const fn expected_multiplicity(self) -> usize {
        match self {
            Self::Baseline => WINDOWS,
            Self::Candidate => 1,
        }
    }

    const fn expected_dictionary_rows(self) -> usize {
        match self {
            Self::Baseline => WINDOWS * (ROWS_PER_WINDOW + 1),
            Self::Candidate => EXPECTED_DICTIONARY_ENTRIES,
        }
    }
}

#[derive(Debug)]
enum Command {
    Prepare {
        output_dir: PathBuf,
    },
    Seal {
        journal_path: PathBuf,
        output_dir: PathBuf,
        mode: Mode,
    },
    Read {
        pgm_path: PathBuf,
        mode: Mode,
    },
    Query {
        pgm_path: PathBuf,
    },
}

#[derive(Debug)]
struct OwnedSection {
    type_id: u32,
    rows: u32,
    body: Vec<u8>,
}

#[derive(Debug, Default)]
struct LogicalData {
    data: BTreeMap<u32, Vec<Vec<u8>>>,
    dictionaries: BTreeMap<(u32, u64), Vec<u8>>,
}

impl LogicalData {
    fn add_data_body(&mut self, type_id: u32, body: &[u8], expected_crc: u32) -> Result<usize> {
        let verified = VerifiedSection::verify(Bytes::copy_from_slice(body), expected_crc, crc32c)?;
        let rows = decode_rows(type_id, verified)?;
        let row_count = rows.len();
        let encoded = self.data.entry(type_id).or_default();
        for row in &rows {
            encoded.push(encode_row(row)?);
        }
        Ok(row_count)
    }

    fn add_dictionary_body(
        &mut self,
        type_id: u32,
        body: &[u8],
        expected_crc: u32,
    ) -> Result<usize> {
        if crc32c(body) != expected_crc {
            return Err(invalid(format!(
                "dictionary section {type_id} failed its catalog CRC"
            )));
        }
        let rows = decode_dictionary_rows(type_id, body)?;
        let row_count = rows.len();
        for (id, row) in rows {
            match self.dictionaries.entry((type_id, id)) {
                std::collections::btree_map::Entry::Vacant(entry) => {
                    entry.insert(row);
                }
                std::collections::btree_map::Entry::Occupied(entry) if entry.get() == &row => {}
                std::collections::btree_map::Entry::Occupied(_) => {
                    return Err(invalid(format!(
                        "dictionary id {id} has conflicting rows in type {type_id}"
                    )));
                }
            }
        }
        Ok(row_count)
    }

    fn validate_shape(&self) -> Result<()> {
        if self.data.len() != EXPECTED_REGISTRY_TYPES {
            return Err(invalid(format!(
                "logical data contains {} registry types, expected {EXPECTED_REGISTRY_TYPES}",
                self.data.len()
            )));
        }
        for contract in registry() {
            let type_id = contract.type_id.get();
            let rows = self
                .data
                .get(&type_id)
                .ok_or_else(|| invalid(format!("logical data is missing type {type_id}")))?;
            let expected = WINDOWS * ROWS_PER_WINDOW;
            if rows.len() != expected {
                return Err(invalid(format!(
                    "type {type_id} has {} logical rows, expected {expected}",
                    rows.len()
                )));
            }
        }
        if self.dictionaries.len() != EXPECTED_DICTIONARY_ENTRIES {
            return Err(invalid(format!(
                "logical dictionaries contain {} unique entries, expected {EXPECTED_DICTIONARY_ENTRIES}",
                self.dictionaries.len()
            )));
        }
        if !self
            .dictionaries
            .keys()
            .any(|(type_id, _)| *type_id == DICT_STRINGS_TYPE_ID)
            || !self
                .dictionaries
                .keys()
                .any(|(type_id, _)| *type_id == DICT_BLOBS_TYPE_ID)
        {
            return Err(invalid("both logical dictionary types must be populated"));
        }
        Ok(())
    }

    fn digest(mut self) -> Result<LogicalDigest> {
        self.validate_shape()?;
        let mut hasher = Sha256::new();
        let mut logical_bytes = 0_u64;
        hash_field(
            &mut hasher,
            &mut logical_bytes,
            b"pgkronika-coalesced-sections-logical-v1",
        )?;
        let mut data_rows = 0_usize;
        for (type_id, rows) in &mut self.data {
            rows.sort_unstable();
            hash_field(&mut hasher, &mut logical_bytes, b"D")?;
            hash_field(&mut hasher, &mut logical_bytes, &type_id.to_le_bytes())?;
            hash_len(&mut hasher, &mut logical_bytes, rows.len())?;
            for row in rows.iter() {
                hash_field(&mut hasher, &mut logical_bytes, row)?;
            }
            data_rows = data_rows
                .checked_add(rows.len())
                .ok_or_else(|| invalid("logical data row count overflowed"))?;
        }
        for ((type_id, id), row) in &self.dictionaries {
            hash_field(&mut hasher, &mut logical_bytes, b"K")?;
            hash_field(&mut hasher, &mut logical_bytes, &type_id.to_le_bytes())?;
            hash_field(&mut hasher, &mut logical_bytes, &id.to_le_bytes())?;
            hash_field(&mut hasher, &mut logical_bytes, row)?;
        }
        Ok(LogicalDigest {
            sha256: hasher.finalize().into(),
            logical_bytes,
            data_rows,
            dictionary_entries: self.dictionaries.len(),
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LogicalDigest {
    sha256: [u8; 32],
    logical_bytes: u64,
    data_rows: usize,
    dictionary_entries: usize,
}

#[derive(Debug)]
struct OutputInspection {
    logical: LogicalDigest,
    catalog_sections: usize,
    catalog_rows: u64,
    catalog_data_rows: u64,
    catalog_dictionary_rows: u64,
}

fn main() -> Result<()> {
    let command = arguments()?;
    let registry_types = registry();
    if registry_types.len() != EXPECTED_REGISTRY_TYPES {
        return Err(invalid(format!(
            "registry contains {} types, expected {EXPECTED_REGISTRY_TYPES}",
            registry_types.len()
        )));
    }

    match command {
        Command::Prepare { output_dir } => prepare(&output_dir),
        Command::Seal {
            journal_path,
            output_dir,
            mode,
        } => seal_measurement(&journal_path, &output_dir, mode),
        Command::Read { pgm_path, mode } => read_measurement(&pgm_path, mode),
        Command::Query { pgm_path } => query_measurement(&pgm_path),
    }
}

fn prepare(output_dir: &Path) -> Result<()> {
    fs::create_dir(output_dir)?;
    let journal_path = output_dir.join("active.parts");
    let root = DataRoot::open(output_dir)?;
    let owner = root.acquire_writer(LayoutLimits::default())?;
    let mut journal = Journal::open(&owner, JournalConfig::default())?;
    if !journal.parts().is_empty() {
        return Err(invalid("new measurement journal did not open empty"));
    }
    let segment_id = measurement_segment_id()?;

    let started = Instant::now();
    let mut expected = LogicalData::default();
    for window in 0..WINDOWS {
        let part = build_window(window, &mut expected)?;
        journal.append(segment_id, &part)?;
    }
    if journal.parts().len() != WINDOWS {
        return Err(invalid(
            "journal does not contain exactly four completed parts",
        ));
    }
    let expected = expected.digest()?;
    if expected.data_rows != EXPECTED_DATA_ROWS {
        return Err(invalid("input logical data row total is wrong"));
    }

    let journal_bytes = fs::metadata(&journal_path)?.len();
    let journal_sha256 = sha256_file(&journal_path)?;
    let elapsed_ns = started.elapsed().as_nanos();
    println!("phase=prepare");
    println!("windows={WINDOWS}");
    println!("rows_per_type_per_window={ROWS_PER_WINDOW}");
    println!("registry_types={}", registry().len());
    println!("registry_type_id_sha256={}", hex(registry_type_id_digest()));
    println!("input_physical_sections={INPUT_SECTIONS}");
    println!("journal_path={}", journal_path.display());
    println!("journal_bytes={journal_bytes}");
    println!("journal_sha256={}", hex(journal_sha256));
    println!("journal_parts={}", journal.parts().len());
    println!("logical_data_rows={}", expected.data_rows);
    println!("logical_dictionary_entries={}", expected.dictionary_entries);
    println!("logical_bytes={}", expected.logical_bytes);
    println!("logical_sha256={}", hex(expected.sha256));
    println!("prepare_wall_ns={elapsed_ns}");
    println!("shape_valid=true");
    Ok(())
}

fn seal_measurement(journal_path: &Path, output_dir: &Path, mode: Mode) -> Result<()> {
    fs::create_dir(output_dir)?;
    let journal_root = journal_root(journal_path)?;
    let source_root = DataRoot::open(journal_root)?;
    let source_owner = source_root.acquire_writer(LayoutLimits::default())?;
    let journal = Journal::open(&source_owner, JournalConfig::default())?;
    if journal.parts().len() != WINDOWS {
        return Err(invalid(
            "measurement journal does not contain exactly four completed parts",
        ));
    }
    let output_root = DataRoot::open(output_dir)?;
    let output_owner = output_root.acquire_writer(LayoutLimits::default())?;
    let address = SegmentAddress::new(measurement_segment_id()?)?;
    let pgm_path = output_root.diagnostic_file_path(address, FileKind::Pgm);
    let journal_bytes = fs::metadata(journal_path)?.len();
    let journal_sha256 = sha256_file(journal_path)?;

    let started = Instant::now();
    let summary = seal(&journal, &output_owner, address)?;
    let seal_wall_ns = started.elapsed().as_nanos();
    if summary.sections != mode.expected_sections() {
        return Err(invalid(format!(
            "{} seal wrote {} sections, expected {}",
            mode.name(),
            summary.sections,
            mode.expected_sections()
        )));
    }

    let pgm_bytes = fs::metadata(&pgm_path)?.len();
    if summary.bytes != pgm_bytes {
        return Err(invalid("seal summary byte count differs from the PGM file"));
    }
    let pgm_sha256 = sha256_file(&pgm_path)?;

    println!("phase=seal");
    println!("mode={}", mode.name());
    println!("journal_path={}", journal_path.display());
    println!("journal_bytes={journal_bytes}");
    println!("journal_sha256={}", hex(journal_sha256));
    println!("journal_parts={}", journal.parts().len());
    println!("pgm_path={}", pgm_path.display());
    println!("pgm_bytes={pgm_bytes}");
    println!("pgm_sha256={}", hex(pgm_sha256));
    println!("catalog_sections={}", summary.sections);
    println!("seal_wall_ns={seal_wall_ns}");
    Ok(())
}

fn read_measurement(pgm_path: &Path, mode: Mode) -> Result<()> {
    let pgm_bytes = fs::metadata(pgm_path)?.len();
    let pgm_sha256 = sha256_file(pgm_path)?;

    let logical_started = Instant::now();
    let output = inspect_output(pgm_path, mode)?;
    let logical_read_wall_ns = logical_started.elapsed().as_nanos();
    let production = inspect_with_production_reader(pgm_path)?;
    if production.data_rows != output.logical.data_rows {
        return Err(invalid(
            "production reader row count differs from the canonical digest reader",
        ));
    }
    let expected_query_rows = expected_query_rows()?;
    if production.query_rows != expected_query_rows {
        return Err(invalid(format!(
            "production pg_stat_activity query returned {} rows, expected {expected_query_rows}",
            production.query_rows
        )));
    }
    if production.snapshot_units != 1
        || production.snapshot_warnings != 0
        || production.snapshot_damages != 0
    {
        return Err(invalid(
            "production restart snapshot did not contain one clean sealed unit",
        ));
    }
    if production.query_has_next_cursor {
        return Err(invalid(
            "production query unexpectedly returned a continuation cursor",
        ));
    }

    println!("phase=read");
    println!("mode={}", mode.name());
    println!("pgm_path={}", pgm_path.display());
    println!("pgm_bytes={pgm_bytes}");
    println!("pgm_sha256={}", hex(pgm_sha256));
    println!("catalog_sections={}", output.catalog_sections);
    println!("catalog_rows={}", output.catalog_rows);
    println!("catalog_data_rows={}", output.catalog_data_rows);
    println!("catalog_dictionary_rows={}", output.catalog_dictionary_rows);
    println!("logical_data_rows={}", output.logical.data_rows);
    println!(
        "logical_dictionary_entries={}",
        output.logical.dictionary_entries
    );
    println!("logical_bytes={}", output.logical.logical_bytes);
    println!("logical_sha256={}", hex(output.logical.sha256));
    println!("logical_read_wall_ns={logical_read_wall_ns}");
    println!("production_reader_open_wall_ns={}", production.open_wall_ns);
    println!(
        "production_reader_decode_wall_ns={}",
        production.decode_wall_ns
    );
    println!("production_reader_data_rows={}", production.data_rows);
    println!("snapshot_restart_wall_ns={}", production.restart_wall_ns);
    println!("snapshot_units={}", production.snapshot_units);
    println!("snapshot_warnings={}", production.snapshot_warnings);
    println!("snapshot_damages={}", production.snapshot_damages);
    println!("query_section=pg_stat_activity");
    println!("query_rows={}", production.query_rows);
    println!("query_gaps={}", production.query_gaps);
    println!("query_has_next_cursor={}", production.query_has_next_cursor);
    println!("query_wall_ns={}", production.query_wall_ns);
    println!("shape_valid=true");
    Ok(())
}

fn query_measurement(pgm_path: &Path) -> Result<()> {
    let production = inspect_production_query(pgm_path)?;
    validate_production_query(&production)?;

    println!("phase=query");
    println!("pgm_path={}", pgm_path.display());
    println!("pgm_bytes={}", fs::metadata(pgm_path)?.len());
    println!("snapshot_restart_wall_ns={}", production.restart_wall_ns);
    println!("snapshot_units={}", production.snapshot_units);
    println!("snapshot_warnings={}", production.snapshot_warnings);
    println!("snapshot_damages={}", production.snapshot_damages);
    println!("query_section=pg_stat_activity");
    println!("query_rows={}", production.query_rows);
    println!("query_gaps={}", production.query_gaps);
    println!("query_has_next_cursor={}", production.query_has_next_cursor);
    println!("query_wall_ns={}", production.query_wall_ns);
    println!("shape_valid=true");
    Ok(())
}

#[derive(Debug)]
struct ProductionRead {
    open_wall_ns: u128,
    decode_wall_ns: u128,
    data_rows: usize,
    restart_wall_ns: u128,
    snapshot_units: usize,
    snapshot_warnings: usize,
    snapshot_damages: usize,
    query_wall_ns: u128,
    query_rows: usize,
    query_gaps: usize,
    query_has_next_cursor: bool,
}

fn inspect_with_production_reader(path: &Path) -> Result<ProductionRead> {
    let open_started = Instant::now();
    let segment = Segment::open(path)?;
    let open_wall_ns = open_started.elapsed().as_nanos();

    let decode_started = Instant::now();
    let mut data_rows = 0_usize;
    for entry in &segment.catalog().entries {
        if matches!(entry.type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID) {
            continue;
        }
        let decoded = segment.decode(entry)?;
        for batch in decoded.batches {
            data_rows = data_rows
                .checked_add(batch.num_rows())
                .ok_or_else(|| invalid("production reader row count overflowed"))?;
        }
    }
    let _dictionary = segment.dictionary()?;
    let decode_wall_ns = decode_started.elapsed().as_nanos();

    let query = inspect_production_query(path)?;

    Ok(ProductionRead {
        open_wall_ns,
        decode_wall_ns,
        data_rows,
        restart_wall_ns: query.restart_wall_ns,
        snapshot_units: query.snapshot_units,
        snapshot_warnings: query.snapshot_warnings,
        snapshot_damages: query.snapshot_damages,
        query_wall_ns: query.query_wall_ns,
        query_rows: query.query_rows,
        query_gaps: query.query_gaps,
        query_has_next_cursor: query.query_has_next_cursor,
    })
}

#[derive(Debug)]
struct ProductionQuery {
    restart_wall_ns: u128,
    snapshot_units: usize,
    snapshot_warnings: usize,
    snapshot_damages: usize,
    query_wall_ns: u128,
    query_rows: usize,
    query_gaps: usize,
    query_has_next_cursor: bool,
}

fn inspect_production_query(path: &Path) -> Result<ProductionQuery> {
    let store_dir = segment_root(path)?;
    let restart_started = Instant::now();
    let mut snapshot = LocalDirSnapshot::open(store_dir)?;
    let restart_wall_ns = restart_started.elapsed().as_nanos();
    let snapshot_units = snapshot.units().len();
    let snapshot_warnings = snapshot.warnings().len();
    let snapshot_damages = snapshot.damages().len();

    let query_started = Instant::now();
    let page = section(
        &mut snapshot,
        "pg_stat_activity",
        i64::MIN,
        i64::MAX,
        EXPECTED_DATA_ROWS,
        None,
    )
    .map_err(|error| invalid(format!("production query failed: {error:?}")))?;
    let query_wall_ns = query_started.elapsed().as_nanos();

    Ok(ProductionQuery {
        restart_wall_ns,
        snapshot_units,
        snapshot_warnings,
        snapshot_damages,
        query_wall_ns,
        query_rows: page.rows.len(),
        query_gaps: page.gaps.len(),
        query_has_next_cursor: page.next_cursor.is_some(),
    })
}

fn measurement_segment_id() -> Result<SegmentId> {
    Ok(SegmentId::new(FIRST_TS_US)?)
}

fn journal_root(path: &Path) -> Result<&Path> {
    if path.file_name().and_then(|name| name.to_str()) != Some(ACTIVE_JOURNAL_NAME) {
        return Err(invalid(format!(
            "measurement journal must be named {ACTIVE_JOURNAL_NAME}"
        )));
    }
    path.parent()
        .ok_or_else(|| invalid("measurement journal has no data root"))
}

fn segment_root(path: &Path) -> Result<&Path> {
    let day = path
        .parent()
        .ok_or_else(|| invalid("measurement PGM has no UTC day directory"))?;
    let month = day
        .parent()
        .ok_or_else(|| invalid("measurement PGM has no UTC month directory"))?;
    let year = month
        .parent()
        .ok_or_else(|| invalid("measurement PGM has no UTC year directory"))?;
    year.parent()
        .ok_or_else(|| invalid("measurement PGM has no data root"))
}

fn validate_production_query(query: &ProductionQuery) -> Result<()> {
    let expected_query_rows = expected_query_rows()?;
    if query.query_rows != expected_query_rows {
        return Err(invalid(format!(
            "production pg_stat_activity query returned {} rows, expected {expected_query_rows}",
            query.query_rows
        )));
    }
    if query.snapshot_units != 1 || query.snapshot_warnings != 0 || query.snapshot_damages != 0 {
        return Err(invalid(
            "production restart snapshot did not contain one clean sealed unit",
        ));
    }
    if query.query_has_next_cursor {
        return Err(invalid(
            "production query unexpectedly returned a continuation cursor",
        ));
    }
    Ok(())
}

fn expected_query_rows() -> Result<usize> {
    registry()
        .iter()
        .filter(|contract| contract.name == "pg_stat_activity")
        .count()
        .checked_mul(WINDOWS * ROWS_PER_WINDOW)
        .ok_or_else(|| invalid("production query row expectation overflowed"))
}

fn arguments() -> Result<Command> {
    let mut args = std::env::args_os();
    let _program = args.next();
    let command = args
        .next()
        .and_then(|value| value.into_string().ok())
        .ok_or_else(usage)?;
    let parsed = match command.as_str() {
        "prepare" => Command::Prepare {
            output_dir: PathBuf::from(args.next().ok_or_else(usage)?),
        },
        "seal" => Command::Seal {
            journal_path: PathBuf::from(args.next().ok_or_else(usage)?),
            output_dir: PathBuf::from(args.next().ok_or_else(usage)?),
            mode: parse_mode(args.next())?,
        },
        "read" => Command::Read {
            pgm_path: PathBuf::from(args.next().ok_or_else(usage)?),
            mode: parse_mode(args.next())?,
        },
        "query" => Command::Query {
            pgm_path: PathBuf::from(args.next().ok_or_else(usage)?),
        },
        _ => return Err(usage()),
    };
    if args.next().is_some() {
        return Err(usage());
    }
    Ok(parsed)
}

fn parse_mode(value: Option<std::ffi::OsString>) -> Result<Mode> {
    let value = value.ok_or_else(usage)?;
    let value = value
        .to_str()
        .ok_or_else(|| invalid("measurement mode is not UTF-8"))?;
    Mode::parse(value)
}

fn usage() -> AnyError {
    invalid(
        "usage: measure_coalesced_sections prepare OUTPUT_DIR | \
         seal ACTIVE_PARTS OUTPUT_DIR baseline|candidate | \
         read PGM_PATH baseline|candidate | query PGM_PATH",
    )
}

fn build_window(window: usize, logical: &mut LogicalData) -> Result<Vec<u8>> {
    let limits = DictLimits::new(32, 256)?;
    let mut dictionaries = SegmentDicts::new(limits);
    let _shared_value_id = dictionaries.intern(SHARED_DICTIONARY_VALUE)?;
    let mut value_ids = Vec::with_capacity(ROWS_PER_WINDOW);
    for value in 0..ROWS_PER_WINDOW / 2 {
        let short = format!("window-{window}-string-{value}");
        value_ids.push(dictionaries.intern(short.as_bytes())?.get());
        let blob = format!(
            "window-{window}-blob-{value}-abcdefghijklmnopqrstuvwxyz-ABCDEFGHIJKLMNOPQRSTUVWXYZ-0123456789"
        );
        value_ids.push(dictionaries.intern_blob(blob.as_bytes())?.get());
    }
    value_ids.sort_unstable();

    let mut sections = Vec::with_capacity(DATA_SECTIONS_PER_PART + DICTIONARY_SECTIONS_PER_PART);
    for contract in registry() {
        let batch = synthetic_batch(contract, window, &value_ids)?;
        let body = encode_window_body(&batch)?;
        logical.add_data_body(contract.type_id.get(), &body, crc32c(&body))?;
        sections.push(OwnedSection {
            type_id: contract.type_id.get(),
            rows: u32::try_from(batch.num_rows())?,
            body,
        });
    }
    sections.sort_unstable_by_key(|section| section.type_id);

    let dictionary_sections = dict::encode(&dictionaries)?;
    if dictionary_sections.len() != DICTIONARY_SECTIONS_PER_PART {
        return Err(invalid(
            "measurement window did not encode both dictionaries",
        ));
    }
    for section in dictionary_sections {
        logical.add_dictionary_body(section.type_id, &section.body, crc32c(&section.body))?;
        sections.push(OwnedSection {
            type_id: section.type_id,
            rows: section.rows,
            body: section.body,
        });
    }
    if sections.len() != DATA_SECTIONS_PER_PART + DICTIONARY_SECTIONS_PER_PART {
        return Err(invalid("measurement part has the wrong section count"));
    }

    let inputs = sections
        .iter()
        .map(|section| SectionInput {
            type_id: section.type_id,
            rows: section.rows,
            body: &section.body,
        })
        .collect::<Vec<_>>();
    let first_ts = window_first_ts(window)?;
    let last_ts = first_ts
        .checked_add(i64::try_from(ROWS_PER_WINDOW - 1)?)
        .ok_or_else(|| invalid("measurement timestamp overflowed"))?;
    Ok(build_part(
        &inputs,
        PartMeta {
            min_ts: first_ts,
            max_ts: last_ts,
        },
    ))
}

fn synthetic_batch(
    contract: &'static TypeContract,
    window: usize,
    value_ids: &[u64],
) -> Result<RecordBatch> {
    if value_ids.len() != ROWS_PER_WINDOW {
        return Err(invalid("synthetic dictionary id inventory is incomplete"));
    }
    let schema = arrow_schema(contract);
    let columns = contract
        .columns
        .iter()
        .enumerate()
        .map(|(column_index, column)| {
            synthetic_column(contract, column, column_index, window, value_ids)
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RecordBatch::try_new(Arc::clone(&schema), columns)?)
}

#[allow(
    clippy::too_many_lines,
    reason = "keeping the exhaustive ColumnType-to-Arrow fixture dispatch together makes schema coverage auditable"
)]
fn synthetic_column(
    contract: &TypeContract,
    column: &Column,
    column_index: usize,
    window: usize,
    value_ids: &[u64],
) -> Result<ArrayRef> {
    let is_sort_key = contract.sort_key.contains(&column.name);
    let nullable = |row: usize| {
        column.nullable && !is_sort_key && (row + column_index + window).is_multiple_of(3)
    };
    let ordinal = |row: usize| -> Result<u32> {
        let window_rows = window
            .checked_mul(ROWS_PER_WINDOW)
            .and_then(|value| value.checked_add(row))
            .ok_or_else(|| invalid("synthetic row ordinal overflowed"))?;
        u32::try_from(window_rows)?
            .checked_mul(256)
            .and_then(|value| value.checked_add(u32::try_from(column_index).ok()?))
            .ok_or_else(|| invalid("synthetic cell ordinal overflowed"))
    };

    let array: ArrayRef = match column.ty {
        ColumnType::I8 => Arc::new(Int8Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| optional(nullable(row), || Ok(i8::try_from(ordinal(row)? % 100)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::I16 => Arc::new(Int16Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| optional(nullable(row), || Ok(i16::try_from(ordinal(row)?)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::I32 => Arc::new(Int32Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| optional(nullable(row), || Ok(i32::try_from(ordinal(row)?)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::I64 => Arc::new(Int64Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| optional(nullable(row), || Ok(i64::from(ordinal(row)?))))
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::U8 => Arc::new(UInt8Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| optional(nullable(row), || Ok(u8::try_from(ordinal(row)? % 200)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::U16 => Arc::new(UInt16Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| optional(nullable(row), || Ok(u16::try_from(ordinal(row)?)?)))
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::U32 => Arc::new(UInt32Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| optional(nullable(row), || ordinal(row)))
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::U64 => Arc::new(UInt64Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| optional(nullable(row), || Ok(u64::from(ordinal(row)?))))
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::F32 => Arc::new(Float32Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| {
                    optional(nullable(row), || {
                        let ordinal = u16::try_from(ordinal(row)?)?;
                        if !is_sort_key && row == 0 {
                            Ok(-0.0_f32)
                        } else {
                            Ok(f32::from(ordinal) + 0.25)
                        }
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::F64 => Arc::new(Float64Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| {
                    optional(nullable(row), || {
                        if !is_sort_key && row == 0 {
                            Ok(-0.0_f64)
                        } else {
                            Ok(f64::from(ordinal(row)?) + 0.5)
                        }
                    })
                })
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::Bool => Arc::new(BooleanArray::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| (!nullable(row)).then_some(row >= ROWS_PER_WINDOW / 2))
                .collect::<Vec<_>>(),
        )),
        ColumnType::Ts => Arc::new(Int64Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| optional(nullable(row), || synthetic_ts(window, row)))
                .collect::<Result<Vec<_>>>()?,
        )),
        ColumnType::StrId => Arc::new(UInt64Array::from(
            (0..ROWS_PER_WINDOW)
                .map(|row| (!nullable(row)).then_some(value_ids[row]))
                .collect::<Vec<_>>(),
        )),
        ColumnType::ListI32 => {
            let item = Arc::new(Field::new("item", DataType::Int32, false));
            let mut builder = ListBuilder::new(Int32Builder::new()).with_field(item);
            for row in 0..ROWS_PER_WINDOW {
                if nullable(row) {
                    builder.append(false);
                } else {
                    builder.values().append_value(i32::try_from(ordinal(row)?)?);
                    builder.values().append_value(i32::try_from(column_index)?);
                    builder.append(true);
                }
            }
            Arc::new(builder.finish())
        }
    };
    Ok(array)
}

fn encode_window_body(batch: &RecordBatch) -> Result<Vec<u8>> {
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(3)?))
        .set_max_row_group_size(MAX_SECTION_ROWS)
        .set_created_by(String::new())
        .build();
    let options = ArrowWriterOptions::new()
        .with_properties(properties)
        .with_skip_arrow_metadata(true);
    let mut body = Vec::with_capacity(4096);
    let mut writer = ArrowWriter::try_new_with_options(&mut body, batch.schema(), options)?;
    writer.write(batch)?;
    writer.close()?;
    if body.len() > MAX_SECTION_BYTES {
        return Err(invalid("synthetic section crossed the production body cap"));
    }
    Ok(body)
}

fn inspect_output(path: &Path, mode: Mode) -> Result<OutputInspection> {
    let file = File::open(path)?;
    let file_len = file.metadata()?.len();
    let (catalog, catalog_at) = read_catalog(&file, file_len)?;
    if catalog.entries.len() != mode.expected_sections() {
        return Err(invalid("output catalog has the wrong section count"));
    }
    validate_catalog_inventory(&catalog, mode)?;

    let mut logical = LogicalData::default();
    let mut catalog_rows = 0_u64;
    let mut catalog_data_rows = 0_u64;
    let mut catalog_dictionary_rows = 0_u64;
    let mut expected_offset = 4_u64;
    for entry in &catalog.entries {
        let end = entry
            .offset
            .checked_add(entry.len)
            .ok_or_else(|| invalid("output section range overflowed"))?;
        if entry.flags != 0 {
            return Err(invalid("output catalog contains non-zero section flags"));
        }
        if entry.offset != expected_offset || end > catalog_at {
            return Err(invalid(
                "output sections are not contiguous before the catalog",
            ));
        }
        expected_offset = end;
        let len = usize::try_from(entry.len)?;
        if len > MAX_SECTION_BYTES {
            return Err(invalid("output section exceeds the production body cap"));
        }
        let mut body = vec![0_u8; len];
        file.read_exact_at(&mut body, entry.offset)?;
        let is_dictionary = matches!(entry.type_id, DICT_STRINGS_TYPE_ID | DICT_BLOBS_TYPE_ID);
        let decoded_rows = if is_dictionary {
            logical.add_dictionary_body(entry.type_id, &body, entry.crc32c)?
        } else {
            logical.add_data_body(entry.type_id, &body, entry.crc32c)?
        };
        if decoded_rows != usize::try_from(entry.rows)? {
            return Err(invalid(format!(
                "section {} decoded {decoded_rows} rows, but its catalog declares {}",
                entry.type_id, entry.rows
            )));
        }
        if is_dictionary {
            catalog_dictionary_rows = catalog_dictionary_rows
                .checked_add(u64::from(entry.rows))
                .ok_or_else(|| invalid("dictionary catalog row total overflowed"))?;
        } else {
            catalog_data_rows = catalog_data_rows
                .checked_add(u64::from(entry.rows))
                .ok_or_else(|| invalid("data catalog row total overflowed"))?;
        }
        catalog_rows = catalog_rows
            .checked_add(u64::from(entry.rows))
            .ok_or_else(|| invalid("catalog row total overflowed"))?;
    }
    if expected_offset != catalog_at {
        return Err(invalid(
            "output section bodies do not end exactly at the catalog",
        ));
    }
    if catalog_data_rows != u64::try_from(EXPECTED_DATA_ROWS)?
        || catalog_dictionary_rows != u64::try_from(mode.expected_dictionary_rows())?
    {
        return Err(invalid(format!(
            "catalog row categories are data={catalog_data_rows}, dictionary={catalog_dictionary_rows}; expected data={EXPECTED_DATA_ROWS}, dictionary={}",
            mode.expected_dictionary_rows()
        )));
    }
    Ok(OutputInspection {
        logical: logical.digest()?,
        catalog_sections: catalog.entries.len(),
        catalog_rows,
        catalog_data_rows,
        catalog_dictionary_rows,
    })
}

fn read_catalog(file: &File, file_len: u64) -> Result<(Catalog, u64)> {
    let tail_len = u64::try_from(TAIL_INDEX_LEN)?;
    let tail_at = file_len
        .checked_sub(tail_len)
        .ok_or_else(|| invalid("output PGM is shorter than its tail index"))?;
    let mut tail = [0_u8; TAIL_INDEX_LEN];
    file.read_exact_at(&mut tail, tail_at)?;
    let tail = TailIndex::decode(tail)?;
    let catalog_len = usize::try_from(tail.catalog_len)?;
    let maximum_catalog_len = MAX_OUTPUT_SECTIONS
        .checked_mul(kronika_format::ENTRY_LEN)
        .and_then(|bytes| bytes.checked_add(kronika_format::META_LEN))
        .ok_or_else(|| invalid("measurement catalog bound overflowed"))?;
    if catalog_len > maximum_catalog_len {
        return Err(invalid(
            "output catalog exceeds the measurement section bound",
        ));
    }
    let catalog_at = tail_at
        .checked_sub(u64::try_from(catalog_len)?)
        .ok_or_else(|| invalid("output catalog points before the file"))?;
    let mut bytes = vec![0_u8; catalog_len];
    file.read_exact_at(&mut bytes, catalog_at)?;
    Ok((Catalog::decode(&bytes)?, catalog_at))
}

fn validate_catalog_inventory(catalog: &Catalog, mode: Mode) -> Result<()> {
    let mut counts = BTreeMap::<u32, usize>::new();
    for entry in &catalog.entries {
        *counts.entry(entry.type_id).or_default() += 1;
    }
    let expected = mode.expected_multiplicity();
    for contract in registry() {
        let type_id = contract.type_id.get();
        if counts.remove(&type_id) != Some(expected) {
            return Err(invalid(format!(
                "type {type_id} does not occur {expected} time(s) in the output catalog"
            )));
        }
    }
    for type_id in [DICT_STRINGS_TYPE_ID, DICT_BLOBS_TYPE_ID] {
        if counts.remove(&type_id) != Some(expected) {
            return Err(invalid(format!(
                "dictionary type {type_id} does not occur {expected} time(s)"
            )));
        }
    }
    if !counts.is_empty() {
        return Err(invalid("output catalog contains unexpected section types"));
    }
    Ok(())
}

fn decode_dictionary_rows(type_id: u32, body: &[u8]) -> Result<Vec<(u64, Vec<u8>)>> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::copy_from_slice(body))?.build()?;
    let mut rows = Vec::new();
    for batch in reader {
        let batch = batch?;
        let ids = array::<UInt64Array>(&batch, "str_id")?;
        match type_id {
            DICT_STRINGS_TYPE_ID => {
                let bytes = array::<BinaryArray>(&batch, "bytes")?;
                for row in 0..batch.num_rows() {
                    if ids.is_null(row) || bytes.is_null(row) {
                        return Err(invalid("dict.strings contains a NULL"));
                    }
                    let id = ids.value(row);
                    let mut encoded = Vec::new();
                    append_bytes(&mut encoded, bytes.value(row))?;
                    rows.push((id, encoded));
                }
            }
            DICT_BLOBS_TYPE_ID => {
                let stored = array::<BinaryArray>(&batch, "stored_bytes")?;
                let full_len = array::<UInt64Array>(&batch, "full_len")?;
                let truncated = array::<BooleanArray>(&batch, "truncated")?;
                let sha = array::<FixedSizeBinaryArray>(&batch, "full_sha256")?;
                for row in 0..batch.num_rows() {
                    if ids.is_null(row)
                        || stored.is_null(row)
                        || full_len.is_null(row)
                        || truncated.is_null(row)
                    {
                        return Err(invalid("dict.blobs contains a NULL required value"));
                    }
                    let id = ids.value(row);
                    let mut encoded = Vec::new();
                    append_bytes(&mut encoded, stored.value(row))?;
                    encoded.extend_from_slice(&full_len.value(row).to_le_bytes());
                    encoded.push(u8::from(truncated.value(row)));
                    if sha.is_null(row) {
                        encoded.push(0);
                    } else {
                        encoded.push(1);
                        encoded.extend_from_slice(sha.value(row));
                    }
                    rows.push((id, encoded));
                }
            }
            _ => return Err(invalid("non-dictionary type passed to dictionary decoder")),
        }
    }
    Ok(rows)
}

fn array<'a, A: Array + 'static>(batch: &'a RecordBatch, name: &str) -> Result<&'a A> {
    batch
        .column_by_name(name)
        .and_then(|column| column.as_any().downcast_ref::<A>())
        .ok_or_else(|| invalid(format!("dictionary column {name:?} has the wrong type")))
}

fn encode_row(row: &Row) -> Result<Vec<u8>> {
    let contract = row.contract();
    if contract.columns.len() != row.cells().len() {
        return Err(invalid("decoded logical row does not match its contract"));
    }
    let mut out = Vec::new();
    for (column, cell) in contract.columns.iter().zip(row.cells()) {
        if matches!(cell, Cell::Null) {
            out.push(0);
            continue;
        }
        out.push(1);
        match (column.ty, cell) {
            (ColumnType::I8, Cell::I16(value)) => {
                out.extend_from_slice(&i8::try_from(*value)?.to_le_bytes());
            }
            (ColumnType::I16, Cell::I16(value)) => out.extend_from_slice(&value.to_le_bytes()),
            (ColumnType::I32, Cell::I32(value)) => out.extend_from_slice(&value.to_le_bytes()),
            (ColumnType::I64 | ColumnType::Ts, Cell::I64(value) | Cell::Ts(value)) => {
                out.extend_from_slice(&value.to_le_bytes());
            }
            (ColumnType::U8, Cell::U32(value)) => {
                out.extend_from_slice(&u8::try_from(*value)?.to_le_bytes());
            }
            (ColumnType::U16, Cell::U32(value)) => {
                out.extend_from_slice(&u16::try_from(*value)?.to_le_bytes());
            }
            (ColumnType::U32, Cell::U32(value)) => out.extend_from_slice(&value.to_le_bytes()),
            (ColumnType::U64 | ColumnType::StrId, Cell::U64(value) | Cell::StrId(value)) => {
                out.extend_from_slice(&value.to_le_bytes());
            }
            (ColumnType::F32, Cell::F64(value)) => {
                out.extend_from_slice(&(*value as f32).to_bits().to_le_bytes());
            }
            (ColumnType::F64, Cell::F64(value)) => {
                out.extend_from_slice(&value.to_bits().to_le_bytes());
            }
            (ColumnType::Bool, Cell::Bool(value)) => out.push(u8::from(*value)),
            (ColumnType::ListI32, Cell::ListI32(values)) => {
                append_len(&mut out, values.len())?;
                for value in values {
                    out.extend_from_slice(&value.to_le_bytes());
                }
            }
            _ => {
                return Err(invalid(format!(
                    "decoded cell for column {:?} has the wrong logical type",
                    column.name
                )));
            }
        }
    }
    Ok(out)
}

fn synthetic_ts(window: usize, row: usize) -> Result<i64> {
    window_first_ts(window)?
        .checked_add(i64::try_from(row)?)
        .ok_or_else(|| invalid("synthetic timestamp overflowed"))
}

fn optional<T>(is_null: bool, value: impl FnOnce() -> Result<T>) -> Result<Option<T>> {
    if is_null { Ok(None) } else { value().map(Some) }
}

fn window_first_ts(window: usize) -> Result<i64> {
    FIRST_TS_US
        .checked_add(
            i64::try_from(window)?
                .checked_mul(10_000)
                .ok_or_else(|| invalid("window timestamp overflowed"))?,
        )
        .ok_or_else(|| invalid("window timestamp overflowed"))
}

fn hash_field(hasher: &mut Sha256, bytes: &mut u64, field: &[u8]) -> Result<()> {
    let len = u64::try_from(field.len())?;
    hasher.update(len.to_le_bytes());
    hasher.update(field);
    *bytes = bytes
        .checked_add(8)
        .and_then(|value| value.checked_add(len))
        .ok_or_else(|| invalid("logical byte count overflowed"))?;
    Ok(())
}

fn hash_len(hasher: &mut Sha256, bytes: &mut u64, len: usize) -> Result<()> {
    hash_field(hasher, bytes, &u64::try_from(len)?.to_le_bytes())
}

fn append_bytes(out: &mut Vec<u8>, bytes: &[u8]) -> Result<()> {
    append_len(out, bytes.len())?;
    out.extend_from_slice(bytes);
    Ok(())
}

fn append_len(out: &mut Vec<u8>, len: usize) -> Result<()> {
    out.extend_from_slice(&u64::try_from(len)?.to_le_bytes());
    Ok(())
}

fn registry_type_id_digest() -> [u8; 32] {
    let mut hasher = Sha256::new();
    for contract in registry() {
        hasher.update(contract.type_id.get().to_le_bytes());
    }
    hasher.finalize().into()
}

fn sha256_file(path: &Path) -> Result<[u8; 32]> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

fn hex(bytes: [u8; 32]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        text.push(char::from(DIGITS[usize::from(byte >> 4)]));
        text.push(char::from(DIGITS[usize::from(byte & 0x0f)]));
    }
    text
}

fn invalid(message: impl Into<String>) -> AnyError {
    Box::new(io::Error::new(io::ErrorKind::InvalidData, message.into()))
}
