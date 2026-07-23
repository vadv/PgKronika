//! Byte-bounded in-memory cache of decoded fact blocks.
//!
//! The cache keys a decoded block by its [`FactKey`] and its directory-entry
//! identity, so two blocks from different files or different positions never
//! collide. Eviction is bounded by the summed decoded byte weight, not by entry
//! count: a working set of a few large blocks and a working set of many small
//! blocks both respect the same ceiling. A block heavier than the whole budget
//! is never stored — the caller streams it instead.
//!
//! The cache holds only immutable decoded values behind [`Arc`]; it carries no
//! health, notable, or response semantics.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use super::container::BlockDirectoryEntry;
use super::factkey::FactKey;

/// The identity of one decoded block within one fact file.
///
/// A directory entry's kind, logical id, offset, and CRC together name an exact
/// stored block, so the same key always denotes the same immutable bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockSlot {
    /// The fact file the block belongs to.
    pub fact_key: FactKey,
    /// The block kind code.
    pub block_kind: u32,
    /// The block's logical factor/source id.
    pub logical_id: u32,
    /// The block's byte offset in the file.
    pub offset: u64,
    /// The block's stored-bytes CRC.
    pub crc32c: u32,
}

impl BlockSlot {
    /// Names the block a directory entry describes inside `fact_key`.
    #[must_use]
    pub const fn of(fact_key: FactKey, entry: &BlockDirectoryEntry) -> Self {
        Self {
            fact_key,
            block_kind: entry.block_kind,
            logical_id: entry.logical_id,
            offset: entry.offset,
            crc32c: entry.block_crc32c,
        }
    }
}

/// One resident decoded block and its byte weight.
#[derive(Debug)]
struct Resident<V> {
    value: Arc<V>,
    weight: usize,
    recency: u64,
}

/// A byte-bounded LRU cache of decoded fact blocks.
#[derive(Debug)]
pub struct BoundedFactCache<V> {
    budget_bytes: usize,
    used_bytes: usize,
    next_recency: u64,
    residents: HashMap<BlockSlot, Resident<V>>,
    by_recency: BTreeMap<u64, BlockSlot>,
}

impl<V> BoundedFactCache<V> {
    /// Builds a cache that holds at most `budget_bytes` of decoded blocks.
    #[must_use]
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            budget_bytes,
            used_bytes: 0,
            next_recency: 0,
            residents: HashMap::new(),
            by_recency: BTreeMap::new(),
        }
    }

    /// The configured byte ceiling.
    #[must_use]
    pub const fn budget_bytes(&self) -> usize {
        self.budget_bytes
    }

    /// The summed decoded weight currently resident.
    #[must_use]
    pub const fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// The number of resident blocks.
    #[must_use]
    pub fn len(&self) -> usize {
        self.residents.len()
    }

    /// Whether the cache holds no blocks.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.residents.is_empty()
    }

    /// Returns the resident block for `slot`, marking it most recently used.
    pub fn get(&mut self, slot: &BlockSlot) -> Option<Arc<V>> {
        let recency = self.next_recency;
        let resident = self.residents.get_mut(slot)?;
        self.by_recency.remove(&resident.recency);
        resident.recency = recency;
        self.by_recency.insert(recency, *slot);
        self.next_recency += 1;
        Some(Arc::clone(&resident.value))
    }

    /// Whether `slot` is resident, without changing recency.
    #[must_use]
    pub fn contains(&self, slot: &BlockSlot) -> bool {
        self.residents.contains_key(slot)
    }

    /// Inserts a decoded block of `weight` bytes, evicting to stay in budget.
    ///
    /// A block heavier than the whole budget is rejected and not stored; the
    /// caller must stream it. Returns whether the block became resident.
    pub fn insert(&mut self, slot: BlockSlot, value: Arc<V>, weight: usize) -> bool {
        if let Some(previous) = self.residents.remove(&slot) {
            self.by_recency.remove(&previous.recency);
            self.used_bytes -= previous.weight;
        }
        if weight > self.budget_bytes {
            return false;
        }
        self.evict_until_fits(weight);
        let recency = self.next_recency;
        self.next_recency += 1;
        self.used_bytes += weight;
        self.by_recency.insert(recency, slot);
        self.residents.insert(
            slot,
            Resident {
                value,
                weight,
                recency,
            },
        );
        true
    }

    /// Evicts least-recently-used blocks until `incoming` bytes fit.
    fn evict_until_fits(&mut self, incoming: usize) {
        while self.used_bytes + incoming > self.budget_bytes {
            let Some((&recency, &slot)) = self.by_recency.iter().next() else {
                break;
            };
            self.by_recency.remove(&recency);
            if let Some(resident) = self.residents.remove(&slot) {
                self.used_bytes -= resident.weight;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use kronika_analytics::overview::SourceScopeId;

    use super::super::block::{BlockCodec, BlockFlags};
    use super::*;
    use crate::SourceDescriptor;

    fn key(byte: u8) -> FactKey {
        FactKey::for_current_segment(SourceScopeId([byte; 32]), SourceDescriptor([byte; 32]))
    }

    fn slot(fact_key: FactKey, block_kind: u32, logical_id: u32) -> BlockSlot {
        BlockSlot {
            fact_key,
            block_kind,
            logical_id,
            offset: u64::from(block_kind) * 1_000 + u64::from(logical_id),
            crc32c: 0,
        }
    }

    #[test]
    fn a_block_can_be_inserted_and_read_back() {
        let mut cache: BoundedFactCache<Vec<u8>> = BoundedFactCache::new(1_024);
        let s = slot(key(1), 6, 0);
        assert!(cache.insert(s, Arc::new(vec![1, 2, 3]), 3));
        assert_eq!(cache.get(&s).as_deref(), Some(&vec![1, 2, 3]));
        assert_eq!(cache.used_bytes(), 3);
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn distinct_slots_of_one_file_do_not_collide() {
        let mut cache: BoundedFactCache<u32> = BoundedFactCache::new(1_024);
        let file = key(1);
        let counters = slot(file, 6, 0);
        let gauges = slot(file, 5, 0);
        cache.insert(counters, Arc::new(10), 8);
        cache.insert(gauges, Arc::new(20), 8);
        assert_eq!(cache.get(&counters).as_deref(), Some(&10));
        assert_eq!(cache.get(&gauges).as_deref(), Some(&20));
    }

    #[test]
    fn eviction_is_bounded_by_bytes_not_entry_count() {
        let mut cache: BoundedFactCache<u8> = BoundedFactCache::new(100);
        for index in 0..10_u32 {
            cache.insert(slot(key(1), 6, index), Arc::new(0), 30);
        }
        assert!(
            cache.used_bytes() <= 100,
            "the byte budget is never exceeded"
        );
        // Ten 30-byte blocks cannot all fit under a 100-byte ceiling.
        assert!(cache.len() < 10);
    }

    #[test]
    fn a_recent_read_protects_a_block_from_eviction() {
        let mut cache: BoundedFactCache<u8> = BoundedFactCache::new(60);
        let first = slot(key(1), 6, 1);
        let second = slot(key(1), 6, 2);
        cache.insert(first, Arc::new(0), 30);
        cache.insert(second, Arc::new(0), 30);
        // Touch `first`, then insert a third block; `second` is now the LRU.
        assert!(cache.get(&first).is_some());
        let third = slot(key(1), 6, 3);
        cache.insert(third, Arc::new(0), 30);
        assert!(cache.contains(&first), "the touched block survives");
        assert!(!cache.contains(&second), "the untouched block is evicted");
        assert!(cache.contains(&third));
    }

    #[test]
    fn a_block_larger_than_the_budget_is_not_cached() {
        let mut cache: BoundedFactCache<u8> = BoundedFactCache::new(10);
        let big = slot(key(1), 6, 0);
        assert!(!cache.insert(big, Arc::new(0), 11));
        assert!(!cache.contains(&big));
        assert_eq!(cache.used_bytes(), 0);
    }

    #[test]
    fn reinserting_a_slot_replaces_its_weight() {
        let mut cache: BoundedFactCache<u8> = BoundedFactCache::new(100);
        let s = slot(key(1), 6, 0);
        cache.insert(s, Arc::new(0), 40);
        cache.insert(s, Arc::new(1), 10);
        assert_eq!(cache.used_bytes(), 10, "the stale weight does not linger");
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(&s).as_deref(), Some(&1));
    }

    #[test]
    fn the_slot_of_a_directory_entry_names_the_block() {
        let file = key(7);
        let entry = BlockDirectoryEntry {
            block_kind: 6,
            block_schema_version: 1,
            flags: BlockFlags {
                required_for_schema: true,
                canonically_sorted: true,
                has_time_range: true,
                codec: BlockCodec::None,
            },
            logical_id: 3,
            offset: 512,
            stored_len: 40,
            decoded_len: 40,
            item_count: 2,
            block_crc32c: 0xABCD,
            min_ts_us: 0,
            max_ts_us: 10,
        };
        let named = BlockSlot::of(file, &entry);
        assert_eq!(named.block_kind, 6);
        assert_eq!(named.logical_id, 3);
        assert_eq!(named.offset, 512);
        assert_eq!(named.crc32c, 0xABCD);
        assert_eq!(named.fact_key, file);
    }
}
