//! End-to-end contracts for the three dump input modes.
#![allow(
    unused_crate_dependencies,
    reason = "the integration target consumes pg_kronika_dump; remaining package dependencies belong to the library"
)]

use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use kronika_analytics::overview::SegmentIdentity;
use kronika_format::{
    DictLimits, FRAME_HEADER_LEN, FrameHeader, JOURNAL_HEADER_LEN, JournalHeader, JournalState,
    MAGIC, PartMeta, SectionInput, build_part, crc32c,
};
use kronika_layout::{
    DataRoot, LayoutLimits, QUARANTINE_DIRECTORY_NAME, SegmentAddress, SegmentId,
};
use kronika_reader::{
    BlockContent, CatalogEntryDescriptor, EntityDictionaryEntry, EntityMetric, EntitySeries,
    EntitySeriesBlock, FactFile, HeaderIdentity, IndexStatus, LIMIT, METRIC_FLAG_CANONICAL,
    ManifestEntryDescriptor, MetricAggregation, MetricStatus, SourceDescriptor,
    SourceManifestBlock, TimeGrid, UiSummaryBlock, ViewSummary,
};
use kronika_registry::os_loadavg::OsLoadavg;
use kronika_registry::pg_stat_archiver::PgStatArchiver;
use kronika_registry::{MAX_DECODED_SECTION_BYTES, Section, StrId, Ts, parquet_decode_profile};
use kronika_writer::{Interner, Journal, JournalConfig, dict, seal};
use pg_kronika_dump::run;
use serde_json::Value;

const FIRST_SEGMENT: i64 = 1_753_500_000_000_001;
const SECOND_SEGMENT: i64 = FIRST_SEGMENT + 86_400_000_000;
const OVF_HEADER_LEN: usize = 192;
const OVF_DIRECTORY_ENTRY_LEN: usize = 64;
const OVF_DIRECTORY_COUNT_OFFSET: usize = 168;
const OVF_DIRECTORY_CRC_OFFSET: usize = 184;
const OVF_HEADER_CRC_OFFSET: usize = 188;

fn loadavg_body(rows: &[i64]) -> Vec<u8> {
    OsLoadavg::encode(
        &rows
            .iter()
            .enumerate()
            .map(|(index, &ts)| {
                let ordinal =
                    u32::try_from(index).expect("fixture row count fits losslessly in f64");
                OsLoadavg {
                    ts: Ts(ts),
                    load1: f64::from(ordinal) + 0.25,
                    load5: f64::from(ordinal) + 0.5,
                    load15: f64::from(ordinal) + 0.75,
                    running: i32::try_from(index + 1).expect("small fixture"),
                    total: 100,
                    scope: 0,
                }
            })
            .collect::<Vec<_>>(),
    )
    .expect("encode loadavg")
}

fn part_with_loadavg(min_ts: i64, max_ts: i64, rows: &[i64]) -> (Vec<u8>, Vec<u8>) {
    let body = loadavg_body(rows);
    let part = build_part(
        &[SectionInput {
            type_id: OsLoadavg::CONTRACT.type_id.get(),
            rows: u32::try_from(rows.len()).expect("small fixture"),
            body: &body,
        }],
        PartMeta {
            min_ts,
            max_ts,
            source_id: 7,
        },
    );
    (part, body)
}

fn ovf_fixture() -> Vec<u8> {
    const BUCKET_US: i64 = 60_000_000;

    let descriptor = SourceDescriptor([0x22; 32]);
    let lineage = SegmentIdentity::sealed(7, descriptor.0);
    let identity = HeaderIdentity::from_current_contract(
        kronika_format::FORMAT_VERSION,
        7,
        0,
        BUCKET_US,
        4_096,
        descriptor,
        lineage.id(),
    );
    let manifest = SourceManifestBlock::new(
        7,
        kronika_format::FORMAT_VERSION,
        0,
        BUCKET_US,
        4_096,
        vec![ManifestEntryDescriptor {
            catalog: CatalogEntryDescriptor {
                type_id: OsLoadavg::CONTRACT.type_id.get(),
                flags: 0,
                body_len: 128,
                rows: 3,
                body_crc32c: 0x1234_5678,
            },
            section_body_id: None,
        }],
        &LIMIT,
    )
    .expect("OVF manifest");
    let grid = TimeGrid::for_range(0, BUCKET_US).expect("two-bucket grid");
    let summary = UiSummaryBlock::new(
        grid,
        vec![0, BUCKET_US],
        vec![
            ViewSummary::new(
                1,
                1,
                IndexStatus::Complete,
                vec![0b11],
                vec![0b10],
                vec![2, 2],
                &LIMIT,
            )
            .expect("summary view"),
        ],
        &LIMIT,
    )
    .expect("summary");
    let dictionary = vec![
        EntityDictionaryEntry::new(vec![1, 2], "backend 42".to_owned(), &LIMIT)
            .expect("first entity"),
        EntityDictionaryEntry::new(vec![3, 4], "backend 43".to_owned(), &LIMIT)
            .expect("second entity"),
    ];
    let metric = EntityMetric::new(
        1,
        1,
        METRIC_FLAG_CANONICAL,
        1,
        MetricAggregation::Sum,
        MetricStatus::Complete,
        0.0,
        vec![
            EntitySeries::new(0, 0.0, 0.0, vec![0b10], vec![0], &LIMIT).expect("missing then zero"),
            EntitySeries::new(1, 0.0, 0.0, vec![0b01], vec![0], &LIMIT).expect("zero then missing"),
        ],
        &LIMIT,
    )
    .expect("metric");
    let series = EntitySeriesBlock::new(
        1,
        1,
        1,
        IndexStatus::Complete,
        (0, BUCKET_US),
        grid,
        vec![0b11],
        dictionary,
        vec![metric],
        &LIMIT,
    )
    .expect("entity series");
    FactFile::build(
        &identity,
        vec![
            BlockContent::SourceManifest(Box::new(manifest)),
            BlockContent::UiSummary(Box::new(summary)),
            BlockContent::EntitySeries(Box::new(series)),
        ],
        &LIMIT,
    )
    .expect("build OVF fixture")
}

fn ovf_entry_offset(bytes: &[u8], kind_code: u32) -> usize {
    let count = u32::from_le_bytes(
        bytes[OVF_DIRECTORY_COUNT_OFFSET..OVF_DIRECTORY_COUNT_OFFSET + 4]
            .try_into()
            .expect("directory count"),
    );
    (0..usize::try_from(count).expect("small directory"))
        .map(|index| OVF_HEADER_LEN + index * OVF_DIRECTORY_ENTRY_LEN)
        .find(|offset| {
            u32::from_le_bytes(bytes[*offset..*offset + 4].try_into().expect("block kind"))
                == kind_code
        })
        .expect("directory block")
}

fn reseal_ovf_metadata(bytes: &mut [u8]) {
    let count = u32::from_le_bytes(
        bytes[OVF_DIRECTORY_COUNT_OFFSET..OVF_DIRECTORY_COUNT_OFFSET + 4]
            .try_into()
            .expect("directory count"),
    );
    let directory_end =
        OVF_HEADER_LEN + usize::try_from(count).expect("small directory") * OVF_DIRECTORY_ENTRY_LEN;
    let directory_crc = crc32c(&bytes[OVF_HEADER_LEN..directory_end]);
    bytes[OVF_DIRECTORY_CRC_OFFSET..OVF_DIRECTORY_CRC_OFFSET + 4]
        .copy_from_slice(&directory_crc.to_le_bytes());
    bytes[OVF_HEADER_CRC_OFFSET..OVF_HEADER_LEN].fill(0);
    let header_crc = crc32c(&bytes[..OVF_HEADER_LEN]);
    bytes[OVF_HEADER_CRC_OFFSET..OVF_HEADER_LEN].copy_from_slice(&header_crc.to_le_bytes());
}

fn run_json(arguments: impl IntoIterator<Item = OsString>) -> (ExitCode, Value, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(arguments, &mut stdout, &mut stderr);
    let output = serde_json::from_slice(&stdout).expect("stdout is one JSON object");
    (
        status,
        output,
        String::from_utf8(stderr).expect("diagnostic is UTF-8"),
    )
}

fn run_raw(arguments: impl IntoIterator<Item = OsString>) -> (ExitCode, Vec<u8>, String) {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(arguments, &mut stdout, &mut stderr);
    (
        status,
        stdout,
        String::from_utf8(stderr).expect("diagnostic is UTF-8"),
    )
}

fn write_segment(root: &Path, id: i64, bytes: &[u8]) -> PathBuf {
    let address = SegmentAddress::new(SegmentId::new(id).expect("segment id")).expect("address");
    let day = root
        .join(address.day.year_component())
        .join(address.day.month_component())
        .join(address.day.day_component());
    fs::create_dir_all(&day).expect("create UTC tree");
    let path = day.join(address.pgm_name());
    fs::write(&path, bytes).expect("write PGM");
    path
}

#[test]
fn ovf_name_selects_metadata_dump_without_reading_bodies() {
    let directory = tempfile::tempdir().expect("tempdir");
    let bytes = ovf_fixture();
    let path = directory.path().join("segment.ovf");
    fs::write(&path, &bytes).expect("write OVF");

    let (status, output, stderr) = run_json([path.into_os_string()]);

    assert_eq!(status, ExitCode::SUCCESS, "{stderr}");
    assert!(stderr.is_empty());
    assert_eq!(output["kind"], "ovf");
    assert_eq!(output["file_bytes"], bytes.len());
    assert_eq!(output["header"]["pgm_source_id"], 7);
    assert!(
        output["blocks"]
            .as_array()
            .is_some_and(|blocks| !blocks.is_empty())
    );
    assert!(
        output["blocks"]
            .as_array()
            .expect("blocks")
            .iter()
            .all(|block| block.get("content").is_none())
    );
}

#[test]
fn file_mode_is_selected_by_name_without_magic_fallback() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (pgm, _body) = part_with_loadavg(10, 10, &[10]);
    let pgm_as_ovf = directory.path().join("renamed.ovf");
    fs::write(&pgm_as_ovf, pgm).expect("write renamed PGM");
    let (status, stdout, _stderr) = run_raw([pgm_as_ovf.into_os_string()]);
    assert_eq!(status, ExitCode::from(1));
    assert!(stdout.is_empty());

    let ovf_as_journal = directory.path().join("renamed.parts");
    fs::write(&ovf_as_journal, ovf_fixture()).expect("write renamed OVF");
    let (status, output, stderr) = run_json([ovf_as_journal.into_os_string()]);
    assert_eq!(status, ExitCode::SUCCESS, "{stderr}");
    assert_eq!(output["kind"], "journal");
}

#[test]
fn ovf_rows_decodes_web_index_and_preserves_missing_zero() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("segment.ovf");
    fs::write(&path, ovf_fixture()).expect("write OVF");

    let (status, output, stderr) = run_json([path.into_os_string(), OsString::from("--rows")]);

    assert_eq!(status, ExitCode::SUCCESS, "{stderr}");
    let blocks = output["blocks"].as_array().expect("blocks");
    let summary = &blocks
        .iter()
        .find(|block| block["kind"] == "ui_summary")
        .expect("summary block")["content"];
    assert_eq!(summary["grid"]["bucket_count"], 2);
    assert_eq!(
        summary["snapshot_times_us"],
        serde_json::json!([0, 60_000_000])
    );
    assert_eq!(
        summary["views"][0]["populations"],
        serde_json::json!([2, 2])
    );
    assert_eq!(
        summary["views"][0]["notable"],
        serde_json::json!([false, true])
    );

    let content = &blocks
        .iter()
        .find(|block| block["kind"] == "entity_series")
        .expect("entity-series block")["content"];
    assert_eq!(content["coverage"], serde_json::json!([true, true]));
    assert_eq!(content["dictionary"][0]["key"], "0102");
    let series = &content["metrics"][0]["series"][0];
    assert!(series["values"][0].is_null());
    assert_eq!(series["values"][1], 0.0);
    assert_eq!(series["key"], "0102");
    assert_eq!(series["label"], "backend 42");
}

#[test]
fn ovf_rows_limit_truncates_each_metric_but_keeps_dictionary() {
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("segment.ovf");
    fs::write(&path, ovf_fixture()).expect("write OVF");

    let (status, output, stderr) = run_json([
        path.into_os_string(),
        OsString::from("--rows"),
        OsString::from("--limit=1"),
    ]);

    assert_eq!(status, ExitCode::SUCCESS, "{stderr}");
    let content = &output["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .find(|block| block["kind"] == "entity_series")
        .expect("entity-series block")["content"];
    assert_eq!(
        content["dictionary"].as_array().expect("dictionary").len(),
        2
    );
    assert_eq!(
        content["metrics"][0]["series"]
            .as_array()
            .expect("series")
            .len(),
        1
    );
    assert_eq!(content["metrics"][0]["truncated"], true);
}

#[test]
fn ovf_metadata_keeps_unknown_optional_blocks_without_reading_them() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut bytes = ovf_fixture();
    let entry = ovf_entry_offset(&bytes, 11);
    bytes[entry..entry + 4].copy_from_slice(&99_u32.to_le_bytes());
    let flags =
        u16::from_le_bytes(bytes[entry + 6..entry + 8].try_into().expect("block flags")) & !1;
    bytes[entry + 6..entry + 8].copy_from_slice(&flags.to_le_bytes());
    reseal_ovf_metadata(&mut bytes);
    let path = directory.path().join("segment.ovf");
    fs::write(&path, bytes).expect("write OVF");

    let (status, output, stderr) = run_json([path.into_os_string()]);

    assert_eq!(status, ExitCode::SUCCESS, "{stderr}");
    let unknown = output["blocks"]
        .as_array()
        .expect("blocks")
        .iter()
        .find(|block| block["kind_code"] == 99)
        .expect("unknown block");
    assert_eq!(unknown["kind"], Value::Null);
    assert!(unknown.get("content").is_none());
}

#[test]
fn ovf_metadata_defers_body_crc_until_rows_reads_the_index() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut bytes = ovf_fixture();
    let entry = ovf_entry_offset(&bytes, 11);
    let body_offset = u64::from_le_bytes(
        bytes[entry + 16..entry + 24]
            .try_into()
            .expect("body offset"),
    );
    bytes[usize::try_from(body_offset).expect("small body offset")] ^= 1;
    let path = directory.path().join("segment.ovf");
    fs::write(&path, bytes).expect("write OVF");

    let (status, output, stderr) = run_json([path.clone().into_os_string()]);
    assert_eq!(status, ExitCode::SUCCESS, "{stderr}");
    assert_eq!(output["kind"], "ovf");

    let (status, stdout, stderr) = run_raw([path.into_os_string(), OsString::from("--rows")]);
    assert_eq!(status, ExitCode::from(1));
    assert!(stdout.is_empty());
    assert!(stderr.contains("entity_series"));
}

#[test]
#[allow(
    clippy::cast_precision_loss,
    reason = "the fixture sizes are tiny and converted only to compare a descriptive JSON ratio"
)]
fn pgm_reports_exact_sizes_and_limits_rows() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (part, body) = part_with_loadavg(10, 30, &[10, 20, 30]);
    let path = directory.path().join(format!("{FIRST_SEGMENT}.pgm"));
    fs::write(&path, &part).expect("write PGM");
    let profile =
        parquet_decode_profile(&body, MAX_DECODED_SECTION_BYTES).expect("inspect fixture profile");

    let (status, output, stderr) = run_json([
        path.into_os_string(),
        OsString::from("--rows"),
        OsString::from("--limit"),
        OsString::from("2"),
    ]);

    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(output["kind"], "pgm");
    assert_eq!(output["segment_id"], FIRST_SEGMENT);
    assert_eq!(output["file_bytes"], part.len());
    assert_eq!(output["windows"]["count"], 1);
    assert_eq!(output["windows"]["first_us"], 10);
    assert_eq!(output["windows"]["last_us"], 30);
    assert_eq!(output["sections"][0]["type_id"], "S_105_001");
    assert_eq!(output["sections"][0]["type_name"], "os_loadavg");
    assert_eq!(output["sections"][0]["rows"], 3);
    assert_eq!(output["sections"][0]["stored_bytes"], body.len());
    assert_eq!(
        output["sections"][0]["decoded_bytes"],
        profile.decoded_bytes
    );
    assert_eq!(
        output["sections"][0]["rows_data"]
            .as_array()
            .expect("rows array")
            .len(),
        2
    );
    assert_eq!(output["sections"][0]["rows_data"][0]["ts"], 10);
    assert_eq!(output["sections"][0]["rows_data"][1]["ts"], 20);
    assert_eq!(output["sections"][0]["truncated"], true);
    let expected_ratio = profile.decoded_bytes as f64 / body.len() as f64;
    let actual_ratio = output["sections"][0]["ratio"]
        .as_f64()
        .expect("numeric ratio");
    assert!((actual_ratio - expected_ratio).abs() < f64::EPSILON);
}

#[test]
fn sealed_two_window_segment_reports_two_windows() {
    let directory = tempfile::tempdir().expect("tempdir");
    let root = DataRoot::open(directory.path()).expect("open root");
    let owner = root
        .acquire_writer(LayoutLimits::default())
        .expect("acquire writer");
    let address =
        SegmentAddress::new(SegmentId::new(FIRST_SEGMENT).expect("segment id")).expect("address");
    let mut journal = Journal::open(&owner, JournalConfig::default()).expect("open journal");
    for ts in [FIRST_SEGMENT, FIRST_SEGMENT + 1_000_000] {
        let (part, _body) = part_with_loadavg(ts, ts, &[ts]);
        journal
            .append(address.id, &part)
            .expect("append collection window");
    }
    seal(&journal, &owner, address).expect("seal two windows");
    let path = root.diagnostic_file_path(address, kronika_layout::FileKind::Pgm);

    let (status, output, stderr) = run_json([path.into_os_string()]);

    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(output["windows"]["count"], 2);
    assert_eq!(output["sections"][0]["rows"], 2);
}

#[test]
fn unknown_type_keeps_metadata_and_skips_rows() {
    let directory = tempfile::tempdir().expect("tempdir");
    let body = loadavg_body(&[10]);
    let part = build_part(
        &[SectionInput {
            type_id: 1_999_999,
            rows: 1,
            body: &body,
        }],
        PartMeta {
            min_ts: 10,
            max_ts: 10,
            source_id: 7,
        },
    );
    let path = directory.path().join(format!("{FIRST_SEGMENT}.pgm"));
    fs::write(&path, part).expect("write unknown-type PGM");

    let (status, output, stderr) = run_json([path.into_os_string(), OsString::from("--rows")]);

    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(output["sections"][0]["type_id"], "S_999_999");
    assert_eq!(output["sections"][0]["type_name"], Value::Null);
    assert_eq!(output["sections"][0]["rows_skipped"], "unknown_type");
    assert!(output["sections"][0].get("rows_data").is_none());
}

#[test]
fn rows_resolve_segment_dictionary_values() {
    let directory = tempfile::tempdir().expect("tempdir");
    let mut interner = Interner::new(DictLimits::new(256, 1 << 20).expect("dictionary limits"));
    let wal_name = "000000010000000000000001";
    let wal_id = interner.intern(wal_name.as_bytes()).expect("intern WAL");
    let data = PgStatArchiver::encode(&[PgStatArchiver {
        ts: Ts(1_000),
        archived_count: 5,
        last_archived_wal: Some(StrId(wal_id.get())),
        last_archived_time: Some(Ts(900)),
        failed_count: 0,
        last_failed_wal: None,
        last_failed_time: None,
        stats_reset: None,
    }])
    .expect("encode archiver");
    let dictionary = dict::encode(interner.window()).expect("encode dictionary");
    let mut sections = vec![SectionInput {
        type_id: PgStatArchiver::CONTRACT.type_id.get(),
        rows: 1,
        body: &data,
    }];
    sections.extend(dictionary.iter().map(|section| SectionInput {
        type_id: section.type_id,
        rows: section.rows,
        body: &section.body,
    }));
    let part = build_part(
        &sections,
        PartMeta {
            min_ts: 1_000,
            max_ts: 1_000,
            source_id: 7,
        },
    );
    let path = directory.path().join(format!("{FIRST_SEGMENT}.pgm"));
    fs::write(&path, part).expect("write PGM");

    let (status, output, stderr) = run_json([path.into_os_string(), OsString::from("--rows")]);

    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(output["dictionary"]["entries"], 1);
    assert_eq!(
        output["sections"][0]["rows_data"][0]["last_archived_wal"],
        wal_name
    );
    assert_eq!(
        output["sections"][0]["rows_data"][0]["last_failed_wal"],
        Value::Null
    );
}

#[test]
fn intact_journal_reports_full_prefix_and_limited_rows() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (part, _body) = part_with_loadavg(10, 20, &[10, 20]);
    let physical_body_len = FRAME_HEADER_LEN + part.len();
    let mut journal = Vec::new();
    journal.extend_from_slice(
        &JournalHeader {
            state: JournalState::Active {
                segment_id: FIRST_SEGMENT,
            },
            body_len: u64::try_from(physical_body_len).expect("fixture length"),
        }
        .encode(),
    );
    journal.extend_from_slice(
        &FrameHeader {
            part_len: u64::try_from(part.len()).expect("fixture length"),
        }
        .encode(),
    );
    journal.extend_from_slice(&part);
    let path = directory.path().join("active.parts");
    fs::write(&path, &journal).expect("write journal");

    let (status, output, stderr) = run_json([
        path.into_os_string(),
        OsString::from("--rows"),
        OsString::from("--limit=1"),
    ]);

    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(output["header"]["state"], "active");
    assert_eq!(output["header"]["segment_id"], FIRST_SEGMENT);
    assert_eq!(output["valid_prefix_bytes"], journal.len());
    assert_eq!(output["damage"], Value::Null);
    assert_eq!(output["recoverable"]["frames"], 1);
    assert_eq!(output["recoverable"]["windows"], 1);
    assert_eq!(
        output["frames"][0]["sections"][0]["rows_data"]
            .as_array()
            .expect("rows array")
            .len(),
        1
    );
    assert_eq!(output["frames"][0]["sections"][0]["truncated"], true);
}

#[test]
fn damaged_journal_is_a_successful_forensic_result() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (part, _body) = part_with_loadavg(10, 20, &[10]);
    let complete_frame_bytes = FRAME_HEADER_LEN + part.len();
    let torn = [0x50_u8, 0x47, 0x4d];
    let physical_body_len = complete_frame_bytes + torn.len();
    let mut journal = Vec::new();
    journal.extend_from_slice(
        &JournalHeader {
            state: JournalState::Active {
                segment_id: FIRST_SEGMENT,
            },
            body_len: u64::try_from(physical_body_len).expect("fixture length"),
        }
        .encode(),
    );
    journal.extend_from_slice(
        &FrameHeader {
            part_len: u64::try_from(part.len()).expect("fixture length"),
        }
        .encode(),
    );
    journal.extend_from_slice(&part);
    journal.extend_from_slice(&torn);
    let path = directory.path().join("active.parts");
    fs::write(&path, &journal).expect("write journal");

    let (status, output, stderr) = run_json([path.into_os_string()]);

    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(output["kind"], "journal");
    assert_eq!(output["header"]["state"], "active");
    assert_eq!(output["recoverable"]["frames"], 1);
    assert_eq!(output["recoverable"]["windows"], 1);
    assert_eq!(output["frames"][0]["offset"], JOURNAL_HEADER_LEN);
    assert_eq!(output["frames"][0]["part_bytes"], part.len());
    assert_eq!(
        output["valid_prefix_bytes"],
        JOURNAL_HEADER_LEN + complete_frame_bytes
    );
    assert_eq!(output["damage"]["kind"], "torn_frame");
    assert_eq!(
        output["damage"]["offset"],
        JOURNAL_HEADER_LEN + complete_frame_bytes
    );
}

#[test]
fn tree_reads_only_catalogs_and_lists_days_and_quarantine() {
    let directory = tempfile::tempdir().expect("tempdir");
    let (mut first, first_body) = part_with_loadavg(FIRST_SEGMENT, FIRST_SEGMENT, &[FIRST_SEGMENT]);
    let (second, second_body) =
        part_with_loadavg(SECOND_SEGMENT, SECOND_SEGMENT, &[SECOND_SEGMENT]);
    // Tree inventory must not touch section bodies; single-PGM mode owns that work.
    first[MAGIC.len()] ^= 0xff;
    write_segment(directory.path(), FIRST_SEGMENT, &first);
    write_segment(directory.path(), SECOND_SEGMENT, &second);
    fs::write(
        directory.path().join("active.parts"),
        JournalHeader::EMPTY.encode(),
    )
    .expect("write empty journal");

    let quarantine = directory.path().join(QUARANTINE_DIRECTORY_NAME);
    fs::create_dir(&quarantine).expect("create quarantine");
    let provisional = quarantine.join("provisional");
    fs::write(&provisional, b"exact evidence").expect("write evidence");
    let metadata = fs::symlink_metadata(&provisional).expect("stat evidence");
    let evidence_name = format!(
        "qv1-03-0000000000000000-{:016x}-{:016x}-00",
        metadata.dev(),
        metadata.ino()
    );
    fs::rename(&provisional, quarantine.join(&evidence_name)).expect("name evidence");

    let (status, output, stderr) = run_json([directory.path().as_os_str().to_owned()]);

    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(output["kind"], "tree");
    assert!(output.get("scope").is_none());
    assert_eq!(output["journal"]["state"], "empty");
    assert_eq!(output["journal"]["frames"], 0);
    assert_eq!(output["quarantine"][0]["id"], evidence_name);
    assert_eq!(output["quarantine"][0]["reason"], "corrupt_active_journal");
    assert_eq!(output["quarantine"][0]["bytes"], 14);
    assert_eq!(output["quarantine"][0]["file_type"], "regular_file");
    assert_eq!(output["days"].as_array().expect("days").len(), 2);
    assert_eq!(
        output["days"][0]["segments"][0]["segment_id"],
        FIRST_SEGMENT
    );
    assert_eq!(
        output["days"][1]["segments"][0]["segment_id"],
        SECOND_SEGMENT
    );
    assert_eq!(output["totals"]["segments"], 2);
    assert_eq!(output["totals"]["pgm_bytes"], first.len() + second.len());
    assert_eq!(
        output["totals"]["stored_bytes"],
        first_body.len() + second_body.len()
    );
    assert_eq!(output["totals"]["decoded_bytes"], Value::Null);
    assert_eq!(output["totals"]["ratio"], Value::Null);

    let first_address =
        SegmentAddress::new(SegmentId::new(FIRST_SEGMENT).expect("segment id")).expect("address");
    let year = first_address.day.year_component();
    let month = first_address.day.month_component();
    let day = first_address.day.day_component();

    let (status, day_output, stderr) = run_json([directory
        .path()
        .join(&year)
        .join(&month)
        .join(&day)
        .into_os_string()]);
    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(day_output["root"], directory.path().display().to_string());
    assert_eq!(day_output["scope"], first_address.day.to_string());
    assert_eq!(day_output["days"].as_array().expect("days").len(), 1);
    assert_eq!(day_output["totals"]["segments"], 1);

    let (status, month_output, stderr) =
        run_json([directory.path().join(&year).join(&month).into_os_string()]);
    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(month_output["scope"], format!("{year}/{month}"));
    assert_eq!(month_output["days"].as_array().expect("days").len(), 2);
    assert_eq!(month_output["totals"]["segments"], 2);

    let (status, year_output, stderr) = run_json([directory.path().join(&year).into_os_string()]);
    assert_eq!(status, ExitCode::SUCCESS);
    assert!(stderr.is_empty());
    assert_eq!(year_output["scope"], year);
    assert_eq!(year_output["days"].as_array().expect("days").len(), 2);
    assert_eq!(year_output["totals"]["segments"], 2);
}

#[test]
fn usage_errors_write_no_json_and_exit_two() {
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let status = run(
        [OsString::from("data"), OsString::from("--limit=5")],
        &mut stdout,
        &mut stderr,
    );

    assert_eq!(status, ExitCode::from(2));
    assert!(stdout.is_empty());
    let diagnostic = String::from_utf8(stderr).expect("diagnostic is UTF-8");
    assert!(diagnostic.contains("--limit requires --rows"));
    assert!(diagnostic.contains("usage: pg_kronika-dump"));
}
