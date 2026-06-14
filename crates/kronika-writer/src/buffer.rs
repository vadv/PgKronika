//! Per-type row buffers: the writer's collection window before a mini-part.
//!
//! A collection step pushes typed rows of many section types into
//! [`SectionBuffers`]; at a flush they are each encoded to a Parquet body and
//! assembled into one PGM part (segment-format.md, "Write and merge").
//!
//! Type erasure is the point. [`SectionBuffers`] holds one buffer per `type_id`,
//! not a field per type, so a new section type costs one [`push`](SectionBuffers::push)
//! call and no change here — the property the registry's hundreds of types need.

use std::any::Any;
use std::collections::BTreeMap;

use kronika_format::{PartMeta, SectionInput, build_part};
use kronika_registry::{CodecError, Section};

/// One section type's buffered rows, erased so [`SectionBuffers`] can hold many
/// types in one map. The registry assigns one `Section` type per `type_id`, so
/// the downcast in [`SectionBuffers::push`] is total.
trait TypeBuffer: Any {
    fn section_type_id(&self) -> u32;
    fn is_empty(&self) -> bool;
    /// Encode the buffered rows to a section body, its row count, and ts range.
    fn encode(&self) -> Result<EncodedRows, CodecError>;
    fn clear(&mut self);
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// One encoded section: the Parquet body plus the catalog fields derived from it.
struct EncodedRows {
    body: Vec<u8>,
    rows: u32,
    ts_range: Option<(i64, i64)>,
}

struct RowBuffer<T: Section> {
    rows: Vec<T>,
}

impl<T: Section + 'static> TypeBuffer for RowBuffer<T> {
    fn section_type_id(&self) -> u32 {
        T::CONTRACT.type_id.get()
    }

    fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn encode(&self) -> Result<EncodedRows, CodecError> {
        let body = T::encode(&self.rows)?;
        // `encode` enforced the row cap, so the count is below `MAX_SECTION_ROWS`
        // and fits the catalog's `u32` row field.
        let rows = u32::try_from(self.rows.len()).unwrap_or(u32::MAX);
        Ok(EncodedRows {
            body,
            rows,
            ts_range: T::ts_range(&self.rows),
        })
    }

    fn clear(&mut self) {
        self.rows.clear();
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

/// The writer's collection window: typed rows buffered per section type until a
/// flush turns them into one PGM part.
#[derive(Default)]
pub struct SectionBuffers {
    by_type: BTreeMap<u32, Box<dyn TypeBuffer>>,
}

impl std::fmt::Debug for SectionBuffers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SectionBuffers")
            .field("type_ids", &self.by_type.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl SectionBuffers {
    /// An empty set of buffers.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            by_type: BTreeMap::new(),
        }
    }

    /// Buffer one row of section type `T`.
    ///
    /// The first row of a type creates its buffer; later rows append. No type is
    /// named here, so adding a section type does not change this code.
    ///
    /// # Panics
    ///
    /// Never in practice: the downcast would only fail if two `Section` types
    /// shared a `type_id`, which the registry's `TypeId` construction forbids.
    pub fn push<T: Section + 'static>(&mut self, row: T) {
        let type_id = T::CONTRACT.type_id.get();
        let buffer = self
            .by_type
            .entry(type_id)
            .or_insert_with(|| Box::new(RowBuffer::<T> { rows: Vec::new() }));
        buffer
            .as_any_mut()
            .downcast_mut::<RowBuffer<T>>()
            .expect("a type_id maps to exactly one Section type")
            .rows
            .push(row);
    }

    /// Whether no rows are buffered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_type.values().all(|buffer| buffer.is_empty())
    }

    /// Encode every buffered type into one PGM part and clear the data buffers.
    ///
    /// `dict_sections` are the window's `dict.strings` / `dict.blobs` bodies
    /// (from [`dict::encode`](crate::dict::encode)); they are laid out after the
    /// data sections, the COLD-then-WARM order of a segment. Data sections come in
    /// `type_id` order, so the part is deterministic. The catalog time range is
    /// the min/max across the types that carry a timestamp; `source_id` is the
    /// interned `{cluster_id}/{pg_system_identifier}` id, or 0 if unset.
    ///
    /// Returns `None` when neither data nor dictionary sections are present. Only
    /// the data buffers are cleared — the caller flushes the interner window so a
    /// failed journal write keeps it.
    ///
    /// # Errors
    ///
    /// Propagates the [`CodecError`] from encoding any buffered type.
    pub fn flush(
        &mut self,
        dict_sections: &[crate::dict::DictSection],
        source_id: u64,
    ) -> Result<Option<Vec<u8>>, CodecError> {
        let encoded: Vec<(u32, EncodedRows)> = self
            .by_type
            .values()
            .filter(|buffer| !buffer.is_empty())
            .map(|buffer| Ok((buffer.section_type_id(), buffer.encode()?)))
            .collect::<Result<_, CodecError>>()?;
        if encoded.is_empty() && dict_sections.is_empty() {
            return Ok(None);
        }

        // The part's time range spans the rows of every type that carries a
        // timestamp; a part of only timestampless sections records 0..0.
        let min_ts = encoded
            .iter()
            .filter_map(|(_, section)| section.ts_range.map(|(lo, _)| lo))
            .min()
            .unwrap_or(0);
        let max_ts = encoded
            .iter()
            .filter_map(|(_, section)| section.ts_range.map(|(_, hi)| hi))
            .max()
            .unwrap_or(0);

        let mut sections: Vec<SectionInput<'_>> = encoded
            .iter()
            .map(|(type_id, section)| SectionInput {
                type_id: *type_id,
                rows: section.rows,
                body: &section.body,
            })
            .collect();
        sections.extend(dict_sections.iter().map(|dict| SectionInput {
            type_id: dict.type_id,
            rows: dict.rows,
            body: &dict.body,
        }));
        let part = build_part(
            &sections,
            PartMeta {
                min_ts,
                max_ts,
                source_id,
            },
        );

        for buffer in self.by_type.values_mut() {
            buffer.clear();
        }
        Ok(Some(part))
    }
}

#[cfg(test)]
mod tests {
    use kronika_format::{crc32c, validate_part};
    use kronika_registry::bgwriter_checkpointer::BgwriterCheckpointer;
    use kronika_registry::instance_metadata::InstanceMetadata;
    use kronika_registry::{Bytes, StrId, Ts, VerifiedSection, decode_any};

    use super::SectionBuffers;

    fn bgwriter(ts: i64) -> BgwriterCheckpointer {
        BgwriterCheckpointer {
            ts: Ts(ts),
            checkpoints_timed: 10,
            checkpoints_req: 2,
            checkpoint_write_time: 1.0,
            checkpoint_sync_time: 2.0,
            buffers_checkpoint: 4096,
            restartpoints_timed: None,
            restartpoints_req: None,
            restartpoints_done: None,
            buffers_clean: 512,
            maxwritten_clean: 3,
            buffers_backend: Some(128),
            buffers_backend_fsync: Some(0),
            buffers_alloc: 9000,
            bgwriter_stats_reset: Ts(ts - 100),
            checkpointer_stats_reset: None,
        }
    }

    fn instance(ts: i64) -> InstanceMetadata {
        InstanceMetadata {
            ts: Ts(ts),
            hostname: StrId(1),
            node_self_id: StrId(2),
            pg_version_num: 170_000,
            kernel_version: StrId(3),
            pg_system_identifier: 7,
            clock_ticks_per_sec: 100,
            page_size_bytes: 4096,
            boot_id: StrId(4),
            btime: Ts(ts - 1_000),
        }
    }

    #[test]
    fn buffers_many_types_and_flushes_one_part() {
        let mut buffers = SectionBuffers::new();
        assert!(buffers.is_empty());
        buffers.push(bgwriter(1_000));
        buffers.push(bgwriter(2_000));
        buffers.push(instance(1_500));
        assert!(!buffers.is_empty());

        let part = buffers
            .flush(&[], 7)
            .expect("flush encodes the buffered rows")
            .expect("buffered rows produce a part");
        assert!(buffers.is_empty(), "flush clears the window");

        let catalog = validate_part(&part).expect("the part is a valid container");
        assert_eq!(catalog.entries.len(), 2, "one section per buffered type");
        assert_eq!(catalog.source_id, 7);
        assert_eq!(
            (catalog.min_ts, catalog.max_ts),
            (1_000, 2_000),
            "time range spans both bgwriter rows"
        );

        // Each section decodes back through the registry with the real CRC check
        // injected from the format crate — the first end-to-end exercise of the
        // CRC trust boundary the registry was built around.
        let decode_rows = |type_id: u32| -> usize {
            let entry = *catalog
                .entries
                .iter()
                .find(|entry| entry.type_id == type_id)
                .expect("the type's entry is present");
            let start = usize::try_from(entry.offset).expect("offset fits usize");
            let len = usize::try_from(entry.len).expect("len fits usize");
            let body = Bytes::copy_from_slice(&part[start..start + len]);
            let verified =
                VerifiedSection::verify(body, entry.crc32c, crc32c).expect("catalog crc matches");
            decode_any(type_id, verified).expect("decode").stats.rows
        };
        assert_eq!(decode_rows(1_006_001), 2);
        assert_eq!(decode_rows(1_021_001), 1);
    }

    #[test]
    fn flushing_an_empty_window_yields_no_part() {
        let mut buffers = SectionBuffers::new();
        assert!(buffers.flush(&[], 0).expect("flush ok").is_none());
    }
}
