//! Generic assertion and oracle steps used by converted features.
//!
//! Cucumber registers each step phrase once; these phrases are reused across
//! metric modules. Metric-specific steps live in the matching submodule
//! (e.g. [`crate::steps::archiver`]).

use anyhow::{Context, Result, bail};
use cucumber::{gherkin::Step, given, then};
use kronika_reader::Segment;
use kronika_registry::{Cell, ColumnType, Row, TypeContract, registry, section_name};

use crate::BddWorld;
use crate::harness::assert_row::{KeyMatch, RowSelector, assert_row, decode_section_labeled};
use crate::harness::dump;
use crate::harness::expected::{ExpectedColumn, ExpectedValue, parse_table};
use crate::harness::oracle::{OracleKind, OracleTarget, assert_oracle, window_contains};
use crate::steps::{docstring, table};

/// Assert that a singleton section has exactly one row matching the table.
///
/// Used by any metric whose section carries a single row (archiver, wal,
/// replication instance). The expected values are written as a `| column |
/// value |` table in the `.feature`.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\w.+-]+) has exactly one row:$")]
fn section_single_row(world: &mut BddWorld, section: String, step: &Step) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let contract = contract_for(section.type_id)?;
    let rows = table(step)?;
    let expected = parse_table(contract, rows, |name| world.harness.placeholder_pid(name))?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    assert_row(
        &segment,
        section.type_id,
        &section.label,
        &RowSelector::SingleRow,
        true,
        &expected,
        &failure_log,
    )
}

/// Assert that a section has a row for the named session, matched by its
/// backend pid.
///
/// The optional `exactly one` qualifier additionally asserts that the section
/// carries exactly one row (singleton). `[Name]` placeholders in the table are
/// resolved to pids; `[]` is an empty `ListI32`.
///
/// Matches:
/// - `section pg_locks.pg14_18 has a row for session "W":`
/// - `section pg_locks.pg14_18 has exactly one row for session "W":`
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = "^section ([\\w.+-]+) has ((?:exactly one )?a? ?)row for session \"([^\"]+)\":$")]
fn section_row_for_session(
    world: &mut BddWorld,
    section: String,
    qualifier: String,
    session_name: String,
    step: &Step,
) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let exactly_one = qualifier.trim().starts_with("exactly one");
    let pid = world.harness.placeholder_pid(&session_name)?;
    let contract = contract_for(section.type_id)?;
    let rows = table(step)?;
    let expected =
        parse_table_with_empty_list(contract, rows, |name| world.harness.placeholder_pid(name))?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    assert_row(
        &segment,
        section.type_id,
        &section.label,
        &RowSelector::ByPid { column: "pid", pid },
        exactly_one,
        &expected,
        &failure_log,
    )
}

/// Assert that a section has a row identified by one or more key columns.
///
/// The key spec is a `column = value` conjunction joined by ` and `; the data
/// table checks further columns. A value may be a quoted string, a bare scalar,
/// or one of the `[scenario database]` / `[second database]` placeholders that
/// resolve to the scenario's own or first extra database name. `[Name]`
/// placeholders in the table are resolved to session pids.
///
/// Matches, for example:
/// - `section pg_prepared_xacts has a row with datname = [scenario database]:`
/// - `section pg_stat_user_tables.pg16_17 has a row with relname = "probe" and datname = [second database]:`
/// - `section pg_stat_io.pg16_17 has a row with backend_type = "client backend" and object = "relation" and context = "normal":`
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\w.+-]+) has a row with (.+):$")]
fn section_row_by_key(
    world: &mut BddWorld,
    section: String,
    key_spec: String,
    step: &Step,
) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let contract = contract_for(section.type_id)?;
    let keys = parse_key_spec(contract, &key_spec, |slot| resolve_database(world, slot))?;
    let rows = table(step)?;
    let expected =
        parse_table_with_empty_list(contract, rows, |name| world.harness.placeholder_pid(name))?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    assert_row(
        &segment,
        section.type_id,
        &section.label,
        &RowSelector::ByKeys(keys),
        false,
        &expected,
        &failure_log,
    )
}

/// Resolve a `[scenario database]` / `[second database]` slot to a database name.
pub(crate) fn resolve_database(world: &BddWorld, slot: &str) -> Result<String> {
    match slot {
        "scenario database" => Ok(world.harness.database()?.to_owned()),
        "second database" => Ok(world.harness.extra_database_name(0)?.to_owned()),
        other => anyhow::bail!("unknown database placeholder [{other}]"),
    }
}

/// Verify a section column against an independent `PostgreSQL` oracle query.
///
/// The oracle kind (`exact`, `subset`, `top-n`, …) is named in the step; the
/// docstring carries the oracle SQL. The query runs on the scenario database,
/// or on the first extra database when the phrase ends with `in the second
/// database` — the form per-database fan-out features use to check each side.
#[then(
    regex = r"^section ([\w.+-]+)(?: (\w+))? matches the ([\w-]+) oracle( in the second database)?:$"
)]
async fn section_oracle(
    world: &mut BddWorld,
    subject: String,
    column: String,
    kind: String,
    second_database: String,
    step: &Step,
) -> Result<()> {
    let subject = parse_section_column(&subject, &column)?;
    let contract = contract_for(subject.section.type_id)?;
    let kind = OracleKind::parse(&kind)?;
    let sql = docstring(step)?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let dsn = if second_database.is_empty() {
        world.harness.database_dsn()?
    } else {
        world.harness.extra_database_dsn(0)?
    };
    let (client, connection) = tokio_postgres::connect(&dsn, tokio_postgres::NoTls)
        .await
        .context("connect for the oracle query")?;
    let driver = tokio::spawn(async move {
        drop(connection.await);
    });
    let result = assert_oracle(
        &client,
        OracleTarget {
            contract,
            section_label: &subject.section.label,
            subject_label: &subject.label,
            column: &subject.column,
        },
        &segment,
        kind,
        sql,
        &failure_log,
    )
    .await;
    driver.abort();
    result
}

/// Assert that a singleton section's column is `NULL`.
///
/// Service sections use this for nullable source-unavailable cases: the vadv
/// `pg_store_plans` fork ships no info view, `pg_stat_io` predates PG16, and
/// so on. The section must hold exactly one row.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\w.+-]+)(?: (\w+))? is null$")]
fn section_column_null(world: &mut BddWorld, subject: String, column: String) -> Result<()> {
    let subject = parse_section_column(&subject, &column)?;
    let (row, _dict) = single_row_labeled(world, &subject.section)?;
    let cell = row
        .get(subject.column.as_str())
        .with_context(|| format!("{} has no column {:?}", subject.label, subject.column))?;
    anyhow::ensure!(
        cell == &Cell::Null,
        "{} is {cell:?}, expected NULL",
        subject.label
    );
    Ok(())
}

/// Assert that a singleton section's `StrId` column resolves to the given
/// string through the segment dictionary.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r#"^section ([\w.+-]+)(?: (\w+))? resolves to "([^"]*)"$"#)]
fn section_column_resolves(
    world: &mut BddWorld,
    subject: String,
    column: String,
    expected: String,
) -> Result<()> {
    let subject = parse_section_column(&subject, &column)?;
    let (row, dict) = single_row_labeled(world, &subject.section)?;
    let actual = resolve_str_column(subject.section.type_id, &row, &dict, &subject.column)?;
    anyhow::ensure!(
        actual == expected,
        "{} is {actual:?}, expected {expected:?}",
        subject.label
    );
    Ok(())
}

/// Decode a singleton section: exactly one row, or an error naming the count.
pub(crate) fn single_row(
    world: &BddWorld,
    type_id: u32,
) -> Result<(Row, kronika_reader::Dictionary)> {
    let section = SectionRef {
        type_id,
        label: canonical_section_label(type_id),
    };
    single_row_labeled(world, &section)
}

fn single_row_labeled(
    world: &BddWorld,
    section: &SectionRef,
) -> Result<(Row, kronika_reader::Dictionary)> {
    let segment = world.harness.segment()?.clone();
    let (mut rows, dict) = decode_section_labeled(&segment, section.type_id, &section.label)?;
    anyhow::ensure!(
        rows.len() == 1,
        "section {} holds {} rows, expected exactly one",
        section.label,
        rows.len()
    );
    let row = rows.remove(0);
    Ok((row, dict))
}

/// Resolve a `StrId` column of one row to an owned UTF-8 string.
pub(crate) fn resolve_str_column(
    type_id: u32,
    row: &Row,
    dict: &kronika_reader::Dictionary,
    column: &str,
) -> Result<String> {
    use kronika_reader::Resolved;

    let cell = row
        .get(column)
        .with_context(|| format!("section {type_id} has no column {column:?}"))?;
    let Cell::StrId(id) = cell else {
        bail!("section {type_id}: {column} is {cell:?}, expected an interned string");
    };
    match dict.resolve(*id) {
        Some(Resolved::String(bytes) | Resolved::Blob { bytes, .. }) => {
            Ok(String::from_utf8_lossy(bytes).into_owned())
        }
        None => {
            bail!("section {type_id}: {column} str_id={id} did not resolve through the dictionary")
        }
    }
}

/// Assert that a section id is absent from the sealed segment catalog.
///
/// Layout-split metrics use this to prove the collector did not also write a
/// row under the wrong versioned `type_id`.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\w.+-]+) is absent from the segment$")]
fn section_absent(world: &mut BddWorld, section: String) -> Result<()> {
    let section = parse_section_ref(&section)?;
    let path = world.harness.segment()?;
    let segment = Segment::open(path).context("open sealed segment")?;
    let present = segment
        .catalog()
        .entries
        .iter()
        .any(|entry| entry.type_id == section.type_id);
    anyhow::ensure!(
        !present,
        "section {} is present in the segment but must be absent for this layout",
        section.label
    );
    Ok(())
}

/// Capture the window floor for a section column: run the docstring SQL now,
/// before the snapshot, and store the value.
///
/// Pairs with `section X.<column> is between the captured floor and:`; together
/// they bracket a monotonically advancing counter between two oracle reads.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[given(regex = r"^the window floor for section ([\w.+-]+)(?: (\w+))? is captured as:$")]
async fn window_floor_captured(
    world: &mut BddWorld,
    subject: String,
    column: String,
    step: &Step,
) -> Result<()> {
    let subject = parse_section_column(&subject, &column)?;
    let sql = docstring(step)?;
    let floor = scalar_i64(&world.harness.database_dsn()?, sql).await?;
    world.harness.set_window_floor(
        &window_floor_key(subject.section.type_id, &subject.column),
        floor,
    );
    Ok(())
}

/// Assert the recorded value lies between the captured floor and a ceiling read
/// by the docstring SQL after the snapshot.
#[allow(
    clippy::needless_pass_by_value,
    reason = "cucumber step parameters must be owned String"
)]
#[then(regex = r"^section ([\w.+-]+)(?: (\w+))? is between the captured floor and:$")]
async fn section_window_bounds(
    world: &mut BddWorld,
    subject: String,
    column: String,
    step: &Step,
) -> Result<()> {
    let subject = parse_section_column(&subject, &column)?;
    let sql = docstring(step)?;
    let floor = world
        .harness
        .window_floor(&window_floor_key(subject.section.type_id, &subject.column))?;
    let ceiling = scalar_i64(&world.harness.database_dsn()?, sql).await?;
    let segment = world.harness.segment()?.clone();
    let failure_log = world.harness.failure_log()?;
    let (rows, _dict) =
        decode_section_labeled(&segment, subject.section.type_id, &subject.section.label)?;
    let value = match recorded_i64(&rows, &subject.column) {
        Ok(value) => value,
        Err(err) => bail!(
            "{}",
            dump::section_dump(
                &format!("{}: {err}", subject.label),
                &rows,
                &failure_log,
                &[],
            )
        ),
    };
    if window_contains(floor, value, ceiling) {
        return Ok(());
    }
    bail!(
        "{}",
        dump::section_dump(
            &format!(
                "{}: window bounds failed (floor <= recorded <= ceiling)",
                subject.label
            ),
            &rows,
            &failure_log,
            &[
                ("window floor", floor.to_string()),
                ("recorded", value.to_string()),
                ("window ceiling", ceiling.to_string()),
            ],
        )
    )
}

/// The storage key for a captured window floor.
fn window_floor_key(type_id: u32, column: &str) -> String {
    format!("{type_id}.{column}")
}

/// The single recorded `int8` value of `column` across the decoded rows.
fn recorded_i64(rows: &[Row], column: &str) -> Result<i64> {
    let [row] = rows else {
        bail!("expected exactly one row, got {}", rows.len());
    };
    match row.get(column) {
        Some(Cell::I64(value)) => Ok(*value),
        Some(other) => bail!(
            "recorded value is {}, not an int8",
            dump::render_cell(other)
        ),
        None => bail!("column absent from the decoded row"),
    }
}

/// Run `sql` on a fresh connection to the scenario database and read the
/// single row's first column as `int8`.
async fn scalar_i64(dsn: &str, sql: &str) -> Result<i64> {
    let (client, connection) = tokio_postgres::connect(dsn, tokio_postgres::NoTls)
        .await
        .context("connect for the window bound read")?;
    let driver = tokio::spawn(async move {
        drop(connection.await);
    });
    let result = async {
        let row = client
            .query_one(sql, &[])
            .await
            .context("window bound read")?;
        row.try_get::<_, i64>(0)
            .context("window bound value as int8")
    }
    .await;
    driver.abort();
    result
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionRef {
    pub(crate) type_id: u32,
    pub(crate) label: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SectionColumnRef {
    pub(crate) section: SectionRef,
    pub(crate) column: String,
    pub(crate) label: String,
}

struct FieldAlias {
    name: &'static str,
    type_id: u32,
    column: &'static str,
}

const SECTION_ALIASES: &[(&str, u32)] = &[
    ("pg_stat_activity.pg10_12", 1_001_001),
    ("pg_stat_activity.pg13", 1_001_002),
    ("pg_stat_activity.pg14_18", 1_001_003),
    ("pg_stat_statements.pg10_12", 1_002_001),
    ("pg_stat_statements.pg13", 1_002_002),
    ("pg_stat_statements.pg14", 1_002_003),
    ("pg_stat_statements.pg15_16", 1_002_004),
    ("pg_stat_statements.pg17", 1_002_005),
    ("pg_stat_statements.pg18", 1_002_006),
    ("pg_store_plans.ossc", 1_003_001),
    ("pg_store_plans.vadv", 1_004_001),
    ("pg_stat_database.pg10_11", 1_005_001),
    ("pg_stat_database.pg12_13", 1_005_002),
    ("pg_stat_database.pg14_17", 1_005_003),
    ("pg_stat_database.pg18", 1_005_004),
    ("pg_stat_bgwriter+pg_stat_checkpointer", 1_006_001),
    ("bgwriter_checkpointer", 1_006_001),
    ("pg_stat_wal.pg15_17", 1_007_001),
    ("pg_stat_wal.pg18", 1_007_002),
    ("pg_stat_io.pg16_17", 1_009_001),
    ("pg_stat_io.pg18", 1_009_002),
    ("pg_locks.pg10_13", 1_011_001),
    ("pg_locks.pg14_18", 1_011_002),
    ("pg_stat_user_tables.pg10_12", 1_013_001),
    ("pg_stat_user_tables.pg13_15", 1_013_002),
    ("pg_stat_user_tables.pg16_17", 1_013_003),
    ("pg_stat_user_tables.pg18", 1_013_004),
    ("pg_stat_user_indexes.pg10_15", 1_014_001),
    ("pg_stat_user_indexes.pg16_18", 1_014_002),
];

const FIELD_ALIASES: &[FieldAlias] = &[
    FieldAlias {
        name: "pg_is_in_recovery.is_in_recovery",
        type_id: 1_015_001,
        column: "is_in_recovery",
    },
    FieldAlias {
        name: "pg_stat_replication.streaming_replicas",
        type_id: 1_015_001,
        column: "streaming_replicas",
    },
    FieldAlias {
        name: "pg_control_checkpoint.timeline_id",
        type_id: 1_015_001,
        column: "timeline_id",
    },
    FieldAlias {
        name: "pg_current_wal_lsn.current_wal_lsn",
        type_id: 1_015_001,
        column: "current_wal_lsn",
    },
    FieldAlias {
        name: "pg_stat_wal.current_wal_lsn",
        type_id: 1_015_001,
        column: "current_wal_lsn",
    },
];

/// Parse a section id or BDD alias as written in features.
pub(crate) fn parse_type_id(raw: &str) -> Result<u32> {
    Ok(parse_section_ref(raw)?.type_id)
}

pub(crate) fn parse_section_ref(raw: &str) -> Result<SectionRef> {
    let raw = raw.trim();
    if let Some(type_id) = parse_numeric_type_id(raw)? {
        return section_ref_for_type_id(type_id);
    }

    let normalized = raw.to_ascii_lowercase();
    if let Some((alias, type_id)) = SECTION_ALIASES
        .iter()
        .find(|(alias, _)| *alias == normalized)
    {
        contract_for(*type_id)?;
        return Ok(SectionRef {
            type_id: *type_id,
            label: (*alias).to_owned(),
        });
    }

    let matches: Vec<&TypeContract> = registry()
        .iter()
        .filter(|contract| contract.name == raw)
        .collect();
    match matches.as_slice() {
        [contract] => Ok(SectionRef {
            type_id: contract.type_id.get(),
            label: raw.to_owned(),
        }),
        [] => bail!("unknown BDD section {raw:?}; use a registry section name or BDD alias"),
        many => {
            let choices = many
                .iter()
                .map(|contract| canonical_section_label(contract.type_id.get()))
                .collect::<Vec<_>>()
                .join(", ");
            bail!("section name {raw:?} is versioned; use one of: {choices}")
        }
    }
}

pub(crate) fn parse_section_column(subject: &str, column: &str) -> Result<SectionColumnRef> {
    let subject = subject.trim();
    let column = column.trim();
    if !column.is_empty() {
        let section = parse_section_ref(subject)?;
        return section_column_ref(section, column, None);
    }

    let normalized = subject.to_ascii_lowercase();
    if let Some(alias) = FIELD_ALIASES.iter().find(|alias| alias.name == normalized) {
        let section = section_ref_for_type_id(alias.type_id)?;
        return section_column_ref(section, alias.column, Some(alias.name));
    }

    let (section, column) = subject
        .rsplit_once('.')
        .with_context(|| format!("{subject:?} must be written as section.column"))?;
    let section = parse_section_ref(section)?;
    section_column_ref(section, column, None)
}

fn section_column_ref(
    section: SectionRef,
    column: &str,
    label: Option<&str>,
) -> Result<SectionColumnRef> {
    let contract = contract_for(section.type_id)?;
    anyhow::ensure!(
        contract.column(column).is_some(),
        "section {} has no column {column:?}",
        section.label
    );
    let label = label.map_or_else(|| format!("{}.{}", section.label, column), str::to_owned);
    Ok(SectionColumnRef {
        section,
        column: column.to_owned(),
        label,
    })
}

fn parse_numeric_type_id(raw: &str) -> Result<Option<u32>> {
    if !raw.chars().all(|c| c.is_ascii_digit() || c == '_') {
        return Ok(None);
    }
    raw.replace('_', "")
        .parse::<u32>()
        .map(Some)
        .with_context(|| format!("invalid section type_id {raw:?}"))
}

fn section_ref_for_type_id(type_id: u32) -> Result<SectionRef> {
    contract_for(type_id)?;
    Ok(SectionRef {
        type_id,
        label: canonical_section_label(type_id),
    })
}

fn canonical_section_label(type_id: u32) -> String {
    SECTION_ALIASES
        .iter()
        .find(|(_, alias_type_id)| *alias_type_id == type_id)
        .map(|(alias, _)| (*alias).to_owned())
        .or_else(|| section_name(type_id).map(str::to_owned))
        .unwrap_or_else(|| type_id.to_string())
}

/// The registry contract for `type_id`, or an error if it is not registered.
pub(crate) fn contract_for(type_id: u32) -> Result<&'static TypeContract> {
    registry()
        .iter()
        .find(|contract| contract.type_id.get() == type_id)
        .with_context(|| format!("no registered section has type_id {type_id}"))
}

/// Parse a `| column | value |` table, additionally recognising `[]` as an
/// empty `ListI32` cell.
///
/// Delegates to [`parse_table`] for all other value forms. `[]` is a literal
/// token in the feature, not a placeholder (a `[Name]` placeholder has content
/// between the brackets); the expected-value placeholder parser already
/// rejects empty brackets, so `[]` falls through to this layer.
pub(crate) fn parse_table_with_empty_list(
    contract: &TypeContract,
    rows: &[Vec<String>],
    mut resolve_pid: impl FnMut(&str) -> Result<i32>,
) -> Result<Vec<ExpectedColumn>> {
    use anyhow::bail;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let [name, raw] = row.as_slice() else {
            bail!("expected-row table needs exactly two columns, got {row:?}");
        };
        if raw.trim() == "[]" {
            out.push(ExpectedColumn {
                name: name.clone(),
                value: ExpectedValue::Cell(Cell::ListI32(Vec::new())),
            });
            continue;
        }
        // All other forms are handled by the standard parser.
        let single = std::slice::from_ref(row);
        let parsed = parse_table(contract, single, &mut resolve_pid)?;
        out.extend(parsed);
    }
    Ok(out)
}

/// Parse `raw` as a key cell for `column` in `contract`.
///
/// Looks up the column type in the contract and parses `raw` as a scalar.
/// `[]` short-circuits the contract lookup and returns `Cell::ListI32(vec![])`.
pub(crate) fn parse_key_cell(contract: &TypeContract, column: &str, raw: &str) -> Result<Cell> {
    use anyhow::bail;

    if raw == "[]" {
        return Ok(Cell::ListI32(Vec::new()));
    }
    let col = contract.column(column).with_context(|| {
        format!(
            "section {} has no column {column:?}",
            contract.type_id.get()
        )
    })?;
    let cell = match col.ty {
        ColumnType::I8 | ColumnType::I16 => Cell::I16(
            raw.parse::<i16>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::I32 => Cell::I32(
            raw.parse::<i32>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::I64 => Cell::I64(
            raw.parse::<i64>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::U8 | ColumnType::U16 | ColumnType::U32 => Cell::U32(
            raw.parse::<u32>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::U64 => Cell::U64(
            raw.parse::<u64>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::F32 | ColumnType::F64 => Cell::F64(
            raw.parse::<f64>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::Bool => {
            let v = match raw.to_ascii_lowercase().as_str() {
                "true" => true,
                "false" => false,
                other => bail!("{other:?} is not a boolean"),
            };
            Cell::Bool(v)
        }
        ColumnType::Ts => Cell::Ts(
            raw.parse::<i64>()
                .map_err(|e| anyhow::anyhow!("{raw:?}: {e}"))?,
        ),
        ColumnType::StrId => {
            bail!("key column {column:?} is a StrId; use the string form in the step phrase")
        }
        ColumnType::ListI32 => {
            bail!("key column {column:?} is a ListI32; only [] is supported in key phrases")
        }
    };
    Ok(cell)
}

/// Parse a `column = value [and column = value ...]` key spec into a match
/// conjunction against `contract`.
///
/// A bracketed value is resolved via `resolve_slot` (the `[scenario database]`
/// placeholders); a quoted or bare `StrId` value becomes a string key; any other
/// value is parsed as a scalar cell for the column's type.
pub(crate) fn parse_key_spec(
    contract: &TypeContract,
    spec: &str,
    mut resolve_slot: impl FnMut(&str) -> Result<String>,
) -> Result<Vec<KeyMatch>> {
    let mut keys = Vec::new();
    for clause in spec.split(" and ") {
        let (column, raw) = clause
            .split_once('=')
            .with_context(|| format!("key clause {clause:?} is not `column = value`"))?;
        let column = column.trim().to_owned();
        let raw = raw.trim();
        let key = if let Some(slot) = bracketed(raw) {
            KeyMatch::Str {
                column,
                value: resolve_slot(slot)?,
            }
        } else if let Some(text) = quoted(raw) {
            KeyMatch::Str {
                column,
                value: text.to_owned(),
            }
        } else if is_str_column(contract, &column) {
            KeyMatch::Str {
                column,
                value: raw.to_owned(),
            }
        } else {
            let cell = parse_key_cell(contract, &column, raw)?;
            KeyMatch::Cell { column, cell }
        };
        keys.push(key);
    }
    anyhow::ensure!(!keys.is_empty(), "key spec {spec:?} has no clauses");
    Ok(keys)
}

/// The content of a `[ ... ]` slot, if `raw` is one.
fn bracketed(raw: &str) -> Option<&str> {
    raw.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .map(str::trim)
        .filter(|slot| !slot.is_empty())
}

/// The content of a `"..."` string, if `raw` is one.
fn quoted(raw: &str) -> Option<&str> {
    raw.strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
}

/// Whether `column` is a `StrId` column in `contract`.
fn is_str_column(contract: &TypeContract, column: &str) -> bool {
    contract
        .column(column)
        .is_some_and(|col| col.ty == ColumnType::StrId)
}

#[cfg(test)]
mod tests {
    use super::{
        contract_for, parse_key_cell, parse_section_column, parse_section_ref,
        parse_table_with_empty_list,
    };
    use crate::harness::expected::ExpectedValue;
    use kronika_registry::pg_stat_archiver::PgStatArchiver;
    use kronika_registry::{Cell, Section};

    fn no_pids(name: &str) -> anyhow::Result<i32> {
        anyhow::bail!("no session named {name:?}")
    }

    #[test]
    fn resolves_the_archiver_contract_and_rejects_an_unknown_id() {
        assert_eq!(
            contract_for(1_008_001).unwrap().type_id.get(),
            1_008_001,
            "the archiver section resolves"
        );
        assert!(
            contract_for(9_999_999).is_err(),
            "an unknown id is rejected"
        );
    }

    #[test]
    fn parse_table_with_empty_list_passes_through_normal_values() {
        let rows = vec![
            vec!["archived_count".to_owned(), "5".to_owned()],
            vec!["failed_count".to_owned(), "0".to_owned()],
        ];
        let parsed =
            parse_table_with_empty_list(&PgStatArchiver::CONTRACT, &rows, no_pids).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(
            parsed[0].value,
            ExpectedValue::Cell(Cell::I64(5)),
            "regular integer passes through"
        );
    }

    #[test]
    fn parse_table_with_empty_list_accepts_empty_list_literal() {
        let rows = vec![vec!["some_list_col".to_owned(), "[]".to_owned()]];
        let parsed =
            parse_table_with_empty_list(&PgStatArchiver::CONTRACT, &rows, no_pids).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(
            parsed[0].value,
            ExpectedValue::Cell(Cell::ListI32(Vec::new())),
            "[] produces an empty ListI32"
        );
    }

    #[test]
    fn parse_key_cell_parses_scalars_for_known_column_types() {
        let cell = parse_key_cell(&PgStatArchiver::CONTRACT, "archived_count", "42").unwrap();
        assert_eq!(cell, Cell::I64(42));

        let cell = parse_key_cell(&PgStatArchiver::CONTRACT, "failed_count", "0").unwrap();
        assert_eq!(cell, Cell::I64(0));
    }

    #[test]
    fn parse_key_cell_accepts_empty_list() {
        let cell = parse_key_cell(&PgStatArchiver::CONTRACT, "any_col", "[]").unwrap();
        assert_eq!(cell, Cell::ListI32(Vec::new()));
    }

    #[test]
    fn parse_key_cell_rejects_unparseable_integer() {
        assert!(
            parse_key_cell(&PgStatArchiver::CONTRACT, "archived_count", "not-a-number").is_err()
        );
    }

    #[test]
    fn oracle_kind_accepts_hyphenated_top_n() {
        use crate::harness::oracle::OracleKind;
        assert_eq!(OracleKind::parse("top-n").unwrap(), OracleKind::TopN);
        assert_eq!(OracleKind::parse("exact").unwrap(), OracleKind::Exact);
        assert_eq!(OracleKind::parse("subset").unwrap(), OracleKind::Subset);
        assert_eq!(
            OracleKind::parse("transformed").unwrap(),
            OracleKind::Transformed
        );
    }

    #[test]
    fn recorded_i64_reads_one_cell_and_rejects_bad_shapes() {
        let rows = vec![crate::harness::test_row(&[(
            "current_wal_lsn",
            Cell::I64(16_777_216),
        )])];
        assert_eq!(
            super::recorded_i64(&rows, "current_wal_lsn").unwrap(),
            16_777_216
        );
        assert!(super::recorded_i64(&[], "c").is_err(), "no rows fail");
        let null_rows = vec![crate::harness::test_row(&[("c", Cell::Null)])];
        assert!(super::recorded_i64(&null_rows, "c").is_err(), "null fails");
        let two = vec![
            crate::harness::test_row(&[("c", Cell::I64(1))]),
            crate::harness::test_row(&[("c", Cell::I64(2))]),
        ];
        assert!(super::recorded_i64(&two, "c").is_err(), "two rows fail");
        assert!(
            super::recorded_i64(&rows, "missing").is_err(),
            "a missing column fails"
        );
    }

    #[test]
    fn type_id_parses_with_and_without_underscores() {
        assert_eq!(super::parse_type_id("1_008_001").unwrap(), 1_008_001);
        assert_eq!(super::parse_type_id("1008001").unwrap(), 1_008_001);
        assert_eq!(
            super::parse_type_id("pg_stat_wal.pg15_17").unwrap(),
            1_007_001
        );
        assert!(super::parse_type_id("porridge").is_err());
    }

    #[test]
    fn section_ref_resolves_aliases_and_rejects_ambiguous_names() {
        let section = parse_section_ref("pg_stat_database.pg14_17").unwrap();
        assert_eq!(section.type_id, 1_005_003);
        assert_eq!(section.label, "pg_stat_database.pg14_17");

        assert!(
            parse_section_ref("pg_stat_database").is_err(),
            "versioned registry names need an explicit BDD alias"
        );
    }

    #[test]
    fn section_column_resolves_section_dot_column_and_raw_probe_aliases() {
        let subject = parse_section_column("pg_stat_wal.pg15_17.stats_reset", "").unwrap();
        assert_eq!(subject.section.type_id, 1_007_001);
        assert_eq!(subject.column, "stats_reset");
        assert_eq!(subject.label, "pg_stat_wal.pg15_17.stats_reset");

        let subject = parse_section_column("pg_control_checkpoint.timeline_id", "").unwrap();
        assert_eq!(subject.section.type_id, 1_015_001);
        assert_eq!(subject.column, "timeline_id");
        assert_eq!(subject.label, "pg_control_checkpoint.timeline_id");
    }

    #[test]
    fn parse_key_spec_builds_a_multi_key_conjunction() {
        use crate::harness::assert_row::KeyMatch;
        use kronika_registry::pg_prepared_xacts::PgPreparedXacts;

        let keys = super::parse_key_spec(
            &PgPreparedXacts::CONTRACT,
            r#"datname = "kronika_db" and prepared_count = 1"#,
            |slot| anyhow::bail!("unexpected slot {slot:?}"),
        )
        .unwrap();
        assert_eq!(keys.len(), 2, "both clauses become keys");
        assert_eq!(
            keys[0],
            KeyMatch::Str {
                column: "datname".to_owned(),
                value: "kronika_db".to_owned(),
            },
            "a quoted value is a string key"
        );
        assert_eq!(
            keys[1],
            KeyMatch::Cell {
                column: "prepared_count".to_owned(),
                cell: Cell::I64(1),
            },
            "a scalar column parses to a cell"
        );
    }

    #[test]
    fn parse_key_spec_resolves_a_bracket_slot_and_bare_strid() {
        use crate::harness::assert_row::KeyMatch;
        use kronika_registry::pg_prepared_xacts::PgPreparedXacts;

        // A bracketed value goes through the slot resolver.
        let keys = super::parse_key_spec(
            &PgPreparedXacts::CONTRACT,
            "datname = [scenario database]",
            |slot| {
                assert_eq!(slot, "scenario database");
                Ok("resolved_db".to_owned())
            },
        )
        .unwrap();
        assert_eq!(
            keys[0],
            KeyMatch::Str {
                column: "datname".to_owned(),
                value: "resolved_db".to_owned(),
            }
        );

        // A bare value against a StrId column becomes a string key, unquoted.
        let keys =
            super::parse_key_spec(&PgPreparedXacts::CONTRACT, "datname = plain_name", |slot| {
                anyhow::bail!("unexpected slot {slot:?}")
            })
            .unwrap();
        assert_eq!(
            keys[0],
            KeyMatch::Str {
                column: "datname".to_owned(),
                value: "plain_name".to_owned(),
            }
        );
    }

    #[test]
    fn parse_key_spec_rejects_a_clause_without_equals() {
        use kronika_registry::pg_prepared_xacts::PgPreparedXacts;

        assert!(
            super::parse_key_spec(&PgPreparedXacts::CONTRACT, "datname is kronika", |_| {
                anyhow::bail!("no slots")
            })
            .is_err(),
            "a clause with no `=` is rejected"
        );
    }
}
