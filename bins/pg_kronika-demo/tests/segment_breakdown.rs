//! One-off forensic helper: prints the per-type byte breakdown of the demo
//! segments. Run manually:
//! `KRONIKA_BREAKDOWN_DIR=... cargo test -p pg_kronika-demo --test segment_breakdown -- --nocapture --ignored`

#![allow(
    unused_crate_dependencies,
    reason = "an integration test binary uses a subset of the package dependencies"
)]
#![allow(
    clippy::cast_precision_loss,
    reason = "byte counts far below 2^52 rendered as ratios in a research report"
)]

use std::collections::BTreeMap;

#[test]
#[ignore = "manual forensic tool, needs KRONIKA_BREAKDOWN_DIR"]
fn print_segment_breakdown() {
    let dir = std::env::var("KRONIKA_BREAKDOWN_DIR").expect("KRONIKA_BREAKDOWN_DIR");
    let mut per_type: BTreeMap<u32, (u64, u64, u32)> = BTreeMap::new(); // len, rows, sections
    let mut total_len = 0_u64;
    let mut file_len = 0_u64;

    for entry in std::fs::read_dir(&dir).expect("read dir") {
        let path = entry.expect("entry").path();
        if path.extension().is_none_or(|e| e != "pgm") {
            continue;
        }
        file_len += std::fs::metadata(&path).expect("stat").len();
        let file = std::fs::File::open(&path).expect("open pgm");
        let unit = kronika_reader::PgmUnit::open(file).expect("parse pgm");
        let catalog = unit.catalog();
        for entry in &catalog.entries {
            let slot = per_type.entry(entry.type_id).or_default();
            slot.0 += entry.len;
            slot.1 += u64::from(entry.rows);
            slot.2 += 1;
            total_len += entry.len;
        }
    }

    let mut rows: Vec<(u32, (u64, u64, u32))> = per_type.into_iter().collect();
    rows.sort_by_key(|(_, (len, _, _))| std::cmp::Reverse(*len));
    println!("== per-type breakdown (all segments) ==");
    println!(
        "{:>12} {:>12} {:>10} {:>9}  type_id",
        "bytes", "rows", "sections", "b/row"
    );
    for (type_id, (len, row_count, sections)) in rows.iter().take(25) {
        let per_row = if *row_count > 0 {
            *len as f64 / *row_count as f64
        } else {
            0.0
        };
        println!("{len:>12} {row_count:>12} {sections:>10} {per_row:>9.1}  {type_id}");
    }
    println!("section bodies total: {total_len}");
    println!("file bytes total:     {file_len}");
    println!("overhead (file - bodies): {}", file_len - total_len);
}
