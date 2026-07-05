# Pg Row Mapper Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** add an internal `tokio_postgres::Row -> *Row` mapper that resolves column names once per result set, returns structured errors, and migrates `activity` plus `statements` as the first pilots.

**Architecture:** `kronika-source-pg::pg_row` owns the safe column-index primitive and public error types. A crate-local `pg_row_mapper!` macro generates per-collector column maps and `read` methods. Converted collectors prepare statements, validate columns from `Statement::columns()`, query by prepared statement, decode rows through `try_get(index)`, and return `PgCollectError`.

**Tech Stack:** Rust 2024, `tokio-postgres`, `macro_rules!`, existing workspace lints, unit tests in `kronika-source-pg`, caller updates in `pg_kronika-collector`.

---

## File Structure

- Create `crates/kronika-source-pg/src/pg_row.rs`.
  Owns `PgRowError`, `PgCollectError`, `PgCol<T>`, unit tests, and `Display`/`Error` impls.
- Modify `crates/kronika-source-pg/src/lib.rs`.
  Adds `mod pg_row;`, re-exports `PgCollectError`/`PgRowError`, and exports the macro for crate-internal modules.
- Modify `crates/kronika-source-pg/src/activity.rs`.
  Replaces manual `row_from_pg` with generated `ActivityCols`; `collect_activity` returns `PgCollectError`.
- Modify `crates/kronika-source-pg/src/statements.rs`.
  Replaces manual `row_from_pg` with generated `StatementsCols`; `collect_statements` returns `PgCollectError`.
- Modify `bins/pg_kronika-collector/src/main.rs`.
  Imports `PgCollectError`, adapts activity and statements logging, and preserves SQLSTATE handling through `PgCollectError::as_query_error()`.

## Task 1: Add Row-Mapping Error and Column Primitive

**Files:**
- Create: `crates/kronika-source-pg/src/pg_row.rs`
- Modify: `crates/kronika-source-pg/src/lib.rs`

- [ ] **Step 1: Add failing tests for column lookup and error formatting**

Add `crates/kronika-source-pg/src/pg_row.rs` with tests first:

```rust
//! Shared helpers for decoding `tokio_postgres::Row` values into source rows.

use std::fmt;
use std::marker::PhantomData;

#[derive(Debug)]
pub enum PgRowError {
    MissingColumn {
        row: &'static str,
        field: &'static str,
        column: &'static str,
    },
    DuplicateColumn {
        row: &'static str,
        field: &'static str,
        column: &'static str,
    },
    DecodeColumn {
        row: &'static str,
        field: &'static str,
        column: &'static str,
        source: tokio_postgres::Error,
    },
}

#[derive(Debug)]
pub enum PgCollectError {
    Query(tokio_postgres::Error),
    Row(PgRowError),
}

pub struct PgCol<T> {
    index: usize,
    row: &'static str,
    field: &'static str,
    column: &'static str,
    _type: PhantomData<fn() -> T>,
}

impl<T> PgCol<T> {
    pub fn required<'a>(
        row: &'static str,
        field: &'static str,
        column: &'static str,
        columns: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, PgRowError> {
        let _ = (row, field, column, columns);
        unimplemented!("write implementation after the failing tests are in place")
    }
}

#[cfg(test)]
mod tests {
    use super::{PgCol, PgRowError};

    #[test]
    fn required_column_finds_index() {
        let col = PgCol::<i64>::required("ActivityRow", "ts", "ts_us", ["pid", "ts_us"])
            .expect("column should exist");

        assert_eq!(col.index(), 1);
    }

    #[test]
    fn required_column_reports_missing_name() {
        let err = PgCol::<i64>::required("ActivityRow", "ts", "ts_us", ["pid"])
            .expect_err("missing column should be an error");

        assert!(matches!(
            err,
            PgRowError::MissingColumn {
                row: "ActivityRow",
                field: "ts",
                column: "ts_us",
            }
        ));
        assert_eq!(err.to_string(), "ActivityRow.ts: missing PostgreSQL column `ts_us`");
    }

    #[test]
    fn required_column_reports_duplicate_name() {
        let err = PgCol::<i64>::required("ActivityRow", "ts", "ts_us", ["ts_us", "ts_us"])
            .expect_err("duplicate column should be an error");

        assert!(matches!(
            err,
            PgRowError::DuplicateColumn {
                row: "ActivityRow",
                field: "ts",
                column: "ts_us",
            }
        ));
        assert_eq!(
            err.to_string(),
            "ActivityRow.ts: duplicate PostgreSQL column `ts_us`"
        );
    }
}
```

- [ ] **Step 2: Run the new tests to verify red**

Run:

```bash
cargo test -p kronika-source-pg pg_row
```

Expected: compilation or test failure because `PgCol::required` is still `unimplemented!` and `PgCol::index` does not exist.

- [ ] **Step 3: Implement `PgRowError`, `PgCollectError`, and `PgCol<T>`**

Replace `crates/kronika-source-pg/src/pg_row.rs` with:

```rust
//! Shared helpers for decoding `tokio_postgres::Row` values into source rows.

use std::error::Error;
use std::fmt;
use std::marker::PhantomData;

use tokio_postgres::Row;
use tokio_postgres::types::FromSqlOwned;

#[derive(Debug)]
pub enum PgRowError {
    MissingColumn {
        row: &'static str,
        field: &'static str,
        column: &'static str,
    },
    DuplicateColumn {
        row: &'static str,
        field: &'static str,
        column: &'static str,
    },
    DecodeColumn {
        row: &'static str,
        field: &'static str,
        column: &'static str,
        source: tokio_postgres::Error,
    },
}

impl fmt::Display for PgRowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingColumn { row, field, column } => {
                write!(f, "{row}.{field}: missing PostgreSQL column `{column}`")
            }
            Self::DuplicateColumn { row, field, column } => {
                write!(f, "{row}.{field}: duplicate PostgreSQL column `{column}`")
            }
            Self::DecodeColumn {
                row,
                field,
                column,
                source,
            } => {
                write!(
                    f,
                    "{row}.{field}: cannot decode PostgreSQL column `{column}`: {source}"
                )
            }
        }
    }
}

impl Error for PgRowError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::DecodeColumn { source, .. } => Some(source),
            Self::MissingColumn { .. } | Self::DuplicateColumn { .. } => None,
        }
    }
}

#[derive(Debug)]
pub enum PgCollectError {
    Query(tokio_postgres::Error),
    Row(PgRowError),
}

impl PgCollectError {
    #[must_use]
    pub const fn as_query_error(&self) -> Option<&tokio_postgres::Error> {
        match self {
            Self::Query(err) => Some(err),
            Self::Row(_) => None,
        }
    }
}

impl fmt::Display for PgCollectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Query(err) => fmt::Display::fmt(err, f),
            Self::Row(err) => fmt::Display::fmt(err, f),
        }
    }
}

impl Error for PgCollectError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Query(err) => Some(err),
            Self::Row(err) => Some(err),
        }
    }
}

impl From<tokio_postgres::Error> for PgCollectError {
    fn from(source: tokio_postgres::Error) -> Self {
        Self::Query(source)
    }
}

impl From<PgRowError> for PgCollectError {
    fn from(source: PgRowError) -> Self {
        Self::Row(source)
    }
}

#[derive(Debug)]
pub struct PgCol<T> {
    index: usize,
    row: &'static str,
    field: &'static str,
    column: &'static str,
    _type: PhantomData<fn() -> T>,
}

impl<T> PgCol<T> {
    pub fn required<'a>(
        row: &'static str,
        field: &'static str,
        column: &'static str,
        columns: impl IntoIterator<Item = &'a str>,
    ) -> Result<Self, PgRowError> {
        let mut matches = columns
            .into_iter()
            .enumerate()
            .filter(|(_, candidate)| *candidate == column)
            .map(|(index, _)| index);
        let Some(index) = matches.next() else {
            return Err(PgRowError::MissingColumn { row, field, column });
        };
        if matches.next().is_some() {
            return Err(PgRowError::DuplicateColumn { row, field, column });
        }
        Ok(Self {
            index,
            row,
            field,
            column,
            _type: PhantomData,
        })
    }

    #[cfg(test)]
    pub const fn index(&self) -> usize {
        self.index
    }
}

impl<T> PgCol<T>
where
    T: FromSqlOwned,
{
    pub fn get(&self, row: &Row) -> Result<T, PgRowError> {
        row.try_get(self.index).map_err(|source| PgRowError::DecodeColumn {
            row: self.row,
            field: self.field,
            column: self.column,
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{PgCol, PgRowError};

    #[test]
    fn required_column_finds_index() {
        let col = PgCol::<i64>::required("ActivityRow", "ts", "ts_us", ["pid", "ts_us"])
            .expect("column should exist");

        assert_eq!(col.index(), 1);
    }

    #[test]
    fn required_column_reports_missing_name() {
        let err = PgCol::<i64>::required("ActivityRow", "ts", "ts_us", ["pid"])
            .expect_err("missing column should be an error");

        assert!(matches!(
            err,
            PgRowError::MissingColumn {
                row: "ActivityRow",
                field: "ts",
                column: "ts_us",
            }
        ));
        assert_eq!(
            err.to_string(),
            "ActivityRow.ts: missing PostgreSQL column `ts_us`"
        );
    }

    #[test]
    fn required_column_reports_duplicate_name() {
        let err = PgCol::<i64>::required("ActivityRow", "ts", "ts_us", ["ts_us", "ts_us"])
            .expect_err("duplicate column should be an error");

        assert!(matches!(
            err,
            PgRowError::DuplicateColumn {
                row: "ActivityRow",
                field: "ts",
                column: "ts_us",
            }
        ));
        assert_eq!(
            err.to_string(),
            "ActivityRow.ts: duplicate PostgreSQL column `ts_us`"
        );
    }
}
```

- [ ] **Step 4: Wire the module and re-exports**

Modify `crates/kronika-source-pg/src/lib.rs` near the module declarations:

```rust
mod pg_row;

pub use pg_row::{PgCollectError, PgRowError};
```

- [ ] **Step 5: Run focused tests**

Run:

```bash
cargo test -p kronika-source-pg pg_row
```

Expected: all `pg_row` tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/kronika-source-pg/src/lib.rs crates/kronika-source-pg/src/pg_row.rs
git commit -m "Добавить основу pg row mapper"
```

## Task 2: Add `pg_row_mapper!` Macro

**Files:**
- Modify: `crates/kronika-source-pg/src/lib.rs`
- Test: `crates/kronika-source-pg/src/pg_row.rs`

- [ ] **Step 1: Add a macro smoke test before the macro exists**

Append this test to `crates/kronika-source-pg/src/pg_row.rs`:

```rust
#[cfg(test)]
mod macro_tests {
    #[derive(Debug, PartialEq)]
    struct DemoRow {
        ts: i64,
        optional: Option<i64>,
        gated: Option<i64>,
        renamed: i64,
    }

    #[derive(Debug, Clone, Copy)]
    enum DemoVersion {
        V1,
        V2,
    }

    pg_row_mapper! {
        DemoCols(version: DemoVersion) => DemoRow {
            ts: i64 = "ts_us",
            optional: Option<i64> = "optional_value",
            gated: Option<i64> = "gated_value"
                if matches!(version, DemoVersion::V2),
            renamed: i64 = {
                match version {
                    DemoVersion::V1 => "old_name",
                    DemoVersion::V2 => "new_name",
                }
            },
        }
    }

    #[test]
    fn macro_builds_versioned_column_map() {
        let cols = DemoCols::new_from_names(
            DemoVersion::V1,
            ["ts_us", "optional_value", "old_name"],
        )
        .expect("V1 columns should resolve");

        assert!(cols.gated.is_none());
    }

    #[test]
    fn macro_requires_gated_column_when_version_enables_it() {
        let err = DemoCols::new_from_names(
            DemoVersion::V2,
            ["ts_us", "optional_value", "new_name"],
        )
            .expect_err("V2 should require gated_value");

        assert_eq!(
            err.to_string(),
            "DemoRow.gated: missing PostgreSQL column `gated_value`"
        );
    }

    #[test]
    fn macro_uses_versioned_column_alias() {
        let err = DemoCols::new_from_names(
            DemoVersion::V2,
            ["ts_us", "optional_value", "gated_value", "old_name"],
        )
        .expect_err("V2 should require the new alias");

        assert_eq!(
            err.to_string(),
            "DemoRow.renamed: missing PostgreSQL column `new_name`"
        );
    }
}
```

- [ ] **Step 2: Run the macro tests to verify red**

Run:

```bash
cargo test -p kronika-source-pg macro_
```

Expected: compilation fails because `pg_row_mapper!` is not defined.

- [ ] **Step 3: Add the macro**

Add this macro to `crates/kronika-source-pg/src/lib.rs` after `marked!`:

```rust
macro_rules! pg_row_mapper {
    (
        $cols:ident($version:ident : $version_ty:ty) => $row_ty:ident {
            $(
                $field:ident : $ty:ty = $column:tt $(if $condition:expr)?
            ),+ $(,)?
        }
    ) => {
        struct $cols {
            $(
                $field: pg_row_mapper!(@col_ty $ty $(, $condition)?),
            )+
        }

        impl $cols {
            fn new(
                $version: $version_ty,
                columns: &[tokio_postgres::Column],
            ) -> Result<Self, crate::PgRowError> {
                Self::new_from_names($version, columns.iter().map(tokio_postgres::Column::name))
            }

            fn new_from_names<I, S>(
                $version: $version_ty,
                column_names: I,
            ) -> Result<Self, crate::PgRowError>
            where
                I: IntoIterator<Item = S>,
                S: AsRef<str>,
            {
                let column_names: Vec<S> = column_names.into_iter().collect();
                Ok(Self {
                    $(
                        $field: pg_row_mapper!(
                            @init
                            $version,
                            &column_names,
                            stringify!($row_ty),
                            stringify!($field),
                            $column,
                            $ty
                            $(, $condition)?
                        )?,
                    )+
                })
            }

            fn read(&self, row: &tokio_postgres::Row) -> Result<$row_ty, crate::PgRowError> {
                Ok($row_ty {
                    $(
                        $field: pg_row_mapper!(@read self, row, $field, $ty $(, $condition)?),
                    )+
                })
            }
        }
    };
    (@col_ty $ty:ty) => {
        crate::pg_row::PgCol<$ty>
    };
    (@col_ty $ty:ty, $condition:expr) => {
        Option<crate::pg_row::PgCol<$ty>>
    };
    (@column $version:ident, $column:literal) => {
        $column
    };
    (@column $version:ident, { $column:expr }) => {
        $column
    };
    (@init $version:ident, $columns:expr, $row:expr, $field:expr, $column:tt, $ty:ty) => {{
        let column = pg_row_mapper!(@column $version, $column);
        crate::pg_row::PgCol::<$ty>::required(
            $row,
            $field,
            column,
            ($columns).iter().map(AsRef::as_ref),
        )
    }};
    (@init $version:ident, $columns:expr, $row:expr, $field:expr, $column:tt, $ty:ty, $condition:expr) => {{
        if $condition {
            let column = pg_row_mapper!(@column $version, $column);
            crate::pg_row::PgCol::<$ty>::required(
                $row,
                $field,
                column,
                ($columns).iter().map(AsRef::as_ref),
            )
                .map(Some)
        } else {
            Ok(None)
        }
    }};
    (@read $self:ident, $row:ident, $field:ident, $ty:ty) => {
        $self.$field.get($row)?
    };
    (@read $self:ident, $row:ident, $field:ident, $ty:ty, $condition:expr) => {
        crate::pg_row::read_gated($self.$field.as_ref(), $row)?
    };
}
```

Keep `mod pg_row;` private in `lib.rs`. The macro can still refer to `crate::pg_row::PgCol` from inside crate modules.

- [ ] **Step 4: Run macro tests**

Run:

```bash
cargo test -p kronika-source-pg macro_
```

Expected: macro tests pass.

- [ ] **Step 5: Run `pg_row` tests**

Run:

```bash
cargo test -p kronika-source-pg pg_row
```

Expected: all `pg_row` tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/kronika-source-pg/src/lib.rs crates/kronika-source-pg/src/pg_row.rs
git commit -m "Добавить macro mapper для pg rows"
```

## Task 3: Convert `activity`

**Files:**
- Modify: `crates/kronika-source-pg/src/activity.rs`
- Modify: `bins/pg_kronika-collector/src/main.rs`

- [ ] **Step 1: Add `ActivityCols` mapping**

In `crates/kronika-source-pg/src/activity.rs`, add this mapping near `row_from_pg`:

```rust
pg_row_mapper! {
    ActivityCols(version: ActivityVersion) => ActivityRow {
        ts: i64 = "ts_us",
        pid: i32 = "pid",
        leader_pid: Option<i32> = "leader_pid"
            if matches!(version, ActivityVersion::V2 | ActivityVersion::V3),
        datname: Option<String> = "datname",
        usename: Option<String> = "usename",
        application_name: String = "application_name",
        client_addr: String = "client_addr",
        backend_type: String = "backend_type",
        state: Option<String> = "state",
        wait_event_type: Option<String> = "wait_event_type",
        wait_event: Option<String> = "wait_event",
        query: Option<String> = "query",
        query_id: Option<i64> = "query_id"
            if matches!(version, ActivityVersion::V3),
        backend_xid_age: Option<i64> = "backend_xid_age",
        backend_xmin_age: Option<i64> = "backend_xmin_age",
        backend_start: i64 = "backend_start_us",
        xact_start: Option<i64> = "xact_start_us",
        query_start: Option<i64> = "query_start_us",
        state_change: Option<i64> = "state_change_us",
    }
}
```

- [ ] **Step 2: Replace `collect_activity`**

Change `collect_activity` to:

```rust
pub async fn collect_activity(
    client: &Client,
    major: u32,
) -> Result<(ActivityVersion, Vec<ActivityRow>), crate::PgCollectError> {
    let version = activity_version(major);
    let stmt = client.prepare(activity_query(version)).await?;
    let cols = ActivityCols::new(version, stmt.columns())?;
    let rows = client.query(&stmt, &[]).await?;
    let parsed = rows
        .iter()
        .map(|row| cols.read(row))
        .collect::<Result<Vec<_>, _>>()?;
    Ok((version, parsed))
}
```

Delete the old `row_from_pg` function after this compiles.

- [ ] **Step 3: Update `pg_kronika-collector` activity error handling**

No extra branch is required in `collect_main_conn_sources`; it logs `err` and wraps with `anyhow::Context`. The `PgCollectError` `Display` and `Error` impls preserve enough context for logs and error chains.

Run:

```bash
cargo check -p pg_kronika-collector
```

Expected: failure only if imports or public re-exports are missing. Fix by importing `PgCollectError` only if the compiler requires it.

- [ ] **Step 4: Run focused source tests**

Run:

```bash
cargo test -p kronika-source-pg activity
```

Expected: activity tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/kronika-source-pg/src/activity.rs bins/pg_kronika-collector/src/main.rs
git commit -m "Перевести pg_stat_activity на pg row mapper"
```

## Task 4: Convert `statements` and Preserve SQLSTATE Access

**Files:**
- Modify: `crates/kronika-source-pg/src/statements.rs`
- Modify: `bins/pg_kronika-collector/src/main.rs`

- [ ] **Step 1: Add `StatementsCols` mapping**

In `crates/kronika-source-pg/src/statements.rs`, add this mapping near `row_from_pg`:

```rust
pg_row_mapper! {
    StatementsCols(version: StatementsVersion) => StatementsRow {
        ts: i64 = "ts_us",
        queryid: Option<i64> = "queryid",
        userid: u32 = "userid",
        dbid: u32 = "dbid",
        toplevel: bool = "toplevel"
            if matches!(
                version,
                StatementsVersion::V3
                    | StatementsVersion::V4
                    | StatementsVersion::V5
                    | StatementsVersion::V6
            ),
        datname: Option<String> = "datname",
        usename: Option<String> = "usename",
        query: Option<String> = "query",
        calls: i64 = "calls",
        rows: i64 = "rows",
        plans: i64 = "plans"
            if !matches!(version, StatementsVersion::V1),
        total_time: f64 = {
            match version {
                StatementsVersion::V1 => "total_time",
                _ => "total_exec_time",
            }
        },
        total_plan_time: f64 = "total_plan_time"
            if !matches!(version, StatementsVersion::V1),
        min_time: f64 = {
            match version {
                StatementsVersion::V1 => "min_time",
                _ => "min_exec_time",
            }
        },
        max_time: f64 = {
            match version {
                StatementsVersion::V1 => "max_time",
                _ => "max_exec_time",
            }
        },
        mean_time: f64 = {
            match version {
                StatementsVersion::V1 => "mean_time",
                _ => "mean_exec_time",
            }
        },
        stddev_time: f64 = {
            match version {
                StatementsVersion::V1 => "stddev_time",
                _ => "stddev_exec_time",
            }
        },
        min_plan_time: f64 = "min_plan_time"
            if !matches!(version, StatementsVersion::V1),
        max_plan_time: f64 = "max_plan_time"
            if !matches!(version, StatementsVersion::V1),
        mean_plan_time: f64 = "mean_plan_time"
            if !matches!(version, StatementsVersion::V1),
        stddev_plan_time: f64 = "stddev_plan_time"
            if !matches!(version, StatementsVersion::V1),
        shared_blks_hit: i64 = "shared_blks_hit",
        shared_blks_read: i64 = "shared_blks_read",
        shared_blks_dirtied: i64 = "shared_blks_dirtied",
        shared_blks_written: i64 = "shared_blks_written",
        local_blks_hit: i64 = "local_blks_hit",
        local_blks_read: i64 = "local_blks_read",
        local_blks_dirtied: i64 = "local_blks_dirtied",
        local_blks_written: i64 = "local_blks_written",
        temp_blks_read: i64 = "temp_blks_read",
        temp_blks_written: i64 = "temp_blks_written",
        blk_read_time: f64 = {
            match version {
                StatementsVersion::V5 | StatementsVersion::V6 => "shared_blk_read_time",
                _ => "blk_read_time",
            }
        },
        blk_write_time: f64 = {
            match version {
                StatementsVersion::V5 | StatementsVersion::V6 => "shared_blk_write_time",
                _ => "blk_write_time",
            }
        },
        local_blk_read_time: f64 = "local_blk_read_time"
            if matches!(version, StatementsVersion::V5 | StatementsVersion::V6),
        local_blk_write_time: f64 = "local_blk_write_time"
            if matches!(version, StatementsVersion::V5 | StatementsVersion::V6),
        temp_blk_read_time: f64 = "temp_blk_read_time"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        temp_blk_write_time: f64 = "temp_blk_write_time"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        wal_records: i64 = "wal_records"
            if !matches!(version, StatementsVersion::V1),
        wal_fpi: i64 = "wal_fpi"
            if !matches!(version, StatementsVersion::V1),
        wal_bytes: i64 = "wal_bytes"
            if !matches!(version, StatementsVersion::V1),
        wal_buffers_full: i64 = "wal_buffers_full"
            if matches!(version, StatementsVersion::V6),
        jit_functions: i64 = "jit_functions"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        jit_generation_time: f64 = "jit_generation_time"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        jit_inlining_count: i64 = "jit_inlining_count"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        jit_inlining_time: f64 = "jit_inlining_time"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        jit_optimization_count: i64 = "jit_optimization_count"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        jit_optimization_time: f64 = "jit_optimization_time"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        jit_emission_count: i64 = "jit_emission_count"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        jit_emission_time: f64 = "jit_emission_time"
            if matches!(
                version,
                StatementsVersion::V4 | StatementsVersion::V5 | StatementsVersion::V6
            ),
        jit_deform_count: i64 = "jit_deform_count"
            if matches!(version, StatementsVersion::V5 | StatementsVersion::V6),
        jit_deform_time: f64 = "jit_deform_time"
            if matches!(version, StatementsVersion::V5 | StatementsVersion::V6),
        parallel_workers_to_launch: i64 = "parallel_workers_to_launch"
            if matches!(version, StatementsVersion::V6),
        parallel_workers_launched: i64 = "parallel_workers_launched"
            if matches!(version, StatementsVersion::V6),
        stats_since: Option<i64> = "stats_since_us"
            if matches!(version, StatementsVersion::V5 | StatementsVersion::V6),
        minmax_stats_since: Option<i64> = "minmax_stats_since_us"
            if matches!(version, StatementsVersion::V5 | StatementsVersion::V6),
    }
}
```

- [ ] **Step 2: Replace `collect_statements`**

Change `collect_statements` to:

```rust
pub async fn collect_statements(
    client: &Client,
    version: StatementsVersion,
    max_statements: i64,
) -> Result<(Vec<StatementsRow>, u64), crate::PgCollectError> {
    let sql = statements_query(version);
    let stmt = client.prepare(&sql).await?;
    let cols = StatementsCols::new(version, stmt.columns())?;
    let rows = client.query(&stmt, &[&max_statements]).await?;
    let source_total = rows
        .first()
        .map_or(0, |row| row.get::<_, i64>("source_total"));
    Ok((
        rows.iter()
            .map(|row| cols.read(row))
            .collect::<Result<Vec<_>, _>>()?,
        u64::try_from(source_total).unwrap_or(0),
    ))
}
```

Delete the old `row_from_pg` after compilation passes.

- [ ] **Step 3: Preserve SQLSTATE access in collector**

In `bins/pg_kronika-collector/src/main.rs`, add helper near `is_sqlstate`:

```rust
fn collect_query_error(err: &kronika_source_pg::PgCollectError) -> Option<&tokio_postgres::Error> {
    err.as_query_error()
}
```

Use this helper only where SQLSTATE decisions are required. Existing statements paths currently log and skip; they do not branch on SQLSTATE, so no SQLSTATE branch is required for `collect_statements`.

- [ ] **Step 4: Run focused tests**

Run:

```bash
cargo test -p kronika-source-pg statements
```

Expected: statements tests pass.

- [ ] **Step 5: Run collector check**

Run:

```bash
cargo check -p pg_kronika-collector
```

Expected: collector compiles with `PgCollectError` displayed in existing logging.

- [ ] **Step 6: Commit**

```bash
git add crates/kronika-source-pg/src/statements.rs bins/pg_kronika-collector/src/main.rs crates/kronika-source-pg/src/lib.rs crates/kronika-source-pg/src/pg_row.rs
git commit -m "Перевести pg_stat_statements на pg row mapper"
```

## Task 5: Final Verification and Cleanup

**Files:**
- Modify: only files touched by previous tasks if verification finds lint issues.

- [ ] **Step 1: Scan for remaining pilot manual mapping**

Run:

```bash
rg -n "fn row_from_pg|row\\.get\\(" crates/kronika-source-pg/src/activity.rs crates/kronika-source-pg/src/statements.rs
```

Expected: no `fn row_from_pg`; remaining `row.get` only where intentionally outside the mapper, such as `statements_extversion` or `source_total` if not migrated to `try_get`.

- [ ] **Step 2: Run formatting check**

Run:

```bash
cargo fmt --all --check
```

Expected: pass. If it fails, run `cargo fmt --all`, inspect the diff, and commit formatting with the relevant task commit if possible.

- [ ] **Step 3: Run clippy for affected crates**

Run:

```bash
cargo clippy -p kronika-source-pg -p pg_kronika-collector --all-targets -- -D warnings
```

Expected: pass.

- [ ] **Step 4: Run workspace tests**

Run:

```bash
cargo test --workspace
```

Expected: pass.

- [ ] **Step 5: Run dependency check**

Run:

```bash
cargo run -p xtask -- check-deps
```

Expected: pass.

- [ ] **Step 6: Review memory bounds and comments**

Check the diff manually:

```bash
git diff main...HEAD -- crates/kronika-source-pg/src/pg_row.rs crates/kronika-source-pg/src/activity.rs crates/kronika-source-pg/src/statements.rs bins/pg_kronika-collector/src/main.rs
```

Expected:

- `PgCol` stores only `usize` plus static metadata per mapped field.
- No per-row allocation added by the mapper beyond existing owned raw row construction.
- Comments explain invariants or contracts only; no line-by-line narration.

- [ ] **Step 7: Commit final cleanup when verification changed files**

If verification required changes:

```bash
git add crates/kronika-source-pg/src bins/pg_kronika-collector/src/main.rs
git commit -m "Почистить pg row mapper после проверок"
```

If verification required no changes, do not create an empty commit.
