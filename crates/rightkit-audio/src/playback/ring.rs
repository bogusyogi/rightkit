//! Bounded lock-free single-producer single-consumer ring of interleaved `f32` samples.
//!
//! Written in this crate on purpose (no new dependency). Storage is a boxed slice of
//! `AtomicU32` holding `f32` bit patterns, so the whole ring is safe Rust: sample slots use
//! `Relaxed` loads and stores, and the monotonic `head`/`tail` counters carry the
//! `Release`/`Acquire` ordering that publishes them.
//!
//! Roles: exactly one thread pushes (the producer owns `tail`), exactly one thread pops or
//! discards (the consumer owns `head`). The counters are total sample counts since creation
//! (they never wrap in practice: a `u64` of samples outlasts any session), so
//! `tail - head` is the queued length and slot index is `counter % capacity`.
//!
//! Nothing here allocates after `new`, locks, or blocks.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

pub(crate) struct SampleRing {
    buf: Box<[AtomicU32]>,
    /// Total samples consumed. Written only by the consumer.
    head: AtomicU64,
    /// Total samples produced. Written only by the producer.
    tail: AtomicU64,
}

impl SampleRing {
    /// `capacity` is in samples (interleaved), and must be at least 1.
    pub(crate) fn new(capacity: usize) -> Self {
        let capacity = capacity.max(1);
        let buf = (0..capacity)
            .map(|_| AtomicU32::new(0f32.to_bits()))
            .collect::<Vec<_>>()
            .into_boxed_slice();
        Self {
            buf,
            head: AtomicU64::new(0),
            tail: AtomicU64::new(0),
        }
    }

    pub(crate) fn capacity(&self) -> usize {
        self.buf.len()
    }

    /// Total samples consumed (popped or discarded) since creation.
    pub(crate) fn head(&self) -> u64 {
        self.head.load(Ordering::Acquire)
    }

    /// Total samples produced since creation.
    pub(crate) fn tail(&self) -> u64 {
        self.tail.load(Ordering::Acquire)
    }

    /// Samples currently queued.
    pub(crate) fn len(&self) -> usize {
        // head first, then tail: tail is monotonic, so the later load is never behind head.
        let head = self.head.load(Ordering::Acquire);
        let tail = self.tail.load(Ordering::Acquire);
        tail.saturating_sub(head) as usize
    }

    /// Producer side. Copies as many whole units (`unit` = channel count, so the ring stays
    /// frame aligned) as fit and returns the number of samples accepted. Never blocks.
    pub(crate) fn push(&self, src: &[f32], unit: usize) -> usize {
        let unit = unit.max(1);
        let cap = self.buf.len();
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        let used = tail.saturating_sub(head) as usize;
        let free = cap.saturating_sub(used);
        let mut n = free.min(src.len());
        n -= n % unit;
        if n == 0 {
            return 0;
        }
        let start = (tail % cap as u64) as usize;
        let first = n.min(cap - start);
        for (slot, v) in self.buf[start..start + first].iter().zip(&src[..first]) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
        for (slot, v) in self.buf[..n - first].iter().zip(&src[first..n]) {
            slot.store(v.to_bits(), Ordering::Relaxed);
        }
        self.tail.store(tail + n as u64, Ordering::Release);
        n
    }

    /// Consumer side. Fills `out` from the front of the queue and returns the number of
    /// samples written; the caller handles any shortfall (silence on underrun).
    pub(crate) fn pop_into(&self, out: &mut [f32]) -> usize {
        let cap = self.buf.len();
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let avail = tail.saturating_sub(head) as usize;
        let n = avail.min(out.len());
        if n == 0 {
            return 0;
        }
        let start = (head % cap as u64) as usize;
        let first = n.min(cap - start);
        for (dst, slot) in out[..first].iter_mut().zip(&self.buf[start..start + first]) {
            *dst = f32::from_bits(slot.load(Ordering::Relaxed));
        }
        for (dst, slot) in out[first..n].iter_mut().zip(&self.buf[..n - first]) {
            *dst = f32::from_bits(slot.load(Ordering::Relaxed));
        }
        self.head.store(head + n as u64, Ordering::Release);
        n
    }

    /// Consumer side. Drops queued samples up to the absolute index `target` (clamped to what
    /// has been produced, never moving backwards).
    pub(crate) fn discard_to(&self, target: u64) {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        let new = target.min(tail).max(head);
        if new != head {
            self.head.store(new, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seq(start: usize, n: usize) -> Vec<f32> {
        (start..start + n).map(|v| v as f32).collect()
    }

    #[test]
    fn wraparound_preserves_order() {
        let ring = SampleRing::new(8);
        assert_eq!(ring.push(&seq(0, 6), 2), 6);
        let mut out = [0.0f32; 4];
        assert_eq!(ring.pop_into(&mut out), 4);
        assert_eq!(out, [0.0, 1.0, 2.0, 3.0]);
        // 2 queued, tail at 6: this write wraps past the end of storage.
        assert_eq!(ring.push(&seq(6, 6), 2), 6);
        assert_eq!(ring.len(), 8);
        let mut all = [0.0f32; 8];
        assert_eq!(ring.pop_into(&mut all), 8);
        assert_eq!(all.to_vec(), seq(4, 8));
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn overrun_returns_accepted_count_and_keeps_queued_data() {
        let ring = SampleRing::new(8);
        assert_eq!(ring.push(&seq(0, 10), 2), 8);
        assert_eq!(ring.push(&seq(100, 4), 2), 0);
        let mut out = [0.0f32; 8];
        assert_eq!(ring.pop_into(&mut out), 8);
        assert_eq!(out.to_vec(), seq(0, 8));
    }

    #[test]
    fn push_stays_frame_aligned() {
        let ring = SampleRing::new(8);
        // 7 samples of stereo: the trailing half frame is not accepted.
        assert_eq!(ring.push(&seq(0, 7), 2), 6);
        assert_eq!(ring.len(), 6);
    }

    #[test]
    fn underrun_pop_returns_short_count() {
        let ring = SampleRing::new(8);
        assert_eq!(ring.push(&seq(0, 2), 2), 2);
        let mut out = [9.0f32; 6];
        assert_eq!(ring.pop_into(&mut out), 2);
        assert_eq!(&out[..2], &[0.0, 1.0]);
        assert_eq!(ring.pop_into(&mut out), 0);
    }

    #[test]
    fn discard_to_clamps_and_never_moves_backwards() {
        let ring = SampleRing::new(8);
        ring.push(&seq(0, 6), 2);
        ring.discard_to(4);
        assert_eq!(ring.head(), 4);
        ring.discard_to(2);
        assert_eq!(ring.head(), 4);
        ring.discard_to(1_000);
        assert_eq!(ring.head(), 6);
        assert_eq!(ring.len(), 0);
    }
}
