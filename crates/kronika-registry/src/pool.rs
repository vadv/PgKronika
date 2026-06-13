//! A reusable-buffer pool for decode input.
//!
//! [`decode_section`](crate::decode_section) and [`decode_any`](crate::decode_any)
//! take owned `Bytes` and never copy the section — the Parquet reader slices it
//! in place. A streaming reader (a future ingest loop, S3-replay) still has to
//! put each section's bytes *somewhere* before decoding; this pool hands out
//! buffers that return to it when the decoded `Bytes` is dropped, so a steady
//! decode loop allocates a bounded set of buffers once and reuses them instead
//! of allocating per section.
//!
//! It only addresses the input buffer. The dominant decode allocations — zstd
//! decompression output and the Arrow arrays — are the decoded data itself and
//! are inherently fresh; the pool does not touch them.

use std::sync::{Arc, Mutex, PoisonError};

use bytes::Bytes;

/// A pool of reusable input buffers.
///
/// Retained memory is bounded: the pool holds at most `max_buffers` idle
/// buffers, and only retains a returned buffer whose capacity is within
/// `buffer_limit` (a larger one is freed, not pooled). A single in-flight
/// buffer can still grow to whatever the caller writes; bound that at the call
/// site (decode rejects inputs above `MAX_SECTION_BYTES`).
///
/// Synchronous: `load` and `Loan::drop` take a `std::sync::Mutex` briefly. That
/// is free for the single-threaded decode loop this feeds; a multi-threaded or
/// async consumer should measure lock contention — or move to a lock-free
/// return — before relying on it, and should not treat a `Loan` as cheap to
/// hold across an `.await`.
#[derive(Clone, Debug)]
pub struct BytesPool {
    shared: Arc<Mutex<Shared>>,
}

#[derive(Debug)]
struct Shared {
    idle: Vec<Vec<u8>>,
    max_buffers: usize,
    buffer_limit: usize,
}

/// A buffer on loan from the pool. It returns to the pool when dropped — which,
/// because it owns the `Bytes`, is after the last `Bytes` reference is gone.
struct Loan {
    data: Vec<u8>,
    shared: Arc<Mutex<Shared>>,
}

impl AsRef<[u8]> for Loan {
    fn as_ref(&self) -> &[u8] {
        &self.data
    }
}

impl Drop for Loan {
    fn drop(&mut self) {
        let data = std::mem::take(&mut self.data);
        let mut shared = self.shared.lock().unwrap_or_else(PoisonError::into_inner);
        if data.capacity() <= shared.buffer_limit && shared.idle.len() < shared.max_buffers {
            let mut data = data;
            data.clear();
            shared.idle.push(data);
        }
    }
}

impl BytesPool {
    /// A pool that keeps at most `max_buffers` idle buffers, each of capacity up
    /// to `buffer_limit` bytes.
    #[must_use]
    pub fn new(max_buffers: usize, buffer_limit: usize) -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                idle: Vec::new(),
                max_buffers,
                buffer_limit,
            })),
        }
    }

    /// Take a buffer, fill it via `fill`, and freeze it to `Bytes`.
    ///
    /// When the returned `Bytes` and every clone or slice of it are dropped, the
    /// buffer returns to the pool for reuse, so a steady loop does not allocate
    /// per call. The buffer handed to `fill` is empty but may already have spare
    /// capacity from a previous loan.
    #[must_use]
    pub fn load(&self, fill: impl FnOnce(&mut Vec<u8>)) -> Bytes {
        let mut data = self
            .shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .idle
            .pop()
            .unwrap_or_default();
        data.clear();
        fill(&mut data);
        Bytes::from_owner(Loan {
            data,
            shared: Arc::clone(&self.shared),
        })
    }

    /// Number of idle buffers currently held, for tests.
    #[cfg(test)]
    fn idle(&self) -> usize {
        self.shared
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .idle
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::BytesPool;

    #[test]
    fn buffer_returns_on_drop_and_is_reused() {
        let pool = BytesPool::new(2, 1 << 20);
        assert_eq!(pool.idle(), 0);

        let first = pool.load(|buf| buf.extend_from_slice(&[1, 2, 3]));
        assert_eq!(&first[..], &[1, 2, 3]);
        assert_eq!(pool.idle(), 0, "in flight, not idle");
        drop(first);
        assert_eq!(pool.idle(), 1, "returned on drop");

        let second = pool.load(|buf| buf.extend_from_slice(&[4, 5]));
        assert_eq!(pool.idle(), 0, "reused the idle buffer, no new one");
        assert_eq!(&second[..], &[4, 5]);
    }

    #[test]
    fn idle_buffers_are_capped() {
        let pool = BytesPool::new(2, 1 << 20);
        let loans: Vec<_> = (0..4).map(|_| pool.load(|buf| buf.push(0))).collect();
        drop(loans);
        assert_eq!(pool.idle(), 2, "kept at most max_buffers");
    }

    #[test]
    fn oversized_buffers_are_not_retained() {
        let pool = BytesPool::new(4, 16);
        let big = pool.load(|buf| buf.extend(std::iter::repeat_n(0_u8, 1024)));
        drop(big);
        assert_eq!(
            pool.idle(),
            0,
            "a buffer above buffer_limit is freed, not pooled"
        );
    }

    #[test]
    fn a_live_clone_keeps_the_buffer_out_of_the_pool() {
        let pool = BytesPool::new(2, 1 << 20);
        let original = pool.load(|buf| buf.extend_from_slice(&[7, 8, 9]));
        let clone = original.clone();
        drop(original);
        assert_eq!(pool.idle(), 0, "still referenced by the clone");
        drop(clone);
        assert_eq!(pool.idle(), 1, "returns only after the last reference");
    }
}
