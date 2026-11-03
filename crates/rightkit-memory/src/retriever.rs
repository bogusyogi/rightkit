//! Adaptive `RoutingPolicy` bandit for memory retrieval (FM-071).
//!
//! Tiers:
//! - `Low`  → lexical only (FTS5 / keyword)
//! - `Mid`  → hybrid  (lexical + embedding cosine, RRF)
//! - `High` → full hybrid with wider candidate set + MMR diversity
//!
//! The bandit tracks per-tier success via an exponential moving average and
//! uses epsilon-greedy selection. When `embedder_available` is false the
//! policy falls back to `Low` regardless of the bandit's preference, so a
//! missing `fastembed` build never degrades availability.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Tier
// ---------------------------------------------------------------------------

/// Retrieval depth tier. Serialized as lowercase for config/JSON surfaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    #[default]
    Low,
    Mid,
    High,
}

/// Alias required by the contract's `RoutingPolicy` interface table.
pub use Tier as RetrievalTier;

impl Tier {
    /// All tiers in bandit arm order.
    pub const ALL: [Tier; 3] = [Tier::Low, Tier::Mid, Tier::High];

    pub fn index(self) -> usize {
        match self {
            Tier::Low => 0,
            Tier::Mid => 1,
            Tier::High => 2,
        }
    }

    pub fn from_index(idx: usize) -> Self {
        match idx {
            0 => Tier::Low,
            1 => Tier::Mid,
            _ => Tier::High,
        }
    }

    /// Human-readable strategy name for diagnostics.
    pub fn strategy(self) -> &'static str {
        match self {
            Tier::Low => "lexical",
            Tier::Mid => "hybrid",
            Tier::High => "full_hybrid",
        }
    }

    /// Whether this tier requires an embedder. `Low` is embedder-free.
    pub fn requires_embedder(self) -> bool {
        self != Tier::Low
    }
}

impl std::fmt::Display for Tier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Tier::Low => "low",
            Tier::Mid => "mid",
            Tier::High => "high",
        })
    }
}

// ---------------------------------------------------------------------------
// Bandit arm stats
// ---------------------------------------------------------------------------

/// Per-tier bandit statistics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct BanditStats {
    /// Number of times this tier was chosen.
    pub trials: u32,
    /// Number of successful recalls (reward = 1).
    pub successes: u32,
    /// EMA of observed reward in [0, 1]. Initialized to 0.5 (uninformative prior).
    pub reward_ema: f64,
}

impl Default for BanditStats {
    fn default() -> Self {
        Self {
            trials: 0,
            successes: 0,
            reward_ema: 0.5,
        }
    }
}

impl BanditStats {
    fn observe(&mut self, success: bool, alpha: f64) {
        self.trials = self.trials.saturating_add(1);
        if success {
            self.successes = self.successes.saturating_add(1);
        }
        let r = if success { 1.0 } else { 0.0 };
        self.reward_ema = alpha.mul_add(r, (1.0 - alpha) * self.reward_ema);
        // Clamp to [0, 1] to absorb floating-point drift.
        self.reward_ema = self.reward_ema.clamp(0.0, 1.0);
    }

    /// Upper-confidence-ish bonus that decays with trials, encouraging
    /// exploration of under-tried arms without a full UCB implementation.
    fn exploration_bonus(&self) -> f64 {
        if self.trials == 0 {
            0.35
        } else {
            0.15 / ((self.trials as f64).ln() + 1.0)
        }
    }
}

// ---------------------------------------------------------------------------
// RoutingPolicy
// ---------------------------------------------------------------------------

/// Adaptive routing policy — a 3-armed bandit over `Tier`.
///
/// Thread-safety: wrap in `Arc<Mutex<_>>` at the call site (e.g. `DaemonState`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoutingPolicy {
    /// Per-tier bandit state, indexed by `Tier::index()`.
    pub arms: [BanditStats; 3],
    /// EMA learning rate. 0.3 is a moderate adaptation speed.
    pub alpha: f64,
    /// Epsilon for epsilon-greedy exploration. 0.10 means 10% random.
    pub epsilon: f64,
    /// Whether an embedding model is currently available (`fastembed` feature
    /// + model loaded). When false, `choose` always returns `Low`.
    pub embedder_available: bool,
    /// Total number of `choose` calls (for diagnostics).
    pub total_choices: u64,
}

impl Default for RoutingPolicy {
    fn default() -> Self {
        Self {
            arms: [BanditStats::default(); 3],
            alpha: 0.3,
            epsilon: 0.10,
            embedder_available: false,
            total_choices: 0,
        }
    }
}

impl RoutingPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Create with an explicit embedder-availability hint.
    pub fn with_embedder_available(available: bool) -> Self {
        Self {
            embedder_available: available,
            ..Self::default()
        }
    }

    /// Override exploration and learning rates (for tests / config).
    pub fn with_params(mut self, alpha: f64, epsilon: f64) -> Self {
        self.alpha = alpha.clamp(0.01, 0.9);
        self.epsilon = epsilon.clamp(0.0, 0.5);
        self
    }

    /// Set embedder availability (e.g. after a load failure or `fastembed`
    /// feature probe at daemon start).
    pub fn set_embedder_available(&mut self, available: bool) {
        self.embedder_available = available;
    }

    /// Choose a tier for the next retrieval.
    ///
    /// When `embedder_available` is false this is a hard lexical fallback —
    /// always `Low` — regardless of bandit state. Otherwise an epsilon-greedy
    /// pick over `reward_ema + exploration_bonus`.
    pub fn choose(&mut self, _query: &str, embedder_available: Option<bool>) -> Tier {
        let available = embedder_available.unwrap_or(self.embedder_available);
        self.total_choices = self.total_choices.saturating_add(1);
        if !available {
            return Tier::Low;
        }
        self.choose_bandit()
    }

    /// Choose using the bandit's current estimates (embedder assumed available).
    /// Deterministic tie-break: lower tier wins when scores are equal.
    pub fn choose_bandit(&self) -> Tier {
        // Epsilon-greedy exploration: with probability epsilon pick a random
        // under-explored arm. For deterministic tests we use a cheap LCG
        // seeded from total_choices rather than pulling `rand`.
        if self.should_explore() {
            return self.least_tried_tier();
        }
        let mut best = Tier::Low;
        let mut best_score = f64::NEG_INFINITY;
        for &tier in &Tier::ALL {
            let arm = &self.arms[tier.index()];
            let score = arm.reward_ema + arm.exploration_bonus();
            if score > best_score {
                best_score = score;
                best = tier;
            }
        }
        best
    }

    /// Query-free tier selection with explicit embedder flag (alias for `choose`).
    pub fn tier_for(&mut self, embedder_available: bool) -> Tier {
        self.choose("", Some(embedder_available))
    }

    /// Record a successful recall for `tier` (reward = 1).
    pub fn reward(&mut self, tier: Tier, success: bool) {
        self.observe(tier, success);
    }

    /// Backwards-compatible alias: reward on success, penalty on miss.
    pub fn observe_outcome(&mut self, tier: Tier, hit: bool) {
        self.observe(tier, hit);
    }

    /// Convenience: successful recall.
    pub fn observe_success(&mut self, tier: Tier) {
        self.observe(tier, true);
    }

    /// Convenience: failed / missed recall (penalty).
    pub fn observe_miss(&mut self, tier: Tier) {
        self.observe(tier, false);
    }

    fn observe(&mut self, tier: Tier, success: bool) {
        let idx = tier.index();
        self.arms[idx].observe(success, self.alpha);
    }

    /// Effective reward estimate for a tier (for diagnostics / tests).
    pub fn estimated_reward(&self, tier: Tier) -> f64 {
        self.arms[tier.index()].reward_ema
    }

    /// Snapshot of per-tier stats for metrics emission.
    pub fn snapshot(&self) -> [BanditStats; 3] {
        self.arms
    }

    // --- internals ---

    fn should_explore(&self) -> bool {
        if self.epsilon <= 0.0 {
            return false;
        }
        // Deterministic pseudo-random in [0,1) from total_choices.
        let x = hash_u64(self.total_choices);
        let r = (x as f64) / (u64::MAX as f64);
        r < self.epsilon
    }

    fn least_tried_tier(&self) -> Tier {
        let mut best = Tier::Low;
        let mut best_trials = u32::MAX;
        for &tier in &Tier::ALL {
            let t = self.arms[tier.index()].trials;
            if t < best_trials {
                best_trials = t;
                best = tier;
            }
        }
        best
    }
}

fn hash_u64(mut x: u64) -> u64 {
    // SplitMix64 — cheap, deterministic.
    x = x.wrapping_add(0x9e3779b97f4a7c15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^ (z >> 31)
}

// ---------------------------------------------------------------------------
// Retrieval helpers (lexical vs hybrid selection)
// ---------------------------------------------------------------------------

/// Select the candidate-set budget for a tier. Call sites use this to size
/// their FTS / vector fan-out.
pub fn candidate_limit_for(tier: Tier) -> usize {
    match tier {
        Tier::Low => 20,
        Tier::Mid => 50,
        Tier::High => 120,
    }
}

/// Whether a tier should consult embeddings. `Low` never does.
pub fn use_embeddings(tier: Tier, embedder_available: bool) -> bool {
    tier.requires_embedder() && embedder_available
}

// ---------------------------------------------------------------------------
// Session recap retrieval
// ---------------------------------------------------------------------------

/// Subject key used when a caller persists a generated session recap as a
/// `RecallCandidate` or `MemoryRecord`.
pub const SESSION_RECAP_SUBJECT_KEY: &str = "session_recap";

/// Return stored session recaps, ordered through the existing relevance and
/// recency policy. Other memory candidates are left untouched by this
/// additive helper and remain available to existing retrieval callers.
pub fn retrieve_prior_recaps(candidates: Vec<RecallCandidate>) -> Vec<RecallCandidate> {
    let recaps = candidates
        .into_iter()
        .filter(|candidate| candidate.subject_key == SESSION_RECAP_SUBJECT_KEY)
        .collect();
    rank_by_relevance_and_recency(recaps)
}

// ---------------------------------------------------------------------------
// Recall curation
// ---------------------------------------------------------------------------

/// Backend-neutral recall row. The daemon maps MemRight records into this
/// shape so dedupe/contradiction/recency policy stays in the existing memory
/// crate without introducing another index or store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallCandidate {
    pub id: String,
    pub scope: String,
    pub content: String,
    /// Caller-computed content digest (the host app uses SHA-256).
    pub digest: String,
    /// Stable semantic subject key, normally normalized sorted keywords.
    pub subject_key: String,
    pub created_at: String,
    pub score: f64,
    pub access_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RecallDropReason {
    Duplicate,
    ContradictedByNewer,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RecallDrop {
    pub id: String,
    pub kept_id: String,
    pub reason: RecallDropReason,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CuratedRecall {
    pub admitted: Vec<RecallCandidate>,
    pub dropped: Vec<RecallDrop>,
}

/// Dedupe exact content and collapse contradictory rows sharing a non-empty
/// subject key. Newer facts win; ties prefer higher retrieval score, then
/// higher access count, then stable ID. The resulting list is score ordered.
pub fn curate_recall_candidates(candidates: Vec<RecallCandidate>) -> CuratedRecall {
    let mut ordered = candidates;
    ordered.sort_by(|left, right| {
        right
            .created_at
            .cmp(&left.created_at)
            .then_with(|| right.score.total_cmp(&left.score))
            .then_with(|| right.access_count.cmp(&left.access_count))
            .then_with(|| left.id.cmp(&right.id))
    });

    let mut digest_owner = std::collections::BTreeMap::<String, String>::new();
    let mut subject_owner = std::collections::BTreeMap::<String, (String, String)>::new();
    let mut curated = CuratedRecall::default();
    for candidate in ordered {
        if let Some(kept_id) = digest_owner.get(&candidate.digest) {
            curated.dropped.push(RecallDrop {
                id: candidate.id,
                kept_id: kept_id.clone(),
                reason: RecallDropReason::Duplicate,
            });
            continue;
        }
        if !candidate.subject_key.is_empty() {
            if let Some((kept_id, kept_digest)) = subject_owner.get(&candidate.subject_key) {
                if kept_digest != &candidate.digest {
                    curated.dropped.push(RecallDrop {
                        id: candidate.id,
                        kept_id: kept_id.clone(),
                        reason: RecallDropReason::ContradictedByNewer,
                    });
                    continue;
                }
            }
        }
        digest_owner.insert(candidate.digest.clone(), candidate.id.clone());
        if !candidate.subject_key.is_empty() {
            subject_owner.insert(
                candidate.subject_key.clone(),
                (candidate.id.clone(), candidate.digest.clone()),
            );
        }
        curated.admitted.push(candidate);
    }
    curated.admitted.sort_by(|left, right| {
        right
            .score
            .total_cmp(&left.score)
            .then_with(|| right.created_at.cmp(&left.created_at))
            .then_with(|| left.id.cmp(&right.id))
    });
    curated
}

// ---------------------------------------------------------------------------
// Relevance + recency ranking
// ---------------------------------------------------------------------------

/// Weight given to the caller-computed lexical/hybrid `score` in the final
/// ranking (ENG-81 "ranks by relevance and recency").
pub const RELEVANCE_WEIGHT: f64 = 0.7;
/// Weight given to recency. Recency is expressed as a rank-based fraction
/// (newest = 1.0, oldest admitted = ~0.0) rather than a wall-clock decay, so
/// this stays dependency-free (no date/time crate) and works uniformly
/// regardless of how far apart timestamps are.
pub const RECENCY_WEIGHT: f64 = 0.3;

/// Rank candidates by a blend of relevance (`score`) and recency
/// (`created_at` order). Run this after `curate_recall_candidates` so
/// duplicates/contradictions are already resolved; this function does not
/// drop anything, it only reorders.
///
/// `created_at` is compared as a string, matching every other ordering in
/// this module -- callers already emit sortable (ISO-8601-like) timestamps,
/// so no date-parsing dependency is needed.
pub fn rank_by_relevance_and_recency(candidates: Vec<RecallCandidate>) -> Vec<RecallCandidate> {
    let total = candidates.len();
    if total <= 1 {
        return candidates;
    }

    // Recency rank: 0 = newest.
    let mut recency_order: Vec<usize> = (0..total).collect();
    recency_order.sort_by(|&a, &b| candidates[b].created_at.cmp(&candidates[a].created_at));
    let mut recency_rank = vec![0usize; total];
    for (rank, &idx) in recency_order.iter().enumerate() {
        recency_rank[idx] = rank;
    }

    let denom = (total.saturating_sub(1)).max(1) as f64;
    let mut combined: Vec<(f64, usize)> = (0..total)
        .map(|i| {
            let recency_score = 1.0 - (recency_rank[i] as f64 / denom);
            let blended = candidates[i].score * RELEVANCE_WEIGHT + recency_score * RECENCY_WEIGHT;
            (blended, i)
        })
        .collect();

    combined.sort_by(|a, b| {
        b.0.total_cmp(&a.0)
            .then_with(|| candidates[b.1].created_at.cmp(&candidates[a.1].created_at))
            .then_with(|| candidates[a.1].id.cmp(&candidates[b.1].id))
    });

    combined
        .into_iter()
        .map(|(_, i)| candidates[i].clone())
        .collect()
}
