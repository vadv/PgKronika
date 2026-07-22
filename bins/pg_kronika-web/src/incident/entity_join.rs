//! Typed, request-scoped cross-section entity joins.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EntityDomain {
    Postgres,
    #[cfg(test)]
    Linux,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum EntityName {
    BackendSession,
    #[cfg(test)]
    Relation,
    #[cfg(test)]
    Database,
    #[cfg(test)]
    Process,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntityJoinActivation {
    /// Both input families carry one producer-proven shared snapshot token.
    SharedSnapshot,
    /// One bounded producer row stores both typed identities and its snapshot.
    SnapshotRelation,
    /// A bounded mapping stores both typed identities and an overlap interval.
    LifetimeMapping,
}

impl EntityJoinActivation {
    pub(crate) const fn producer(self) -> &'static str {
        match self {
            Self::SharedSnapshot => "shared_snapshot_producer",
            Self::SnapshotRelation => "typed_relation_producer",
            Self::LifetimeMapping => "stored_mapping_producer",
        }
    }

    pub(crate) const fn provenance(self) -> &'static str {
        match self {
            Self::SharedSnapshot => "shared_snapshot_token",
            Self::SnapshotRelation => "snapshot_scoped_relation",
            Self::LifetimeMapping => "overlapping_lifetime_mapping",
        }
    }

    pub(crate) const fn coverage(self) -> &'static str {
        match self {
            Self::SharedSnapshot => "both_inputs_complete",
            Self::SnapshotRelation => "relation_and_inputs_complete",
            Self::LifetimeMapping => "mapping_and_inputs_complete",
        }
    }
}

/// The producer and coverage work required before one cross-section branch can
/// claim an entity relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntityJoinContract {
    QueryDatabaseTemp,
    RelationQueryPlan,
    RelationVacuum,
    RelationVacuumHorizon,
    RelationIndexWal,
    QueryWalCheckpoint,
    RelationQueryCache,
    PgIoBlockDevice,
    BackendRelationHorizon,
    DatabaseBackend,
    ReplicationWal,
    SlotFilesystem,
    ArchiveFilesystem,
    BackendReplication,
    ActivityLockWaiter,
    HostPgCpu,
    BackendCgroupCpu,
    HostPgMemory,
    BackendCgroupMemory,
    PgStorageBlockDevice,
    WriterBlockDevice,
    ProcessCgroupDevice,
    PgStorageFilesystem,
    PgEndpointNetwork,
}

impl EntityJoinContract {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::QueryDatabaseTemp => "query_database_temp",
            Self::RelationQueryPlan => "relation_query_plan",
            Self::RelationVacuum => "relation_vacuum",
            Self::RelationVacuumHorizon => "relation_vacuum_horizon",
            Self::RelationIndexWal => "relation_index_wal",
            Self::QueryWalCheckpoint => "query_wal_checkpoint",
            Self::RelationQueryCache => "relation_query_cache",
            Self::PgIoBlockDevice => "pg_io_block_device",
            Self::BackendRelationHorizon => "backend_relation_horizon",
            Self::DatabaseBackend => "database_backend",
            Self::ReplicationWal => "replication_wal",
            Self::SlotFilesystem => "slot_filesystem",
            Self::ArchiveFilesystem => "archive_filesystem",
            Self::BackendReplication => "backend_replication",
            Self::ActivityLockWaiter => "activity_lock_waiter",
            Self::HostPgCpu => "host_pg_cpu",
            Self::BackendCgroupCpu => "backend_cgroup_cpu",
            Self::HostPgMemory => "host_pg_memory",
            Self::BackendCgroupMemory => "backend_cgroup_memory",
            Self::PgStorageBlockDevice => "pg_storage_block_device",
            Self::WriterBlockDevice => "writer_block_device",
            Self::ProcessCgroupDevice => "process_cgroup_device",
            Self::PgStorageFilesystem => "pg_storage_filesystem",
            Self::PgEndpointNetwork => "pg_endpoint_network",
        }
    }

    pub(crate) const fn activation(self) -> EntityJoinActivation {
        match self {
            Self::ActivityLockWaiter => EntityJoinActivation::SharedSnapshot,
            Self::QueryDatabaseTemp
            | Self::RelationQueryPlan
            | Self::RelationVacuum
            | Self::RelationVacuumHorizon
            | Self::RelationIndexWal
            | Self::QueryWalCheckpoint
            | Self::RelationQueryCache
            | Self::BackendRelationHorizon
            | Self::DatabaseBackend
            | Self::ReplicationWal
            | Self::BackendReplication => EntityJoinActivation::SnapshotRelation,
            Self::PgIoBlockDevice
            | Self::SlotFilesystem
            | Self::ArchiveFilesystem
            | Self::HostPgCpu
            | Self::BackendCgroupCpu
            | Self::HostPgMemory
            | Self::BackendCgroupMemory
            | Self::PgStorageBlockDevice
            | Self::WriterBlockDevice
            | Self::ProcessCgroupDevice
            | Self::PgStorageFilesystem
            | Self::PgEndpointNetwork => EntityJoinActivation::LifetimeMapping,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct EntityScope<'a> {
    source_id: u64,
    node_self_id: &'a str,
}

impl<'a> EntityScope<'a> {
    pub(crate) const fn new(source_id: u64, node_self_id: &'a str) -> Option<Self> {
        if node_self_id.is_empty() {
            None
        } else {
            Some(Self {
                source_id,
                node_self_id,
            })
        }
    }

    fn matches(self, other: Self) -> bool {
        self.source_id == other.source_id && self.node_self_id == other.node_self_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum EntityValue {
    I64Pair(i64, i64),
    #[cfg(test)]
    I64(i64),
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TypedEntityIdentity {
    domain: EntityDomain,
    name: EntityName,
    value: EntityValue,
}

impl TypedEntityIdentity {
    pub(crate) const fn postgres_backend_session(pid: i64, backend_start: i64) -> Option<Self> {
        if pid <= 0 || backend_start <= 0 {
            return None;
        }
        Some(Self {
            domain: EntityDomain::Postgres,
            name: EntityName::BackendSession,
            value: EntityValue::I64Pair(pid, backend_start),
        })
    }

    #[cfg(test)]
    const fn scalar(domain: EntityDomain, name: EntityName, value: i64) -> Self {
        Self {
            domain,
            name,
            value: EntityValue::I64(value),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct EntityJoinKey {
    snapshot_token: i64,
    observed_at_us: i64,
    identity: TypedEntityIdentity,
}

impl EntityJoinKey {
    pub(crate) const fn shared_snapshot(
        snapshot_token: i64,
        observed_at_us: i64,
        identity: TypedEntityIdentity,
    ) -> Self {
        Self {
            snapshot_token,
            observed_at_us,
            identity,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntityJoinInsert {
    Inserted,
    Duplicate,
    LimitExceeded { observed: usize, limit: usize },
}

/// Exact-key index bounded by the number of already-admitted input relations.
pub(crate) struct EntityJoinIndex<'scope> {
    scope: EntityScope<'scope>,
    entries: BTreeMap<EntityJoinKey, BTreeSet<i64>>,
    relation_count: usize,
    relation_limit: usize,
}

impl<'scope> EntityJoinIndex<'scope> {
    pub(crate) const fn new(scope: EntityScope<'scope>, relation_limit: usize) -> Self {
        Self {
            scope,
            entries: BTreeMap::new(),
            relation_count: 0,
            relation_limit,
        }
    }

    pub(crate) fn insert(&mut self, key: EntityJoinKey, related_id: i64) -> EntityJoinInsert {
        if self
            .entries
            .get(&key)
            .is_some_and(|related| related.contains(&related_id))
        {
            return EntityJoinInsert::Duplicate;
        }
        let Some(observed) = self.relation_count.checked_add(1) else {
            return EntityJoinInsert::LimitExceeded {
                observed: usize::MAX,
                limit: self.relation_limit,
            };
        };
        if observed > self.relation_limit {
            return EntityJoinInsert::LimitExceeded {
                observed,
                limit: self.relation_limit,
            };
        }
        self.entries.entry(key).or_default().insert(related_id);
        self.relation_count = observed;
        EntityJoinInsert::Inserted
    }

    pub(crate) fn matches(
        &self,
        scope: EntityScope<'_>,
        key: &EntityJoinKey,
    ) -> Option<&BTreeSet<i64>> {
        self.scope
            .matches(scope)
            .then(|| self.entries.get(key))
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(source_id: u64, node_self_id: &str) -> EntityScope<'_> {
        EntityScope::new(source_id, node_self_id).expect("non-empty node identity")
    }

    fn backend(snapshot: i64, pid: i64, backend_start: i64) -> EntityJoinKey {
        EntityJoinKey::shared_snapshot(
            snapshot,
            snapshot,
            TypedEntityIdentity::postgres_backend_session(pid, backend_start)
                .expect("valid backend session"),
        )
    }

    #[test]
    fn activation_requirements_have_stable_machine_ids() {
        let actual = [
            EntityJoinActivation::SharedSnapshot,
            EntityJoinActivation::SnapshotRelation,
            EntityJoinActivation::LifetimeMapping,
        ]
        .map(|activation| {
            (
                activation.producer(),
                activation.provenance(),
                activation.coverage(),
            )
        });
        assert_eq!(
            actual,
            [
                (
                    "shared_snapshot_producer",
                    "shared_snapshot_token",
                    "both_inputs_complete",
                ),
                (
                    "typed_relation_producer",
                    "snapshot_scoped_relation",
                    "relation_and_inputs_complete",
                ),
                (
                    "stored_mapping_producer",
                    "overlapping_lifetime_mapping",
                    "mapping_and_inputs_complete",
                ),
            ]
        );
    }

    #[test]
    fn exact_scoped_snapshot_session_join_is_observable_and_ordered() {
        let request = scope(7, "node-a");
        let key = backend(100, 42, 10);
        let mut index = EntityJoinIndex::new(request, 2);
        assert_eq!(index.insert(key.clone(), 9), EntityJoinInsert::Inserted);
        assert_eq!(index.insert(key.clone(), 3), EntityJoinInsert::Inserted);
        assert_eq!(
            index
                .matches(request, &key)
                .expect("exact join")
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [3, 9]
        );
    }

    #[test]
    fn zero_relation_limit_rejects_without_creating_a_lookup_entry() {
        let request = scope(7, "node-a");
        let key = backend(100, 42, 10);
        let mut index = EntityJoinIndex::new(request, 0);
        assert_eq!(
            index.insert(key.clone(), 9),
            EntityJoinInsert::LimitExceeded {
                observed: 1,
                limit: 0,
            }
        );
        assert!(index.matches(request, &key).is_none());
    }

    #[test]
    fn equal_numbers_in_other_names_or_domains_do_not_join() {
        let request = scope(7, "node-a");
        let relation = EntityJoinKey::shared_snapshot(
            100,
            100,
            TypedEntityIdentity::scalar(EntityDomain::Postgres, EntityName::Relation, 42),
        );
        let database = EntityJoinKey::shared_snapshot(
            100,
            100,
            TypedEntityIdentity::scalar(EntityDomain::Postgres, EntityName::Database, 42),
        );
        let process = EntityJoinKey::shared_snapshot(
            100,
            100,
            TypedEntityIdentity::scalar(EntityDomain::Linux, EntityName::Process, 42),
        );
        let mut index = EntityJoinIndex::new(request, 1);
        assert_eq!(index.insert(relation, 1), EntityJoinInsert::Inserted);
        assert!(index.matches(request, &database).is_none());
        assert!(index.matches(request, &process).is_none());
    }

    #[test]
    fn source_node_snapshot_time_and_lifetime_are_part_of_the_match() {
        let request = scope(7, "node-a");
        let key = backend(100, 42, 10);
        let mut index = EntityJoinIndex::new(request, 1);
        assert_eq!(index.insert(key.clone(), 1), EntityJoinInsert::Inserted);
        assert!(index.matches(scope(8, "node-a"), &key).is_none());
        assert!(index.matches(scope(7, "node-b"), &key).is_none());
        assert!(index.matches(request, &backend(101, 42, 10)).is_none());
        assert!(index.matches(request, &backend(100, 42, 11)).is_none());

        let same_token_other_time = EntityJoinKey::shared_snapshot(
            100,
            101,
            TypedEntityIdentity::postgres_backend_session(42, 10).expect("valid session"),
        );
        assert!(index.matches(request, &same_token_other_time).is_none());
    }

    #[test]
    fn oid_reuse_requires_a_new_snapshot_provenance() {
        let request = scope(7, "node-a");
        let relation = |snapshot| {
            EntityJoinKey::shared_snapshot(
                snapshot,
                snapshot,
                TypedEntityIdentity::scalar(EntityDomain::Postgres, EntityName::Relation, 42),
            )
        };
        let mut index = EntityJoinIndex::new(request, 1);
        assert_eq!(index.insert(relation(100), 1), EntityJoinInsert::Inserted);
        assert!(index.matches(request, &relation(200)).is_none());
    }

    #[test]
    fn invalid_or_missing_backend_identity_cannot_form_a_key() {
        assert!(TypedEntityIdentity::postgres_backend_session(0, 1).is_none());
        assert!(TypedEntityIdentity::postgres_backend_session(1, 0).is_none());
        assert!(TypedEntityIdentity::postgres_backend_session(-1, 1).is_none());
        assert!(EntityScope::new(7, "").is_none());
    }

    #[test]
    fn duplicate_relations_do_not_consume_the_bound() {
        let request = scope(7, "node-a");
        let key = backend(100, 42, 10);
        let mut index = EntityJoinIndex::new(request, 1);
        assert_eq!(index.insert(key.clone(), 9), EntityJoinInsert::Inserted);
        assert_eq!(index.insert(key.clone(), 9), EntityJoinInsert::Duplicate);
        assert_eq!(
            index.insert(key, 10),
            EntityJoinInsert::LimitExceeded {
                observed: 2,
                limit: 1,
            }
        );
        assert_eq!(
            index
                .matches(request, &backend(100, 42, 10))
                .expect("first admitted relation")
                .iter()
                .copied()
                .collect::<Vec<_>>(),
            [9]
        );
    }

    #[test]
    fn relation_count_overflow_is_rejected() {
        let request = scope(7, "node-a");
        let mut index = EntityJoinIndex::new(request, usize::MAX);
        index.relation_count = usize::MAX;
        let key = backend(100, 42, 10);
        assert_eq!(
            index.insert(key.clone(), 9),
            EntityJoinInsert::LimitExceeded {
                observed: usize::MAX,
                limit: usize::MAX,
            }
        );
        assert!(index.matches(request, &key).is_none());
    }
}
