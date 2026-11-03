use serde_json;

/// Auto-approval tier for tool calls.
///
/// Mind the GAP (arXiv 2602.16943) — text-safety does NOT transfer to tool-call safety;
/// train on tool-call features only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutoApprovalTier {
    /// File edits auto-apply without asking.
    AutoEdit,
    /// Shell commands ask for approval.
    AskShell,
    /// All actions ask for approval.
    AlwaysAsk,
}

/// Features extracted from a tool call for approval classification.
///
/// These are tool-call-specific signals, NOT text-safety signals.
#[derive(Debug, Clone)]
pub struct ToolCallFeatures {
    pub tool: String,
    pub touches_outside_workspace: bool,
    pub is_destructive: bool,
    pub command_len: usize,
    pub is_test_file: bool,
    pub workspace_trust: crate::types::WorkspaceTrust,
}

impl ToolCallFeatures {
    pub fn new(tool: impl Into<String>) -> Self {
        Self {
            tool: tool.into(),
            touches_outside_workspace: false,
            is_destructive: false,
            command_len: 0,
            is_test_file: false,
            workspace_trust: crate::types::WorkspaceTrust::Trusted,
        }
    }

    pub fn with_outside_workspace(mut self, outside: bool) -> Self {
        self.touches_outside_workspace = outside;
        self
    }

    pub fn with_destructive(mut self, destructive: bool) -> Self {
        self.is_destructive = destructive;
        self
    }

    pub fn with_command_len(mut self, len: usize) -> Self {
        self.command_len = len;
        self
    }

    pub fn with_test_file(mut self, is_test: bool) -> Self {
        self.is_test_file = is_test;
        self
    }

    pub fn with_workspace_trust(mut self, trust: crate::types::WorkspaceTrust) -> Self {
        self.workspace_trust = trust;
        self
    }
}

/// Authority context which must be bound to every adaptive approval entry.
///
/// Callers must provide non-empty policy/rules revisions, workspace identity,
/// and permission context. The latter should include every active mode,
/// principal, and permission grant that can affect the verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoApprovalContext {
    pub policy_revision: String,
    pub rules_revision: String,
    pub workspace_identity: String,
    pub permission_context: String,
}

impl AutoApprovalContext {
    pub fn new(
        policy_revision: impl Into<String>,
        rules_revision: impl Into<String>,
        workspace_identity: impl Into<String>,
        permission_context: impl Into<String>,
    ) -> Self {
        Self {
            policy_revision: policy_revision.into(),
            rules_revision: rules_revision.into(),
            workspace_identity: workspace_identity.into(),
            permission_context: permission_context.into(),
        }
    }

    fn is_complete(&self) -> bool {
        [
            &self.policy_revision,
            &self.rules_revision,
            &self.workspace_identity,
            &self.permission_context,
        ]
        .iter()
        .all(|value| !value.trim().is_empty())
    }
}

/// ML-style auto-approval classifier trained on tool-call features.
///
/// Weights are adjusted via online training on tool-call-specific signals.
/// The classifier learns which tool+feature combinations should auto-approve
/// (AutoEdit), ask with shell review (AskShell), or always ask (AlwaysAsk).
pub struct AutoApprovalClassifier {
    weights: Vec<f64>,
}

impl AutoApprovalClassifier {
    /// Create a new classifier with defaults.
    ///
    /// Defaults:
    /// - File writes (Edit, Write tools) -> AutoEdit
    /// - Shell commands -> AskShell
    /// - Destructive ops -> AlwaysAsk
    pub fn new() -> Self {
        Self {
            // [file_tool, outside_workspace, destructive, command_len, is_test_file, workspace_untrusted]
            weights: vec![0.8, -0.5, -1.0, 0.1, -1.2, -0.6],
        }
    }

    /// Classify a tool call based on its features.
    ///
    /// Returns the approval tier based on current weights and feature signals.
    pub fn classify(&self, f: &ToolCallFeatures) -> AutoApprovalTier {
        let score = self.compute_score(f);

        if score > 0.5 {
            AutoApprovalTier::AutoEdit
        } else if score > -0.2 {
            AutoApprovalTier::AskShell
        } else {
            AutoApprovalTier::AlwaysAsk
        }
    }

    /// Half-width of the band around each classify() threshold (0.5, -0.2)
    /// treated as gray-zone (S:B4). A score this close to a boundary is not a
    /// confident classification -- online weight updates (`train`) move the
    /// score continuously, so a call landing a hair on either side of 0.5 is
    /// not meaningfully different in intent from one a hair on the other
    /// side, yet one auto-applies and the other prompts. Surfacing the
    /// closeness lets the caller ask for explicit, reasoned review instead of
    /// trusting a threshold crossing that the model itself is barely sure of.
    pub const GRAY_ZONE_BAND: f64 = 0.08;

    /// Whether `f`'s score falls within [`Self::GRAY_ZONE_BAND`] of either decision
    /// boundary. Independent of which tier the score lands in.
    pub fn is_gray_zone(&self, f: &ToolCallFeatures) -> bool {
        Self::score_is_gray_zone(self.compute_score(f))
    }

    /// Pure boundary check, split out from [`is_gray_zone`] so the threshold
    /// math is directly testable without needing feature vectors that happen
    /// to produce a score in the band.
    fn score_is_gray_zone(score: f64) -> bool {
        (score - 0.5).abs() <= Self::GRAY_ZONE_BAND
            || (score - (-0.2)).abs() <= Self::GRAY_ZONE_BAND
    }

    /// Classify with an explicit gray-zone flag (S:B4).
    ///
    /// The tier is unchanged from [`Self::classify`] -- gray-zone status never
    /// changes the caller's behavior on its own, only whether the decision is
    /// annotated as requiring reviewer attention rather than presented as
    /// confident. This keeps `classify` cheap for callers that only need the
    /// tier and don't care about the boundary distance.
    pub fn classify_reviewable(&self, f: &ToolCallFeatures) -> (AutoApprovalTier, bool) {
        (self.classify(f), self.is_gray_zone(f))
    }

    /// Raw score (f32-compatible) for Goose-discipline caching.
    ///
    /// `> 0.5` → confident Allow (cache), `(-0.2, 0.5]` → uncertain → Ask,
    /// `<= -0.2` → AlwaysAsk. This is the `classify() -> f32` seam the contract
    /// names — the f64 here is the source of that f32.
    pub fn score(&self, f: &ToolCallFeatures) -> f64 {
        self.compute_score(f)
    }

    /// Online weight update based on user approval decision.
    ///
    /// If the user approved a tool call, increase weights in the direction of
    /// approval. If denied, decrease them. This is a simple Hebbian-style update.
    pub fn train(&mut self, f: &ToolCallFeatures, approved: bool) {
        let delta = if approved { 0.05 } else { -0.05 };

        let features = self.feature_vector(f);
        for (i, &feat) in features.iter().enumerate() {
            if i < self.weights.len() {
                self.weights[i] += delta * feat;
            }
        }
    }

    fn compute_score(&self, f: &ToolCallFeatures) -> f64 {
        let features = self.feature_vector(f);
        features
            .iter()
            .zip(self.weights.iter())
            .map(|(feat, weight)| feat * weight)
            .sum()
    }

    fn feature_vector(&self, f: &ToolCallFeatures) -> Vec<f64> {
        vec![
            self.feature_is_file_tool(&f.tool),
            if f.touches_outside_workspace {
                1.0
            } else {
                0.0
            },
            if f.is_destructive { 1.0 } else { 0.0 },
            (f.command_len as f64).min(1000.0) / 1000.0,
            if f.is_test_file { 1.0 } else { 0.0 },
            if matches!(
                f.workspace_trust,
                crate::types::WorkspaceTrust::Untrusted | crate::types::WorkspaceTrust::Sandboxed
            ) {
                1.0
            } else {
                0.0
            },
        ]
    }

    fn feature_is_file_tool(&self, tool: &str) -> f64 {
        match tool.to_lowercase().as_str() {
            // Only the model-facing aliases score as file tools here. The
            // canonical registry names (`write_file`, `edit_file`, …) are
            // deliberately NOT added: this feature feeds the FM-029 adaptive
            // Ask->Allow classifier, and scoring them 1.0 lifts a plain
            // `write_file` into `AutoEdit`, silently converting an explicit
            // `PermissionMode::Ask` into `Allow` with no approval event. Ask
            // must ask; broadening this table is a permission change, not a
            // naming fix.
            "edit" | "write" => 1.0,
            "read" | "glob" => 0.3,
            _ => 0.0,
        }
    }
}

impl Default for AutoApprovalClassifier {
    fn default() -> Self {
        Self::new()
    }
}

/// Time-to-live for the adaptive Ask→Allow cache (Goose discipline: 10 min).
pub const AUTO_APPROVAL_CACHE_TTL_SECS: u64 = 600;

/// Adaptive Ask→Allow cache with Goose discipline.
///
/// - Negative-only: only `Allow` (score > 0.5) is cached, never `Ask`/destructive.
/// - Uncertainty → Ask: scores in `(-0.2, 0.5]` are not cached.
/// - Contextual entries bind tool, normalized args/features, policy/rules
///   revisions, workspace identity, trust, and permission context so an allow
///   cannot cross an authority boundary.
/// - TTL 10 min; entries expire automatically on read/write via `prune_expired`.
#[derive(Debug, Clone)]
pub struct AutoApprovalCache {
    entries: std::collections::HashMap<String, std::time::Instant>,
    ttl: std::time::Duration,
}

impl AutoApprovalCache {
    pub fn new() -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            ttl: std::time::Duration::from_secs(AUTO_APPROVAL_CACHE_TTL_SECS),
        }
    }

    pub fn with_ttl(ttl: std::time::Duration) -> Self {
        Self {
            entries: std::collections::HashMap::new(),
            ttl,
        }
    }

    /// Insert a key as cached Allow at now.
    fn insert(&mut self, key: String) {
        self.entries.insert(key, std::time::Instant::now());
    }

    /// Insert an approval only when all authority context is present.
    /// Returns `false` for incomplete context, preserving fail-closed behavior.
    pub fn insert_call(
        &mut self,
        features: &ToolCallFeatures,
        args_json: &str,
        context: &AutoApprovalContext,
    ) -> bool {
        // Keep the legacy entry point safe even when a caller does not carry
        // its classifier instance. An approval cache must never become an
        // accidental allow-list for shell, destructive, test-file, or
        // boundary-adjacent calls.
        let classifier = AutoApprovalClassifier::new();
        self.insert_classified_call(&classifier, features, args_json, context)
    }

    /// Cache a call only when the supplied classifier has a confident
    /// AutoEdit result. This is the Goose cache-safety invariant at the
    /// insertion seam: AskShell, AlwaysAsk, and gray-zone decisions can never
    /// become cached Allows, even if a caller invokes this method directly.
    pub fn insert_classified_call(
        &mut self,
        classifier: &AutoApprovalClassifier,
        features: &ToolCallFeatures,
        args_json: &str,
        context: &AutoApprovalContext,
    ) -> bool {
        if classifier.classify(features) != AutoApprovalTier::AutoEdit
            || classifier.is_gray_zone(features)
        {
            return false;
        }
        let Some(key) = cache_key_for_call(features, args_json, context) else {
            return false;
        };
        self.insert(key);
        true
    }

    /// Returns true if key is present and not expired.
    fn contains(&self, key: &str) -> bool {
        if let Some(at) = self.entries.get(key) {
            at.elapsed() < self.ttl
        } else {
            false
        }
    }

    /// Check an approval using its complete authority context.
    /// Incomplete context can never hit an existing entry.
    pub fn contains_call(
        &self,
        features: &ToolCallFeatures,
        args_json: &str,
        context: &AutoApprovalContext,
    ) -> bool {
        cache_key_for_call(features, args_json, context).is_some_and(|key| self.contains(&key))
    }

    /// Remove expired entries.
    pub fn prune_expired(&mut self) {
        let ttl = self.ttl;
        self.entries.retain(|_, at| at.elapsed() < ttl);
    }

    pub fn len(&mut self) -> usize {
        self.prune_expired();
        self.entries.len()
    }

    pub fn is_empty(&mut self) -> bool {
        self.len() == 0
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

impl Default for AutoApprovalCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Normalize tool arguments to a canonical string for cache keying.
///
/// Parses `args_json` as JSON, sorts object keys recursively, and serializes
/// deterministically. Falls back to trimmed raw JSON on parse failure so the
/// cache never panics on malformed input.
pub fn normalize_tool_args(args_json: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(args_json) {
        Ok(v) => {
            let sorted = sort_value(v);
            serde_json::to_string(&sorted).unwrap_or_else(|_| args_json.trim().to_string())
        }
        Err(_) => args_json.trim().to_string(),
    }
}

fn sort_value(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut sorted = serde_json::Map::new();
            let mut keys: Vec<String> = map.keys().cloned().collect();
            keys.sort();
            for k in keys {
                if let Some(val) = map.get(&k) {
                    sorted.insert(k.clone(), sort_value(val.clone()));
                }
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(sort_value).collect())
        }
        other => other,
    }
}

/// Legacy cache key for `(tool, normalized_args)` — lower-cased tool,
/// normalized JSON. Use [`cache_key_for_call`] for new authority decisions.
pub fn cache_key(tool: &str, args_json: &str) -> String {
    format!("{}:{}", tool.to_lowercase(), normalize_tool_args(args_json))
}

/// Build an authority-bound cache key for one classified call.
///
/// `None` is intentional fail-closed behavior: an approval cannot be cached or
/// reused without policy/rules revisions, workspace identity, permission
/// context, and the full classifier feature set.
pub fn cache_key_for_call(
    features: &ToolCallFeatures,
    args_json: &str,
    context: &AutoApprovalContext,
) -> Option<String> {
    if !context.is_complete() {
        return None;
    }

    let trust = match features.workspace_trust {
        crate::types::WorkspaceTrust::Untrusted => "untrusted",
        crate::types::WorkspaceTrust::Trusted => "trusted",
        crate::types::WorkspaceTrust::Sandboxed => "sandboxed",
        crate::types::WorkspaceTrust::Locked => "locked",
    };
    Some(
        serde_json::json!([
            "auto-approval-v2",
            features.tool.trim().to_ascii_lowercase(),
            normalize_tool_args(args_json),
            features.touches_outside_workspace,
            features.is_destructive,
            features.command_len,
            features.command_len.min(1000),
            features.is_test_file,
            trust,
            context.policy_revision.as_str(),
            context.rules_revision.as_str(),
            context.workspace_identity.as_str(),
            context.permission_context.as_str(),
        ])
        .to_string(),
    )
}
