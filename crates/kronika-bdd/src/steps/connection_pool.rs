//! Steps for `features/connection_pool.feature`.
//!
//! `pg_stat_user_tables` rows are collected only through the pool's
//! per-database connections, and each sealed row carries the datname of the
//! connection that collected it. Resolving that datname through the segment
//! dictionary makes a row direct evidence the pool reached its database.

use anyhow::{Result, bail};
use cucumber::{gherkin::Step, given, then};
use kronika_reader::{Dictionary, Resolved};
use kronika_registry::{Cell, Row};

use crate::BddWorld;
use crate::harness::assert_row::decode_section;
use crate::harness::dump;
use crate::harness::session::Session;
use crate::steps::docstring;

/// Create a second isolated database on the scenario's cluster and seed it
/// with the docstring SQL, giving the pool more than one target to cover.
#[given("an extra pool-target database seeded with:")]
async fn extra_pool_target_database(world: &mut BddWorld, step: &Step) -> Result<()> {
    let sql = docstring(step)?;
    let dsn = world.harness.create_extra_database("pool_extra").await?;
    let session = Session::open(&dsn, sql).await?;
    session.close().await?;
    Ok(())
}

/// Assert the section holds exactly one row whose `relname` and `datname`
/// resolve through the segment dictionary to the given table and database.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(
    regex = r#"^section (\d+) has one row for table "([^"]+)" attributed to the (scenario|extra pool-target) database$"#
)]
fn section_row_attributed_to_database(
    world: &mut BddWorld,
    type_id: u32,
    relname: String,
    which: String,
) -> Result<()> {
    let datname = match which.as_str() {
        "scenario" => world.harness.database()?.to_owned(),
        _ => world.harness.extra_database()?.to_owned(),
    };
    let segment = world.harness.segment()?.clone();
    let subprocess_logs = world.harness.failure_log()?;
    let (rows, dict) = decode_section(&segment, type_id)?;
    let matched = count_attributed_rows(&rows, &dict, &relname, &datname);
    if matched == 1 {
        return Ok(());
    }
    bail!(
        "{}",
        dump::section_dump(
            &format!(
                "section {type_id}: expected one row with relname {relname:?} \
                 and datname {datname:?}, found {matched}"
            ),
            &rows,
            &subprocess_logs,
            &[(
                "expected",
                format!("relname = {relname:?}\ndatname = {datname:?}"),
            )],
        )
    )
}

/// Count rows whose `relname` and `datname` resolve to the wanted pair.
fn count_attributed_rows(rows: &[Row], dict: &Dictionary, relname: &str, datname: &str) -> usize {
    rows.iter()
        .filter(|row| {
            str_cell_is(row, "relname", relname, dict) && str_cell_is(row, "datname", datname, dict)
        })
        .count()
}

/// Whether the row's `column` is a `StrId` that resolves through `dict` to `want`.
fn str_cell_is(row: &Row, column: &str, want: &str, dict: &Dictionary) -> bool {
    match row.get(column) {
        Some(Cell::StrId(id)) => matches!(
            dict.resolve(*id),
            Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. })
                if bytes == want.as_bytes()
        ),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{count_attributed_rows, str_cell_is};
    use kronika_format::{DictLimits, PartMeta, SectionInput, build_part};
    use kronika_reader::{Dictionary, Segment};
    use kronika_registry::{Cell, Row};

    /// A real segment dictionary resolving the given strings, with their ids.
    fn dictionary_of(values: &[&str]) -> (Dictionary, Vec<u64>) {
        let limits = DictLimits::new(1 << 10, 1 << 20).expect("limits");
        let mut interner = kronika_writer::Interner::new(limits);
        let ids: Vec<u64> = values
            .iter()
            .map(|v| interner.intern(v.as_bytes()).expect("intern").get())
            .collect();

        let dict_sections = kronika_writer::dict::encode(interner.window()).expect("encode");
        let section_inputs: Vec<_> = dict_sections
            .iter()
            .map(|s| SectionInput {
                type_id: s.type_id,
                rows: s.rows,
                body: &s.body,
            })
            .collect();
        let bytes = build_part(
            &section_inputs,
            PartMeta {
                min_ts: 0,
                max_ts: 0,
                source_id: 0,
            },
        );

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("dict.pgm");
        std::fs::write(&path, &bytes).expect("write segment");
        let dict = Segment::open(&path)
            .expect("open segment")
            .dictionary()
            .expect("read dictionary");
        (dict, ids)
    }

    fn table_row(relname_id: u64, datname_id: u64) -> Row {
        Row::from([
            ("relname", Cell::StrId(relname_id)),
            ("datname", Cell::StrId(datname_id)),
        ])
    }

    #[test]
    fn str_cell_is_matches_only_the_resolved_string() {
        let (dict, ids) = dictionary_of(&["pool_probe", "kronika_db"]);
        let row = table_row(ids[0], ids[1]);
        assert!(str_cell_is(&row, "relname", "pool_probe", &dict));
        assert!(
            !str_cell_is(&row, "relname", "other_table", &dict),
            "a different string does not match"
        );
        assert!(
            !str_cell_is(&row, "datname", "pool_probe", &dict),
            "columns are not interchangeable"
        );
    }

    #[test]
    fn str_cell_is_rejects_missing_column_unresolved_id_and_non_strid() {
        let (dict, ids) = dictionary_of(&["pool_probe"]);
        let row = table_row(ids[0], 9_999);
        assert!(
            !str_cell_is(&row, "absent", "pool_probe", &dict),
            "a missing column never matches"
        );
        assert!(
            !str_cell_is(&row, "datname", "anything", &dict),
            "an id the dictionary does not carry never matches"
        );
        let non_str = Row::from([("relname", Cell::I64(5))]);
        assert!(
            !str_cell_is(&non_str, "relname", "5", &dict),
            "a non-StrId cell never matches"
        );
    }

    #[test]
    fn count_attributed_rows_requires_both_columns_to_match() {
        let (dict, ids) = dictionary_of(&["probe_a", "probe_b", "db_one", "db_two"]);
        let rows = vec![
            table_row(ids[0], ids[2]), // probe_a in db_one
            table_row(ids[1], ids[2]), // probe_b in db_one
            table_row(ids[0], ids[3]), // probe_a in db_two
        ];
        assert_eq!(count_attributed_rows(&rows, &dict, "probe_a", "db_one"), 1);
        assert_eq!(count_attributed_rows(&rows, &dict, "probe_b", "db_two"), 0);
        assert_eq!(
            count_attributed_rows(&rows, &dict, "probe_a", "db_two"),
            1,
            "the same table name in another database is a distinct row"
        );
    }
}
