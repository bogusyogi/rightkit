//! `rightkit-cache` - cache primitives shared by Right Suite apps.
//!
//! - [`DiskCache`]: size-capped byte cache (attachments, remote images, rendered thumbnails) with LRU
//!   that survives restarts, atomic writes, group invalidation, fetch-or-cache, and a read-only open.
//! - [`CacheReservation`]: a temp path a file producer (ffmpeg, an encoder) writes, committed into
//!   the cache atomically or aborted.
//! - [`CacheSet`]: several roots, or namespaces under one root, with aggregated stats and a combined
//!   eviction budget.
//! - [`Key`]: unambiguous slot keys (length-prefixed SHA-256), optionally grouped.
//! - [`prefix`]: stable-prefix prompt-cache keys and break tracking from CodeRight.

pub mod disk;
pub mod key;
pub mod prefix;
pub mod reservation;
pub mod set;

pub use disk::{
    foreign_content_error, pinned_capacity_error, Config, DiskCache, DiskCacheStatus,
    ForeignContentError, PinnedCapacityError, PutOutcome, PutWriter, Stats,
};
pub use key::{group_id, Key};
pub use prefix::{CacheStatus, CacheTracker, ConductorStableKey, StableCacheKey};
pub use reservation::CacheReservation;
pub use set::{valid_namespace, CacheSet, SetStats};
