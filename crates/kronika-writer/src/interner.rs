//! The collector's string interner: segment lifecycle over [`SegmentDicts`].
//!
//! The dictionary contract itself is defined in `kronika-format` and is
//! documented there; this module adds what only the write path needs
//! (this crate's README.md): which values are new since the last
//! mini-part flush, dictionary sizes for self-metrics, and the reset on
//! segment seal.

use kronika_format::{DictError, DictLimits, DictStats, SegmentDicts, StrId};

/// Per-segment string interner.
///
/// All `intern*` methods are the deduplicating methods of
/// [`SegmentDicts`] plus tracking for new ids: ids first seen since the
/// previous [`Interner::take_new`] call are the dictionary content of
/// the next mini-part — a mini-part dictionary holds the strings first
/// seen in its window (README.md, "Implemented Scope"). A failed call
/// changes neither the dictionaries nor the list of new ids.
#[derive(Debug)]
pub struct Interner {
    dicts: SegmentDicts,
    /// Ids inserted since the last `take_new`, in first-seen order.
    fresh: Vec<StrId>,
}

impl Interner {
    /// Empty interner for a new segment.
    #[must_use]
    pub const fn new(limits: DictLimits) -> Self {
        Self {
            dicts: SegmentDicts::new(limits),
            fresh: Vec::new(),
        }
    }

    /// Intern with size-based routing. See [`SegmentDicts::intern`].
    ///
    /// # Errors
    ///
    /// [`DictError::Collision`] — see [`SegmentDicts::intern`].
    pub fn intern(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.track(|dicts| dicts.intern(bytes))
    }

    /// Intern a registry-forced blob value. See
    /// [`SegmentDicts::intern_blob`].
    ///
    /// # Errors
    ///
    /// [`DictError::Collision`] or [`DictError::PlacementConflict`] — see
    /// [`SegmentDicts::intern_blob`].
    pub fn intern_blob(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.track(|dicts| dicts.intern_blob(bytes))
    }

    /// Intern a strict-hot value. See [`SegmentDicts::intern_hot`].
    ///
    /// # Errors
    ///
    /// [`DictError::Collision`] or [`DictError::PlacementConflict`] — see
    /// [`SegmentDicts::intern_hot`].
    pub fn intern_hot(&mut self, bytes: &[u8]) -> Result<StrId, DictError> {
        self.track(|dicts| dicts.intern_hot(bytes))
    }

    /// Intern a best-effort hot value. See
    /// [`SegmentDicts::intern_hot_best_effort`].
    ///
    /// # Errors
    ///
    /// [`DictError::Collision`] — see
    /// [`SegmentDicts::intern_hot_best_effort`].
    pub fn intern_hot_best_effort(&mut self, bytes: &[u8]) -> Result<(StrId, bool), DictError> {
        let before = self.dicts.len();
        let (id, hot) = self.dicts.intern_hot_best_effort(bytes)?;
        self.note_new(before, id);
        Ok((id, hot))
    }

    /// Ids first interned since the previous call: the dictionary content
    /// of the next mini-part, which holds the strings first seen in its
    /// flush window. The internal list is drained.
    ///
    /// The ids are bare references into [`Self::dicts`], and a later
    /// requirement upgrade (e.g. [`Self::intern_blob`] of an already
    /// taken id) can still move a value between `strings` and `blobs`.
    /// So the mini-part encoder must resolve the ids before further
    /// interning, and the seal path must take placement from the live
    /// dictionaries, not from placement recorded in already-flushed
    /// mini-parts.
    #[must_use = "dropping the result loses the dictionary content of a mini-part window"]
    pub fn take_new(&mut self) -> Vec<StrId> {
        std::mem::take(&mut self.fresh)
    }

    /// Finish the segment: hand the dictionaries to the seal path and
    /// start the next segment empty. The collector seals early under
    /// interner growth pressure precisely so that the next segment starts
    /// with an empty interner (README.md, "Implemented Scope").
    pub fn seal(&mut self) -> SegmentDicts {
        self.fresh.clear();
        let limits = self.dicts.limits();
        std::mem::replace(&mut self.dicts, SegmentDicts::new(limits))
    }

    /// Dictionary sizes for the collector's self-metrics.
    #[must_use]
    pub fn stats(&self) -> DictStats {
        self.dicts.stats()
    }

    /// Read access to the dictionaries built so far.
    #[must_use]
    pub const fn dicts(&self) -> &SegmentDicts {
        &self.dicts
    }

    /// Run one interning call and record the id if it is new.
    fn track(
        &mut self,
        op: impl FnOnce(&mut SegmentDicts) -> Result<StrId, DictError>,
    ) -> Result<StrId, DictError> {
        let before = self.dicts.len();
        let id = op(&mut self.dicts)?;
        self.note_new(before, id);
        Ok(id)
    }

    /// New ids are detected by dictionary growth: a repeat or a pure
    /// requirement upgrade does not grow the map.
    fn note_new(&mut self, before: usize, id: StrId) {
        if self.dicts.len() > before {
            self.fresh.push(id);
        }
    }
}

#[cfg(test)]
mod tests {
    use kronika_format::Resolved;

    use super::*;

    fn small_interner() -> Interner {
        Interner::new(DictLimits::new(8, 16).expect("8 <= 16"))
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
    fn take_new_covers_every_intern_variant() {
        let mut interner = small_interner();
        let a = interner.intern(b"a").expect("plain");
        let b = interner.intern_blob(b"plan").expect("forced blob");
        let c = interner.intern_hot(b"hot").expect("strict hot");
        let (d, hot) = interner.intern_hot_best_effort(b"label").expect("soft hot");
        assert!(hot);
        assert_eq!(interner.take_new(), vec![a, b, c, d]);

        // Repeats through any variant are not news.
        interner.intern_blob(b"plan").expect("re-interns");
        let _ = interner
            .intern_hot_best_effort(b"label")
            .expect("re-interns");
        assert_eq!(interner.take_new(), Vec::new());
    }

    #[test]
    fn take_new_tracks_first_seen_per_window() {
        let mut interner = small_interner();
        let a = interner.intern(b"a").expect("interns");
        let b = interner.intern(b"b").expect("interns");
        assert_eq!(interner.take_new(), vec![a, b]);

        // A repeat is not news; a new value is.
        interner.intern(b"a").expect("re-interns");
        let c = interner.intern(b"c").expect("interns");
        assert_eq!(interner.take_new(), vec![c]);
        assert_eq!(interner.take_new(), Vec::new());
    }

    #[test]
    fn failed_intern_leaves_state_unchanged() {
        let mut interner = small_interner();
        interner
            .intern_hot(b"longer than the eight-byte threshold")
            .expect_err("strict hot of an oversized value");
        assert_eq!(interner.take_new(), Vec::new());
        assert_eq!(interner.stats().string_count, 0);
        assert_eq!(interner.stats().blob_count, 0);
    }

    #[test]
    fn seal_hands_over_dicts_and_resets() {
        let mut interner = small_interner();
        let id = interner.intern(b"value").expect("interns");

        let dicts = interner.seal();
        assert!(matches!(dicts.resolve(id), Some(Resolved::Str(b"value"))));

        assert_eq!(interner.stats().string_count, 0);
        assert_eq!(interner.take_new(), Vec::new());
        assert!(interner.dicts().is_empty());

        // The next segment starts from scratch but with the same limits.
        let again = interner.intern(b"value").expect("interns after seal");
        assert_eq!(again, id, "str_id is content-defined, not per-segment");
        assert_eq!(interner.take_new(), vec![again]);
    }
}
