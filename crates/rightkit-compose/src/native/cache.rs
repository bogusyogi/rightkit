//! Bounded composed-frame cache with identity-keyed eviction.
//!
//! Adapted (rewritten) from Clypra `frame_cache.rs` / `frame_resource.rs`
//! concepts: entries are immutable `FrameResource`s keyed by `FrameKey`, the
//! budget is in bytes, eviction is LRU, and `invalidate_identity` drops every
//! frame belonging to a superseded snapshot — which is how project switches,
//! revisions and cancellations guarantee a stale frame can never be served.
//!
//! The cache stores opaque frame payloads (`FrameResource`) so it stays
//! independent of where pixels live. The GPU-backed implementation wraps a
//! `wgpu::Texture`; tests use CPU buffers.

/// One composed frame held by the cache. `payload` is owned by the caller's
/// transport (e.g. a `wgpu::Texture` clone, or RGBA bytes in tests).
#[derive(Debug)]
pub struct FrameResource<P> {
    pub key: crate::native::identity::FrameKey,
    /// Approximate bytes held by the payload, counted against the budget.
    pub byte_len: usize,
    pub payload: P,
}

/// LRU frame cache with a hard byte budget.
pub struct FrameCache<P> {
    /// Keyed by `FrameKey::cache_key`.
    entries: std::collections::HashMap<String, FrameResource<P>>,
    /// Oldest-first order of cache keys for eviction.
    order: std::collections::VecDeque<String>,
    budget_bytes: usize,
    used_bytes: usize,
}

impl<P> FrameCache<P> {
    pub fn new(budget_bytes: usize) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            order: std::collections::VecDeque::new(),
            budget_bytes,
            used_bytes: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn used_bytes(&self) -> usize {
        self.used_bytes
    }

    /// Insert a frame, evicting least-recently-used entries until it fits.
    /// A frame larger than the whole budget is not cached (returns `false`).
    pub fn insert(&mut self, resource: FrameResource<P>) -> bool {
        if resource.byte_len > self.budget_bytes {
            return false;
        }
        let key = resource.key.cache_key();
        if let Some(old) = self.entries.remove(&key) {
            self.used_bytes -= old.byte_len;
            self.order.retain(|k| k != &key);
        }
        while self.used_bytes + resource.byte_len > self.budget_bytes {
            match self.order.pop_front() {
                Some(evict) => {
                    if let Some(old) = self.entries.remove(&evict) {
                        self.used_bytes -= old.byte_len;
                    }
                }
                None => break,
            }
        }
        self.used_bytes += resource.byte_len;
        self.order.push_back(key.clone());
        self.entries.insert(key, resource);
        true
    }

    /// Presence check that does not refresh recency — prefetch scheduling
    /// asks "is this frame already produced" without making it MRU.
    pub fn contains(&self, key: &crate::native::identity::FrameKey) -> bool {
        self.entries.contains_key(&key.cache_key())
    }

    /// Newest cached frame in `(after, at_or_before]` — used to present the
    /// freshest prefetched frame when the exact due frame is not yet
    /// composed. Does not refresh recency.
    pub fn latest_leq(&self, after: i64, at_or_before: i64) -> Option<(i64, &P)> {
        self.entries
            .values()
            .filter(|r| r.key.frame > after && r.key.frame <= at_or_before)
            .max_by_key(|r| r.key.frame)
            .map(|r| (r.key.frame, &r.payload))
    }

    /// Fetch and refresh recency.
    pub fn get(&mut self, key: &crate::native::identity::FrameKey) -> Option<&P> {
        let ck = key.cache_key();
        if self.entries.contains_key(&ck) {
            self.order.retain(|k| k != &ck);
            self.order.push_back(ck.clone());
            return self.entries.get(&ck).map(|r| &r.payload);
        }
        None
    }

    /// Drop every frame belonging to a superseded snapshot identity. Called
    /// on revision change, cancellation and project switch; afterwards a stale
    /// request can only miss, never hit old pixels.
    pub fn invalidate_identity(&mut self, identity: &crate::native::snapshot::SnapshotIdentity) {
        let stale: Vec<String> = self
            .entries
            .iter()
            .filter(|(_, r)| &r.key.identity == identity)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &stale {
            if let Some(old) = self.entries.remove(k) {
                self.used_bytes -= old.byte_len;
            }
        }
        let stale: std::collections::HashSet<&String> = stale.iter().collect();
        self.order.retain(|k| !stale.contains(k));
    }

    pub fn clear(&mut self) {
        self.entries.clear();
        self.order.clear();
        self.used_bytes = 0;
    }
}
