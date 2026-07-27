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

use kronika_format::{
    DictLimits, FRAME_HEADER_LEN, FrameHeader, JOURNAL_HEADER_LEN, JournalHeader, JournalState,
    MAGIC, PartMeta, SectionInput, build_part,
};
use kronika_layout::{
    DataRoot, LayoutLimits, QUARANTINE_DIRECTORY_NAME, SegmentAddress, SegmentId,
};
use kronika_registry::os_loadavg::OsLoadavg;
use kronika_registry::pg_stat_archiver::PgStatArchiver;
use kronika_registry::{MAX_DECODED_SECTION_BYTES, Section, StrId, Ts, parquet_decode_profile};
use kronika_writer::{Interner, Journal, JournalConfig, dict, seal};
use pg_kronika_dump::run;
use serde_json::Value;

const FIRST_SEGMENT: i64 = 1_753_500_000_000_001;
const SECOND_SEGMENT: i64 = FIRST_SEGMENT + 86_400_000_000;

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
