//! `pg_settings` collection for section `1_019_001`.
//!
//! One query on the main connection returns every parameter of the running
//! server. The layout is stable across PG10-18. Rows arrive sorted by `name`,
//! so interning in arrival order keeps the `name` sort key meaningful.

use kronika_registry::pg_settings::PgSettingsV1;
use kronika_registry::{StrId, Ts};
use tokio_postgres::Client;

/// Prefix a query literal with the collector marker.
macro_rules! marked {
    ($sql:literal) => {
        concat!(
            "/* pg_kronika:",
            env!("CARGO_PKG_VERSION"),
            " crates/kronika-source-pg/src/settings.rs */ ",
            $sql,
        )
    };
}

/// One `pg_settings` row before interning.
#[derive(Debug, Clone)]
pub struct SettingsRow {
    /// Collection time, unix microseconds.
    pub ts: i64,
    /// Parameter name.
    pub name: String,
    /// Running value, in `unit` units.
    pub setting: String,
    /// Unit of `setting`; `None` for unitless parameters.
    pub unit: Option<String>,
    /// How the running value was set.
    pub source: String,
    /// Config file that set the value.
    pub sourcefile: Option<String>,
    /// Line within `sourcefile`.
    pub sourceline: Option<i32>,
    /// The value changed but takes effect only after a restart.
    pub pending_restart: bool,
    /// Required context to change the value.
    pub context: String,
    /// Value type.
    pub vartype: String,
    /// Compiled-in default.
    pub boot_val: Option<String>,
    /// Value `RESET` would restore.
    pub reset_val: Option<String>,
}

/// Collect every `pg_settings` row, sorted by name.
///
/// # Errors
/// Returns the [`tokio_postgres::Error`] if the query fails.
pub async fn collect_settings(client: &Client) -> Result<Vec<SettingsRow>, tokio_postgres::Error> {
    let rows = client
        .query(
            marked!(
                "SELECT \
                     (extract(epoch from statement_timestamp()) * 1e6)::int8 AS ts_us, \
                     name, \
                     setting, \
                     unit, \
                     source, \
                     sourcefile, \
                     sourceline, \
                     pending_restart, \
                     context, \
                     vartype, \
                     boot_val, \
                     reset_val \
                 FROM pg_settings \
                 ORDER BY name"
            ),
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| SettingsRow {
            ts: row.get("ts_us"),
            name: row.get("name"),
            setting: row.get("setting"),
            unit: row.get("unit"),
            source: row.get("source"),
            sourcefile: row.get("sourcefile"),
            sourceline: row.get("sourceline"),
            pending_restart: row.get("pending_restart"),
            context: row.get("context"),
            vartype: row.get("vartype"),
            boot_val: row.get("boot_val"),
            reset_val: row.get("reset_val"),
        })
        .collect())
}

/// Intern the row's strings and build the registry row.
///
/// # Errors
/// Propagates the interner error when the dictionary is full.
pub fn to_settings_v1<E>(
    row: &SettingsRow,
    mut intern: impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<PgSettingsV1, E> {
    Ok(PgSettingsV1 {
        ts: Ts(row.ts),
        name: intern(row.name.as_bytes())?,
        setting: intern(row.setting.as_bytes())?,
        unit: intern_opt(row.unit.as_deref(), &mut intern)?,
        source: intern(row.source.as_bytes())?,
        sourcefile: intern_opt(row.sourcefile.as_deref(), &mut intern)?,
        sourceline: row.sourceline,
        pending_restart: row.pending_restart,
        context: intern(row.context.as_bytes())?,
        vartype: intern(row.vartype.as_bytes())?,
        boot_val: intern_opt(row.boot_val.as_deref(), &mut intern)?,
        reset_val: intern_opt(row.reset_val.as_deref(), &mut intern)?,
    })
}

/// Intern an optional string, keeping `None` as `None`.
fn intern_opt<E>(
    value: Option<&str>,
    intern: &mut impl FnMut(&[u8]) -> Result<StrId, E>,
) -> Result<Option<StrId>, E> {
    value.map(|s| intern(s.as_bytes())).transpose()
}

#[cfg(test)]
mod tests {
    use super::{SettingsRow, to_settings_v1};
    use kronika_registry::{StrId, Ts};

    #[test]
    fn to_settings_v1_interns_in_field_order_and_keeps_nulls() {
        let row = SettingsRow {
            ts: 5,
            name: "work_mem".to_owned(),
            setting: "4096".to_owned(),
            unit: Some("kB".to_owned()),
            source: "default".to_owned(),
            sourcefile: None,
            sourceline: None,
            pending_restart: false,
            context: "user".to_owned(),
            vartype: "integer".to_owned(),
            boot_val: Some("4096".to_owned()),
            reset_val: Some("4096".to_owned()),
        };
        let mut next = 0_u64;
        let sealed = to_settings_v1::<()>(&row, |_| {
            next += 1;
            Ok(StrId(next))
        })
        .expect("interner never fails here");
        assert_eq!(sealed.ts, Ts(5));
        assert_eq!(sealed.name, StrId(1));
        assert_eq!(sealed.setting, StrId(2));
        assert_eq!(sealed.unit, Some(StrId(3)));
        assert_eq!(sealed.sourcefile, None);
        assert_eq!(sealed.sourceline, None);
        assert!(!sealed.pending_restart);
        assert_eq!(sealed.boot_val, Some(StrId(7)));
        assert_eq!(sealed.reset_val, Some(StrId(8)));
    }
}
