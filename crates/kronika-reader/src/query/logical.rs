//! Union view of a logical section across its layout versions.

use kronika_registry::{ColumnClass, ColumnType, registry};

/// One column in a logical section's union schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogicalColumn {
    /// Column name.
    pub name: &'static str,
    /// On-disk value type (from the first version that introduces the column).
    pub ty: ColumnType,
    /// The column's role; `Cumulative` marks a counter the diff layer rates.
    pub class: ColumnClass,
}

/// A logical section: the union of all layout versions sharing the same name.
#[derive(Debug, Clone)]
pub struct LogicalSection {
    /// Source name, e.g. `"pg_stat_activity"`.
    pub name: &'static str,
    /// Raw type ids of all versions, ascending.
    pub type_ids: Vec<u32>,
    /// Union of columns across all versions, in first-appearance order
    /// (scanning versions by ascending `type_id`).
    pub columns: Vec<LogicalColumn>,
    /// Sort key, identical across all versions.
    pub sort_key: &'static [&'static str],
    /// Union of every version's identity columns (the diff series key). A
    /// version that lacks a later identity column, e.g. `toplevel`, fills it
    /// with NULL, so pre- and post-upgrade rows land in distinct series.
    pub identity: Vec<&'static str>,
}

impl LogicalSection {
    /// Columns that key a diff series.
    ///
    /// The declared `identity`, or the sort key without `ts` when no identity is
    /// declared. The sort-key fallback names the entity for sections whose sort
    /// key is `(entity…, ts)` — the common case, one row per entity per snapshot
    /// — while `identity` overrides it where the sort key alone is insufficient
    /// (`pg_stat_statements` sorts by dbid/userid, but the entity also needs
    /// queryid/toplevel). A singleton (sort key `ts` only) yields an empty key:
    /// one series for the whole section, which is correct for it.
    #[must_use]
    pub fn diff_key(&self) -> Vec<&'static str> {
        if self.identity.is_empty() {
            self.sort_key
                .iter()
                .copied()
                .filter(|column| *column != "ts")
                .collect()
        } else {
            self.identity.clone()
        }
    }
}

/// Build the union view of a logical section by name.
///
/// Returns `None` when no registered contract carries that name.
///
/// # Panics
///
/// Panics with a diagnostic when the registry contains contracts for this
/// name that disagree on a column's type or on the sort key — a registry
/// invariant violation that must be fixed in the registry, not coded around.
#[must_use]
pub fn logical_section(name: &str) -> Option<LogicalSection> {
    // Collect all contracts for this name, sorted ascending by type_id.
    let mut contracts: Vec<_> = registry().iter().filter(|c| c.name == name).collect();

    if contracts.is_empty() {
        return None;
    }

    contracts.sort_by_key(|c| c.type_id.get());

    // Validate sort_key consistency across versions.
    let sort_key = contracts[0].sort_key;
    for contract in &contracts[1..] {
        assert!(
            contract.sort_key == sort_key,
            "registry violation: logical section {:?} has inconsistent sort_key \
             across versions — type_id {} has {:?}, type_id {} has {:?}",
            name,
            contracts[0].type_id.get(),
            sort_key,
            contract.type_id.get(),
            contract.sort_key,
        );
    }

    // Build union of columns by first appearance; reject type conflicts.
    let mut columns: Vec<LogicalColumn> = Vec::new();
    for contract in &contracts {
        for col in contract.columns {
            if let Some(existing) = columns.iter().find(|lc| lc.name == col.name) {
                assert!(
                    existing.ty == col.ty,
                    "registry violation: logical section {:?} column {:?} has \
                     conflicting types — first seen as {:?}, but type_id {} declares {:?}",
                    name,
                    col.name,
                    existing.ty,
                    contract.type_id.get(),
                    col.ty,
                );
                assert!(
                    existing.class == col.class,
                    "registry violation: logical section {:?} column {:?} has \
                     conflicting classes — first seen as {:?}, but type_id {} declares {:?}",
                    name,
                    col.name,
                    existing.class,
                    contract.type_id.get(),
                    col.class,
                );
            } else {
                columns.push(LogicalColumn {
                    name: col.name,
                    ty: col.ty,
                    class: col.class,
                });
            }
        }
    }

    // Identity is the union of the versions' identity columns: a later version
    // that adds an identity column widens the key, and older rows fill it NULL.
    let mut identity: Vec<&'static str> = Vec::new();
    for contract in &contracts {
        for &id_name in contract.identity {
            if !identity.contains(&id_name) {
                identity.push(id_name);
            }
        }
    }

    let type_ids = contracts.iter().map(|c| c.type_id.get()).collect();

    Some(LogicalSection {
        name: contracts[0].name,
        type_ids,
        columns,
        sort_key,
        identity,
    })
}

#[cfg(test)]
mod tests {
    use kronika_registry::registry;

    use super::logical_section;

    #[test]
    fn returns_none_for_unknown_name() {
        assert!(logical_section("no_such_name").is_none());
    }

    #[test]
    fn registry_wide_invariant_all_multi_version_names_succeed() {
        // Collect all distinct names that appear in ≥2 contracts.
        let mut names: Vec<&'static str> = registry().iter().map(|c| c.name).collect();
        names.sort_unstable();
        names.dedup();

        for name in names {
            let count = registry().iter().filter(|c| c.name == name).count();
            if count >= 2 {
                // Must not panic — a panic here means a registry violation.
                let section = logical_section(name)
                    .unwrap_or_else(|| panic!("logical_section({name:?}) returned None"));
                assert!(
                    section.type_ids.len() >= 2,
                    "expected ≥2 type_ids for {name:?}"
                );
                // type_ids must be ascending.
                for window in section.type_ids.windows(2) {
                    assert!(
                        window[0] < window[1],
                        "type_ids not ascending for {name:?}: {window:?}"
                    );
                }
                assert!(
                    !section.columns.is_empty(),
                    "union columns must not be empty for {name:?}"
                );
                assert!(
                    !section.sort_key.is_empty(),
                    "sort_key must not be empty for {name:?}"
                );
            }
        }
    }

    #[test]
    fn pg_stat_statements_identity_unions_and_classes_resolve() {
        use kronika_registry::ColumnClass;

        let section = logical_section("pg_stat_statements")
            .expect("pg_stat_statements must be in the registry");

        // V1/V2 identity is (queryid, userid, dbid); V3+ widen it with toplevel.
        assert_eq!(
            section.identity,
            vec!["queryid", "userid", "dbid", "toplevel"]
        );

        let class = |name: &str| {
            section
                .columns
                .iter()
                .find(|c| c.name == name)
                .map(|c| c.class)
        };
        assert_eq!(class("calls"), Some(ColumnClass::Cumulative));
        assert_eq!(class("queryid"), Some(ColumnClass::Label));
    }

    #[test]
    fn diff_key_uses_identity_or_falls_back_to_sort_key_without_ts() {
        // A declared identity is used verbatim.
        let stmt =
            logical_section("pg_stat_statements").expect("pg_stat_statements in the registry");
        assert_eq!(
            stmt.diff_key(),
            vec!["queryid", "userid", "dbid", "toplevel"]
        );

        // os_cpu declares no identity; the entity is the sort key minus `ts`, so
        // each core stays its own series instead of collapsing into one.
        let os_cpu = logical_section("os_cpu").expect("os_cpu in the registry");
        assert!(os_cpu.identity.is_empty());
        assert_eq!(os_cpu.diff_key(), vec!["cpu_id"]);

        // A singleton (sort key `ts` only) has an empty key: one series.
        let wal = logical_section("pg_stat_wal").expect("pg_stat_wal in the registry");
        assert!(wal.diff_key().is_empty());
    }

    #[test]
    fn pg_stat_activity_union_is_correct() {
        let section =
            logical_section("pg_stat_activity").expect("pg_stat_activity must be in the registry");

        // Three layout versions: 1_001_001, 1_001_002, 1_001_003.
        assert_eq!(section.type_ids, vec![1_001_001, 1_001_002, 1_001_003]);

        // type_ids are ascending.
        for window in section.type_ids.windows(2) {
            assert!(window[0] < window[1], "type_ids not ascending: {window:?}");
        }

        // Sort key is consistent across all versions.
        assert_eq!(section.sort_key, ["ts", "pid"]);

        // Columns shared by all versions appear (from V1).
        let col_names: Vec<_> = section.columns.iter().map(|c| c.name).collect();
        assert!(col_names.contains(&"ts"), "ts column must be present");
        assert!(col_names.contains(&"pid"), "pid column must be present");
        assert!(
            col_names.contains(&"datname"),
            "datname column must be present"
        );

        // V2-specific column (leader_pid added in PG13) must appear.
        assert!(
            col_names.contains(&"leader_pid"),
            "leader_pid (V2+) must be in the union"
        );

        // V3-specific column (query_id added in PG14) must appear.
        assert!(
            col_names.contains(&"query_id"),
            "query_id (V3 only) must be in the union"
        );

        // V1 columns precede version-specific additions (first-appearance order).
        let ts_pos = col_names.iter().position(|&n| n == "ts").unwrap();
        let leader_pos = col_names.iter().position(|&n| n == "leader_pid").unwrap();
        let query_id_pos = col_names.iter().position(|&n| n == "query_id").unwrap();
        assert!(ts_pos < leader_pos, "ts (V1) must precede leader_pid (V2)");
        assert!(
            leader_pos < query_id_pos,
            "leader_pid (V2) must precede query_id (V3)"
        );
    }
}
