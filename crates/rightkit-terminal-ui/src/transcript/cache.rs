//! Width-keyed memoization for expensive per-message renders.
//!
//! A streamed token appends to exactly one message. Re-running markdown
//! parsing and wrapping for every *other* message on that frame would make
//! streaming cost grow with transcript length instead of staying constant.
//! Each message keeps its own cache, invalidated only when its content or the
//! column width it was wrapped to changes -- never on an unrelated append.
//!
//! Prior art: pi's `box.ts` (57-63, 98-122) and `markdown.ts` (248, 278) key
//! cached widget output on width and content the same way.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// A single cache slot: the last width and content fingerprint it was
/// computed for, plus hit/miss counters so callers can prove memoization is
/// actually doing something rather than assert it in a comment.
#[derive(Debug, Clone, Default)]
pub struct WidthCache<T> {
    key: Option<(u16, u64)>,
    value: Option<T>,
    hits: u64,
    misses: u64,
}

impl<T> WidthCache<T> {
    pub fn new() -> Self {
        Self {
            key: None,
            value: None,
            hits: 0,
            misses: 0,
        }
    }

    /// Return the cached value for `(width, content_hash)`, recomputing with
    /// `compute` only when either differs from what produced the cached
    /// value.
    pub fn get_or_compute(
        &mut self,
        width: u16,
        content_hash: u64,
        compute: impl FnOnce() -> T,
    ) -> &T {
        let key = (width, content_hash);
        if self.value.is_some() && self.key == Some(key) {
            self.hits += 1;
        } else {
            self.value = Some(compute());
            self.key = Some(key);
            self.misses += 1;
        }
        self.value.as_ref().expect("just set above")
    }

    /// The currently cached value, if any, without recomputing. Valid to
    /// call after a prior `get_or_compute` with the same key populated it.
    pub fn peek(&self) -> Option<&T> {
        self.value.as_ref()
    }

    pub fn hits(&self) -> u64 {
        self.hits
    }

    pub fn misses(&self) -> u64 {
        self.misses
    }
}

/// Fingerprint content for cache-key comparison. Not cryptographic -- this
/// only needs to distinguish "same bytes" from "different bytes" within one
/// process's lifetime.
pub fn hash_content<H: Hash>(value: &H) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}
