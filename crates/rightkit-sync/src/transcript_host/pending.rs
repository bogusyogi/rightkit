//! Pure event latch used by the scheduler.
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

/// A cheap, lock-free pending-work latch.  Requests made while a cycle is in
/// flight set the bit again; the worker drains it on its next pass.
#[derive(Debug, Default)]
pub(crate) struct PendingWork {
    bits: AtomicU8,
    generation: AtomicU64,
}

impl PendingWork {
    pub(crate) fn request(&self, bits: u8) {
        if bits == 0 {
            return;
        }
        self.bits.fetch_or(bits, Ordering::AcqRel);
        self.generation.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn requeue(&self, bits: u8) {
        self.bits.fetch_or(bits, Ordering::AcqRel);
    }

    pub(crate) fn take(&self) -> u8 {
        self.bits.swap(0, Ordering::AcqRel)
    }

    pub(crate) fn clear(&self) {
        self.bits.store(0, Ordering::Release);
    }

    pub(crate) fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
}
