//! Per-segment string interner.
//!
//! The interner keeps enough bytes to build PGM parts without retaining
//! every string until segment completion. It uses two stores:
//!
//! - the **window**: a [`SegmentDicts`] with full bytes for values first seen
//!   since the previous flush;
//! - **flushed entries**: one record per id already written to the journal,
//!   with full length, a 16-byte SHA-256 prefix, and accumulated placement
//!   requirements. The original text is not kept.
//!
//! At segment completion, the writer rebuilds final dictionaries from
//! journaled part dictionaries and the remaining window. Two cases need extra
//! handling:
//!
//! - strict-hot values, such as catalog `source_id` and chart headers, are
//!   kept in memory and inserted into every window;
//! - when a flushed value gets a stronger requirement, the value enters the
//!   window again so the next part records the new placement.

use std::collections::{BTreeMap, HashMap};

use kronika_format::{DictError, DictLimits, DictStats, HotMark, Placement, SegmentDicts, StrId};
use sha2::{Digest, Sha256};

/// Value metadata retained after its bytes have been written to the journal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Flushed {
    /// Length of the full original value, bytes.
    full_len: u64,
    /// First 16 bytes of SHA-256 of the full original value.
    ///
    /// This verifies repeated values after the full bytes have been flushed.
    check: [u8; 16],
    /// The registry forced this value into `dict.blobs`.
    blob_required: bool,
    /// Strict hot requirement.
    hot_hard: bool,
    /// Soft hot request.
    hot_soft: bool,
}

impl Flushed {
    /// Whether `bytes` is the same value this entry was created from.
    fn matches(&self, bytes: &[u8]) -> bool {
        self.full_len == bytes.len() as u64 && check16(bytes) == self.check
    }

    const fn placement(&self, limits: DictLimits) -> Placement {
        if self.blob_required || self.full_len >= limits.blob_threshold() as u64 {
            Placement::Blobs
        } else {
            Placement::Strings
        }
    }

    const fn hot(&self) -> HotMark {
        if self.hot_hard {
            HotMark::Hard
        } else if self.hot_soft {
            HotMark::Soft
        } else {
            HotMark::None
        }
    }
}

/// One interning request, mirroring the four `intern*` entry points.
#[derive(Debug, Clone, Copy, Default)]
struct Request {
    blob_required: bool,
    hot_hard: bool,
    hot_soft: bool,
}

/// First 16 bytes of SHA-256 over `bytes`.
fn check16(bytes: &[u8]) -> [u8; 16] {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    let mut out = [0_u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// First 16 bytes of an already computed SHA-256.
fn first16(digest: [u8; 32]) -> [u8; 16] {
    let mut out = [0_u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// Data returned when the interner finishes a segment.
#[derive(Debug)]
pub struct SealedSegment {
    /// Values still in memory, not yet written to the journal.
    ///
    /// Segment completion merges this window with journal parts.
    pub window: SegmentDicts,
    /// Final placement directives for flushed ids, in `str_id` order.
    pub flushed: Vec<FlushedEntry>,
}

/// Placement directive for one flushed id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FlushedEntry {
    /// The id.
    pub str_id: StrId,
    /// Length of the full original value, bytes.
    pub full_len: u64,
    /// Final dictionary for the value.
    pub placement: Placement,
    /// The hot requirement accumulated for the value.
    pub hot: HotMark,
}

/// Per-segment string interner.
///
/// All `intern*` methods deduplicate against both the window and flushed
/// entries. A failed call changes no state. Repeats of already-flushed values
/// do not enter the window again unless they add a stronger placement
/// requirement.
#[derive(Debug)]
pub struct Interner {
    window: SegmentDicts,
    /// Identities of values already in the journal: ~48 bytes per distinct
    /// id (plus map overhead) until `seal()`. The journal cap
    /// (`JournalError::Full`) forces a merge before the journal, and with it
    /// this map, can grow without limit.
    flushed: HashMap<StrId, Flushed>,
    /// Bytes of strict-hot values inserted into every window.
    ///
    /// Bounded by contract, not by data: callers may strict-hot only
    /// registry-defined header strings (chart headers, the catalog
    /// `source_id`), each shorter than `blob_threshold`. A data-driven
    /// strict-hot call is a caller bug.
    hot_pinned: BTreeMap<StrId, Vec<u8>>,
}

impl Interner {
    /// Create an empty interner for a new segment.
    #[must_use]
    pub fn new(limits: DictLimits) -> Self {
        Self {
            window: SegmentDicts::new(limits),
            flushed: HashMap::new(),
            hot_pinned: BTreeMap::new(),
        }
    }

    /// Intern with size-based placement.
    ///
    /// # Errors
    ///
    /// Returns [`DictError::Collision`] if the id is already used for a
    /// different value, or if the input hashes to zero. On error, the interner
    /// state is unchanged.
    pub fn intern(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.request(bytes, Request::default()).map(|(id, _)| id)
    }

    /// Intern a value that must be stored in `dict.blobs`.
    ///
    /// # Errors
    ///
    /// Returns [`DictError::Collision`] or [`DictError::PlacementConflict`] as
    /// in [`SegmentDicts::intern_blob`]. On error, the interner state is
    /// unchanged.
    pub fn intern_blob(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.request(
            bytes,
            Request {
                blob_required: true,
                ..Request::default()
            },
        )
        .map(|(id, _)| id)
    }

    /// Intern a value that must be available in every part hot cache.
    ///
    /// The value is kept in memory and inserted into each new window.
    ///
    /// # Errors
    ///
    /// Returns [`DictError::Collision`] or [`DictError::PlacementConflict`] as
    /// in [`SegmentDicts::intern_hot`]. On error, the interner state is
    /// unchanged.
    pub fn intern_hot(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        let (id, _) = self.request(
            bytes,
            Request {
                hot_hard: true,
                ..Request::default()
            },
        )?;
        self.hot_pinned.entry(id).or_insert_with(|| bytes.to_vec());
        Ok(id)
    }

    /// Intern a value and try to add it to `dict.hot_strings`.
    ///
    /// Returns the id and whether the value is hot after this call. Large or
    /// blob-forced values keep their normal placement and return `false`.
    ///
    /// # Errors
    ///
    /// Returns [`DictError::Collision`] as in [`Self::intern`]. On error, the
    /// interner state is unchanged.
    pub fn intern_hot_best_effort(&mut self, bytes: &[u8]) -> Result<(StrId, bool), DictError> {
        self.request(
            bytes,
            Request {
                hot_soft: true,
                ..Request::default()
            },
        )
    }

    /// Flush the current window to the journal and keep only compact records.
    ///
    /// `write` receives the current window dictionaries. Only after it returns
    /// `Ok` are entries moved into flushed records and the window cleared. If
    /// `write` returns `Err`, the window is left unchanged.
    ///
    /// Returns the number of entries flushed.
    ///
    /// # Errors
    ///
    /// Returns whatever `write` returns. The interner adds no errors of its
    /// own.
    pub fn flush_window<E>(
        &mut self,
        write: impl FnOnce(&SegmentDicts) -> Result<(), E>,
    ) -> Result<usize, E> {
        write(&self.window)?;

        let count = self.window.len();
        for snap in self.window.entries() {
            // The stored bytes are the full value when not truncated.
            let check = snap
                .full_sha256
                .map_or_else(|| check16(snap.stored_bytes), first16);
            self.flushed.insert(
                snap.str_id,
                Flushed {
                    full_len: snap.full_len,
                    check,
                    blob_required: snap.blob_required,
                    hot_hard: snap.hot == HotMark::Hard,
                    hot_soft: snap.hot == HotMark::Soft,
                },
            );
        }

        let limits = self.window.limits();
        self.window = SegmentDicts::new(limits);
        // Reinsert strict-hot values so the next part carries them too.
        // These re-inserts cannot fail: every pinned value already passed
        // the strict-hot checks once, and the window is empty.
        for bytes in self.hot_pinned.values() {
            let _ = self.window.intern_hot(bytes);
        }
        Ok(count)
    }

    /// Finish the segment and reset the interner for the next segment.
    ///
    /// Returns the remaining window plus placement directives for values already
    /// flushed to the journal.
    pub fn seal(&mut self) -> SealedSegment {
        let limits = self.window.limits();
        let window = std::mem::replace(&mut self.window, SegmentDicts::new(limits));
        let flushed = std::mem::take(&mut self.flushed);
        self.hot_pinned.clear();

        let mut entries: Vec<FlushedEntry> = flushed
            .iter()
            .map(|(id, f)| FlushedEntry {
                str_id: *id,
                full_len: f.full_len,
                placement: f.placement(limits),
                hot: f.hot(),
            })
            .collect();
        entries.sort_by_key(|entry| entry.str_id);

        SealedSegment {
            window,
            flushed: entries,
        }
    }

    /// Return dictionary sizes across the window and flushed entries.
    ///
    /// Byte sizes of flushed values count the stored form after truncation,
    /// matching what is on disk.
    #[must_use]
    pub fn stats(&self) -> DictStats {
        let limits = self.window.limits();
        let mut stats = self.window.stats();
        for (id, f) in &self.flushed {
            // A re-flushed upgrade is present in both maps; the window copy is
            // current and already counted.
            if self.window.resolve(*id).is_some() {
                continue;
            }
            let stored_len = f.full_len.min(limits.truncate_limit() as u64);
            match f.placement(limits) {
                Placement::Blobs => {
                    stats.blob_count += 1;
                    stats.blob_bytes += stored_len;
                }
                Placement::Strings => {
                    stats.string_count += 1;
                    stats.string_bytes += stored_len;
                    if f.hot() != HotMark::None {
                        stats.hot_count += 1;
                    }
                }
            }
        }
        stats
    }

    /// Return the current window.
    #[must_use]
    pub const fn window(&self) -> &SegmentDicts {
        &self.window
    }

    /// Return whether the id was interned in this segment.
    #[must_use]
    pub fn is_interned(&self, id: StrId) -> bool {
        self.window.resolve(id).is_some() || self.flushed.contains_key(&id)
    }

    /// Shared intern path.
    ///
    /// Checks the window first, then the flushed map, then inserts into the
    /// window. All checks run before any mutation.
    fn request(&mut self, bytes: &[u8], req: Request) -> Result<(StrId, bool), DictError> {
        let Some(id) = StrId::of(bytes) else {
            return Err(DictError::Collision { id: 0 });
        };

        if self.window.resolve(id).is_some() {
            return self.apply_to_window(bytes, req);
        }

        if let Some(flushed) = self.flushed.get(&id) {
            if !flushed.matches(bytes) {
                return Err(DictError::Collision { id: id.get() });
            }
            let merged = Request {
                blob_required: flushed.blob_required || req.blob_required,
                hot_hard: flushed.hot_hard || req.hot_hard,
                hot_soft: flushed.hot_soft || req.hot_soft,
            };
            let oversized = flushed.full_len >= self.window.limits().blob_threshold() as u64;
            if merged.hot_hard && (merged.blob_required || oversized) {
                return Err(DictError::PlacementConflict { id });
            }
            let placement_is_blob = merged.blob_required || oversized;
            // Only changes that must survive a crash re-enter the window:
            // placement (forced blob) and the strict hot mark are rebuilt
            // from part dictionaries at recovery, so the next part has to
            // record them. A soft hot mark may be lost after a crash. On a
            // blob-placed value it never becomes effective, so neither case
            // is worth loading a large value into memory again.
            let durable_upgrade = merged.blob_required != flushed.blob_required
                || merged.hot_hard != flushed.hot_hard;
            let soft_became_effective = merged.hot_soft != flushed.hot_soft && !placement_is_blob;

            if durable_upgrade {
                let result = self.apply_to_window(bytes, merged)?;
                self.record_flushed_bits(id, merged);
                return Ok(result);
            }
            if soft_became_effective {
                self.record_flushed_bits(id, merged);
            }
            // The common case: a repeat of a flushed value does not
            // re-enter memory.
            let hot = (merged.hot_hard || merged.hot_soft) && !placement_is_blob;
            return Ok((id, hot));
        }

        self.apply_to_window(bytes, req)
    }

    /// Keep the flushed record in sync with an accepted upgrade, so
    /// [`Interner::seal`] can report the final directives even before the next
    /// flush writes the upgraded value again.
    fn record_flushed_bits(&mut self, id: StrId, merged: Request) {
        if let Some(entry) = self.flushed.get_mut(&id) {
            entry.blob_required = merged.blob_required;
            entry.hot_hard = merged.hot_hard;
            entry.hot_soft = merged.hot_soft;
        }
    }

    /// Apply a request to the window, bit by bit: requirements
    /// accumulate inside [`SegmentDicts`], so each flag is one call.
    fn apply_to_window(&mut self, bytes: &[u8], req: Request) -> Result<(StrId, bool), DictError> {
        // Pre-check the conflict so that a multi-flag request cannot
        // fail halfway and leave a partially-required entry behind.
        let oversized = bytes.len() >= self.window.limits().blob_threshold();
        if req.hot_hard && (req.blob_required || oversized) {
            return Err(DictError::PlacementConflict {
                id: StrId::of(bytes).unwrap_or_else(|| unreachable!("checked by request()")),
            });
        }

        let id = self.window.intern(bytes)?;
        if req.blob_required {
            self.window.intern_blob(bytes)?;
        }
        if req.hot_hard {
            self.window.intern_hot(bytes)?;
        }
        let hot = if req.hot_soft {
            self.window.intern_hot_best_effort(bytes)?.1
        } else {
            // A successful hard request is hot by definition; the other
            // callers discard the flag, so no window scan is needed.
            req.hot_hard
        };
        Ok((id, hot))
    }
}

#[cfg(test)]
mod tests {
    use kronika_format::Resolved;

    use super::*;

    fn small_interner() -> Interner {
        Interner::new(DictLimits::new(8, 16).expect("8 <= 16"))
    }

    /// Flush the window pretending the journal write always succeeds.
    fn flush_ok(interner: &mut Interner) -> usize {
        interner
            .flush_window(|_| Ok::<(), ()>(()))
            .expect("infallible write")
    }

    #[test]
    fn str_id_is_stable_across_instances() {
        let mut a = small_interner();
        let mut b = small_interner();
        let id_a = a.intern(b"pg_stat_activity").expect("interns");
        let id_b = b.intern(b"pg_stat_activity").expect("interns");
        assert_eq!(id_a, id_b);
    }

    #[test]
    fn window_holds_only_values_new_since_last_flush() {
        let mut interner = small_interner();
        interner.intern(b"a").expect("interns");
        interner.intern(b"b").expect("interns");
        assert_eq!(flush_ok(&mut interner), 2);
        assert!(interner.window().is_empty());

        // A repeat of a flushed value does not re-enter memory; a new
        // value does.
        let again = interner.intern(b"a").expect("re-interns");
        assert!(interner.window().resolve(again).is_none());
        assert!(interner.is_interned(again));
        let c = interner.intern(b"c").expect("interns");
        assert!(interner.window().resolve(c).is_some());
        assert_eq!(flush_ok(&mut interner), 1);
    }

    #[test]
    fn failed_journal_write_keeps_the_window() {
        let mut interner = small_interner();
        interner.intern(b"value").expect("interns");
        let err = interner.flush_window(|_| Err::<(), &str>("disk full"));
        assert_eq!(err, Err("disk full"));
        assert_eq!(
            interner.window().len(),
            1,
            "the only copy of the bytes must survive a failed write"
        );
        // A failed write must not create flushed records, or seal() would
        // emit directives for values that exist nowhere in the journal.
        let sealed = interner.seal();
        assert!(sealed.flushed.is_empty());
        assert_eq!(sealed.window.len(), 1);
    }

    #[test]
    fn seal_reports_upgrades_not_yet_flushed_again() {
        // Blob upgrade after a flush, sealed before the next flush: the
        // directive must already carry the new placement, because the
        // merge takes placement from directives, not from part dicts.
        let mut interner = small_interner();
        let id = interner.intern(b"plan").expect("interns");
        flush_ok(&mut interner);
        interner.intern_blob(b"plan").expect("upgrade");
        let sealed = interner.seal();
        let entry = sealed
            .flushed
            .iter()
            .find(|entry| entry.str_id == id)
            .expect("directive");
        assert_eq!(entry.placement, Placement::Blobs);

        // Same for a strict hot upgrade.
        let mut interner = small_interner();
        let id = interner.intern(b"src/42").expect("interns");
        flush_ok(&mut interner);
        interner.intern_hot(b"src/42").expect("hot upgrade");
        let sealed = interner.seal();
        let entry = sealed
            .flushed
            .iter()
            .find(|entry| entry.str_id == id)
            .expect("directive");
        assert_eq!(entry.hot, HotMark::Hard);
    }

    #[test]
    fn soft_hot_survives_the_flush_boundary() {
        let mut interner = small_interner();
        let (id, hot) = interner.intern_hot_best_effort(b"label").expect("soft hot");
        assert!(hot);
        flush_ok(&mut interner);

        // A repeat of the flushed soft-hot value reports hot without
        // re-entering the window.
        let (again, hot) = interner.intern_hot_best_effort(b"label").expect("repeat");
        assert_eq!(again, id);
        assert!(hot);
        assert!(interner.window().is_empty());

        // A late soft mark on a flushed plain string updates the
        // directive without loading the bytes back into the window.
        let plain = interner.intern(b"note").expect("interns");
        flush_ok(&mut interner);
        let (_, hot) = interner.intern_hot_best_effort(b"note").expect("soft mark");
        assert!(hot);
        assert!(interner.window().is_empty());
        let sealed = interner.seal();
        let entry = sealed
            .flushed
            .iter()
            .find(|entry| entry.str_id == plain)
            .expect("directive");
        assert_eq!(entry.hot, HotMark::Soft);
    }

    #[test]
    fn soft_hot_on_flushed_blob_does_not_reload_value() {
        let mut interner = small_interner();
        let oversized = b"this value is longer than sixteen bytes";
        let id = interner.intern(oversized).expect("interns as a blob");
        flush_ok(&mut interner);

        // A soft mark can never become effective on a blob-placed value:
        // it must not pull the stored bytes back into memory.
        let (again, hot) = interner
            .intern_hot_best_effort(oversized)
            .expect("soft mark on a blob");
        assert_eq!(again, id);
        assert!(!hot);
        assert!(
            interner.window().is_empty(),
            "soft hot on a blob must not reload the value"
        );
        let sealed = interner.seal();
        assert_eq!(sealed.flushed[0].hot, HotMark::None);
    }

    #[test]
    fn flushed_values_are_verified_not_trusted() {
        let mut interner = small_interner();
        let id = interner.intern(b"short").expect("interns");
        flush_ok(&mut interner);

        // The same value re-interns fine even though its bytes are gone.
        assert_eq!(interner.intern(b"short").expect("repeat"), id);
        // Different bytes under the same id would be a collision; the
        // public path cannot construct one (no known xxh3 preimages), so
        // the verifier is tested directly.
        let flushed = Flushed {
            full_len: 5,
            check: check16(b"short"),
            blob_required: false,
            hot_hard: false,
            hot_soft: false,
        };
        assert!(flushed.matches(b"short"));
        assert!(!flushed.matches(b"shore"), "same length, different bytes");
        assert!(!flushed.matches(b"shorter"), "different length");
    }

    #[test]
    fn upgrade_of_a_flushed_value_reenters_the_window() {
        let mut interner = small_interner();
        let id = interner.intern(b"plan").expect("interns as a string");
        flush_ok(&mut interner);

        // The registry now requires the same value in dict.blobs. The next
        // part must record that, so the value enters the window again.
        let same = interner.intern_blob(b"plan").expect("upgrade");
        assert_eq!(same, id);
        assert!(
            matches!(interner.window().resolve(id), Some(Resolved::Blob(_))),
            "the window records the upgraded placement"
        );
        assert_eq!(flush_ok(&mut interner), 1);

        // After the next flush the directive remains in the flushed map.
        let sealed = interner.seal();
        let entry = sealed
            .flushed
            .iter()
            .find(|entry| entry.str_id == id)
            .expect("flushed directive");
        assert_eq!(entry.placement, Placement::Blobs);
    }

    #[test]
    fn conflicts_on_flushed_values_fail_at_the_call_site() {
        let mut interner = small_interner();
        interner.intern_blob(b"plan").expect("forced blob");
        flush_ok(&mut interner);

        let err = interner
            .intern_hot(b"plan")
            .expect_err("hot of a flushed forced-blob value");
        assert!(matches!(err, DictError::PlacementConflict { .. }));
        assert!(
            interner.window().is_empty(),
            "a rejected upgrade must not re-enter the window"
        );
    }

    #[test]
    fn pinned_hot_values_reach_every_window() {
        let mut interner = small_interner();
        let source = interner.intern_hot(b"src/42").expect("strict hot");
        flush_ok(&mut interner);

        // The fresh window already carries the pinned value, so the next part
        // resolves its own catalog source_id.
        assert!(interner.window().resolve(source).is_some());
        assert_eq!(interner.window().hot_strings().count(), 1);
        assert_eq!(flush_ok(&mut interner), 1, "the pin is flushed again");
        assert!(interner.window().resolve(source).is_some());
    }

    #[test]
    fn seal_returns_remaining_window_and_flushed_directives() {
        let mut interner = small_interner();
        let flushed_id = interner.intern(b"flushed").expect("interns");
        flush_ok(&mut interner);
        let window_id = interner.intern(b"window").expect("interns");

        let sealed = interner.seal();
        assert!(sealed.window.resolve(window_id).is_some());
        assert!(sealed.window.resolve(flushed_id).is_none());
        assert_eq!(sealed.flushed.len(), 1);
        assert_eq!(sealed.flushed[0].str_id, flushed_id);
        assert_eq!(sealed.flushed[0].placement, Placement::Strings);

        // The interner starts the next segment empty.
        assert!(interner.window().is_empty());
        assert!(!interner.is_interned(flushed_id));
        assert_eq!(interner.stats(), DictStats::default());
    }

    #[test]
    fn stats_cover_window_and_flushed_without_double_counting() {
        let mut interner = small_interner();
        interner.intern(b"one").expect("string");
        interner.intern_hot(b"hot").expect("hot string");
        interner.intern(b"longer than the threshold").expect("blob");
        flush_ok(&mut interner);
        interner.intern(b"fresh").expect("window string");
        // An upgrade present in both maps must be counted once.
        interner.intern_blob(b"one").expect("upgrade");

        let stats = interner.stats();
        // Strings: "hot" (pinned, in window), "fresh" (window). "one" is
        // now a blob. Blobs: "one" + the oversized value.
        assert_eq!(stats.string_count, 2);
        assert_eq!(stats.blob_count, 2);
        assert_eq!(stats.hot_count, 1);
    }

    #[test]
    fn oversized_strict_hot_fails_without_state_change() {
        let mut interner = small_interner();
        let err = interner
            .intern_hot(b"longer than the eight-byte threshold")
            .expect_err("strict hot of an oversized value");
        assert!(matches!(err, DictError::PlacementConflict { .. }));
        assert!(interner.window().is_empty());
        assert_eq!(interner.stats(), DictStats::default());
    }

    #[test]
    fn full_window_signals_flush_and_recovers() {
        let limits = DictLimits::new(8, 16)
            .expect("valid")
            .with_max_total_bytes(16)
            .expect("cap fits one value");
        let mut interner = Interner::new(limits);

        interner.intern(b"0123456789").expect("fits the window cap");
        let err = interner
            .intern(b"abcdefghij")
            .expect_err("the window is full");
        assert!(matches!(err, DictError::Full { .. }));

        // The signal means "flush": after the flush the value fits.
        flush_ok(&mut interner);
        interner
            .intern(b"abcdefghij")
            .expect("fits after the flush");
    }
}
