//! `rightkit-cache` - cache primitives shared by Right Suite apps.
//!
//! - [`DiskCache`]: size-capped byte cache (attachments, remote images) with LRU that survives restarts,
//!   atomic writes, group invalidation and fetch-or-cache.
//! - [`Key`]: unambiguous slot keys (length-prefixed SHA-256), optionally grouped.
//! - [`prefix`]: stable-prefix prompt-cache keys and break tracking from CodeRight.

pub mod disk;
pub mod key;
pub mod prefix;

pub use disk::{Config, DiskCache, PutOutcome, Stats};
pub use key::{group_id, Key};
pub use prefix::{CacheStatus, CacheTracker, ConductorStableKey, StableCacheKey};
