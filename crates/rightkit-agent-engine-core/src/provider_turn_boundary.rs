use rightkit_agent_engine_protocol::TranscriptMessage;
use rightkit_agent_coding_tools::ToolRegistry;
use rightkit_mcp::agent_mcp::McpManager;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct BoundaryTextUpdate {
    pub revision: u64,
    pub content: String,
    pub fingerprint: String,
}

impl BoundaryTextUpdate {
    pub fn new(revision: u64, content: impl Into<String>) -> Self {
        let content = content.into();
        let fingerprint = sha256_hex(content.as_bytes());
        Self {
            revision,
            content,
            fingerprint,
        }
    }
}

/// Provider-visible host state observed asynchronously and promoted only at a
/// provider-turn boundary. The payloads are already-rendered projections; this
/// type does not select, rank, or reinterpret semantic context.
#[derive(Clone)]
pub enum ProviderBoundaryUpdate {
    Prompt(BoundaryTextUpdate),
    Context(BoundaryTextUpdate),
    WorldState(BoundaryTextUpdate),
    ToolCatalog {
        revision: u64,
        tools: Arc<ToolRegistry>,
    },
    /// Atomically replace the MCP manager and the visible session catalog.
    /// Both become provider-visible at one request boundary.
    McpManager {
        revision: u64,
        manager: Arc<McpManager>,
        tools: Arc<ToolRegistry>,
    },
}

impl ProviderBoundaryUpdate {
    fn rank(&self) -> u8 {
        match self {
            Self::Prompt(_) => 0,
            Self::Context(_) => 1,
            Self::WorldState(_) => 2,
            // A manager replacement carries its matching visible catalog.
            // Promote an ordinary catalog first so a same-boundary MCP
            // replacement is the final atomic manager/catalog pair.
            Self::ToolCatalog { .. } => 3,
            Self::McpManager { .. } => 4,
        }
    }

    fn revision(&self) -> u64 {
        match self {
            Self::Prompt(update) | Self::Context(update) | Self::WorldState(update) => {
                update.revision
            }
            Self::ToolCatalog { revision, .. } => *revision,
            Self::McpManager { revision, .. } => *revision,
        }
    }
}

/// Outcome of offering a provider-visible host-state update to the boundary.
///
/// `Queued` and `Coalesced` are both durable acceptances: the newest revision
/// for the category remains representable and will be promoted at the next
/// provider-turn boundary. Only `Closed` is a real loss, and it is never
/// silent - the caller must log it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundarySendOutcome {
    /// Delivered into the bounded queue.
    Queued,
    /// Queue was full; folded into the latest-state cell for its category,
    /// superseding any older pending revision. Nothing is lost.
    Coalesced,
    /// Receiver is gone. The update cannot become provider-visible.
    Closed,
}

type CoalescedLatest = Arc<Mutex<BTreeMap<u8, ProviderBoundaryUpdate>>>;

/// Producer half of the provider-boundary channel.
///
/// Never blocks and never silently drops an accepted update. Because every
/// payload is a full-state projection carrying a monotonic revision, a full
/// queue degrades to latest-state coalescing rather than to loss. It never
/// awaits, so a producer can never deadlock behind an in-flight model stream.
#[derive(Clone)]
pub struct ProviderBoundarySender {
    tx: tokio::sync::mpsc::Sender<ProviderBoundaryUpdate>,
    coalesced: CoalescedLatest,
}

impl ProviderBoundarySender {
    pub fn send_latest(&self, update: ProviderBoundaryUpdate) -> BoundarySendOutcome {
        match self.tx.try_send(update) {
            Ok(()) => BoundarySendOutcome::Queued,
            Err(tokio::sync::mpsc::error::TrySendError::Full(update)) => {
                self.fold_latest(update);
                BoundarySendOutcome::Coalesced
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => BoundarySendOutcome::Closed,
        }
    }

    fn fold_latest(&self, update: ProviderBoundaryUpdate) {
        let mut latest = match self.coalesced.lock() {
            Ok(latest) => latest,
            Err(poisoned) => poisoned.into_inner(),
        };
        let key = update.rank();
        let supersedes = latest
            .get(&key)
            .is_none_or(|pending| update.revision() >= pending.revision());
        if supersedes {
            latest.insert(key, update);
        }
    }
}

/// Consumer half of the provider-boundary channel.
pub struct ProviderBoundaryReceiver {
    rx: tokio::sync::mpsc::Receiver<ProviderBoundaryUpdate>,
    coalesced: CoalescedLatest,
}

impl ProviderBoundaryReceiver {
    fn drain(&mut self) -> Vec<ProviderBoundaryUpdate> {
        let mut pending = std::iter::from_fn(|| self.rx.try_recv().ok()).collect::<Vec<_>>();
        let mut latest = match self.coalesced.lock() {
            Ok(latest) => latest,
            Err(poisoned) => poisoned.into_inner(),
        };
        pending.extend(std::mem::take(&mut *latest).into_values());
        pending
    }
}

/// Build a bounded provider-boundary channel with latest-state overflow.
pub fn provider_boundary_channel(
    capacity: usize,
) -> (ProviderBoundarySender, ProviderBoundaryReceiver) {
    let (tx, rx) = tokio::sync::mpsc::channel(capacity);
    let coalesced: CoalescedLatest = Arc::new(Mutex::new(BTreeMap::new()));
    (
        ProviderBoundarySender {
            tx,
            coalesced: coalesced.clone(),
        },
        ProviderBoundaryReceiver { rx, coalesced },
    )
}

#[derive(Clone, Default)]
struct ProjectedText {
    revision: u64,
    content: String,
    fingerprint: String,
}

#[derive(Default)]
pub struct ProviderBoundaryProjection {
    generation: u64,
    // Host state is revisioned input. Keep every published revision so an
    // update can be appended at the current request tail without rewriting
    // bytes that a provider has already cached.
    prompt: Vec<ProjectedText>,
    context: Vec<ProjectedText>,
    world_state: Vec<ProjectedText>,
    /// Last raw (producer-accumulated) world-state text, used to send only the
    /// newest refresh instead of every earlier refresh again (audit C7).
    world_state_raw: String,
    tool_catalog_revision: u64,
    mcp_manager_revision: u64,
}

#[derive(Default)]
pub struct BoundaryPromotion {
    pub accepted: usize,
    pub tools: Option<Arc<ToolRegistry>>,
    pub mcp: Option<(Arc<McpManager>, Arc<ToolRegistry>)>,
}

impl ProviderBoundaryProjection {
    /// Producers append each environment refresh to one accumulating string.
    /// Every refresh is a complete snapshot, so only the tail after the
    /// previously published text is new; re-sending the whole accumulation
    /// grows quadratically and defeats prefix caching.
    fn apply_world_state(&mut self, update: BoundaryTextUpdate) -> bool {
        if self
            .world_state
            .last()
            .is_some_and(|current| current.revision >= update.revision)
        {
            return false;
        }
        let raw = update.content.clone();
        let tail = if self.world_state_raw.is_empty() {
            None
        } else {
            raw.strip_prefix(self.world_state_raw.as_str())
                .map(|rest| rest.trim_start_matches('\n'))
                .filter(|rest| !rest.is_empty())
        };
        let update = match tail {
            Some(tail) => BoundaryTextUpdate::new(update.revision, tail),
            None => update,
        };
        self.world_state_raw = raw;
        apply_text(&mut self.world_state, update)
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn tool_catalog_revision(&self) -> u64 {
        self.tool_catalog_revision
    }

    pub fn mcp_manager_revision(&self) -> u64 {
        self.mcp_manager_revision
    }

    pub fn context_fingerprint(&self) -> Option<String> {
        let mut entries = Vec::new();
        for (kind, values) in [
            ("prompt", self.prompt.as_slice()),
            ("context", self.context.as_slice()),
            ("world_state", self.world_state.as_slice()),
        ] {
            for value in values {
                entries.push(format!("{kind}:{}:{}", value.revision, value.fingerprint));
            }
        }
        (!entries.is_empty()).then(|| sha256_hex(entries.join("\n").as_bytes()))
    }

    pub fn promote_pending(&mut self, rx: &mut ProviderBoundaryReceiver) -> BoundaryPromotion {
        let mut pending = rx.drain();
        pending.sort_by_key(|update| (update.rank(), update.revision()));
        let mut promoted = BoundaryPromotion::default();
        for update in pending {
            let accepted = match update {
                ProviderBoundaryUpdate::Prompt(update) => apply_text(&mut self.prompt, update),
                ProviderBoundaryUpdate::Context(update) => apply_text(&mut self.context, update),
                ProviderBoundaryUpdate::WorldState(update) => self.apply_world_state(update),
                ProviderBoundaryUpdate::ToolCatalog { revision, tools }
                    if revision > self.tool_catalog_revision =>
                {
                    self.tool_catalog_revision = revision;
                    promoted.tools = Some(tools);
                    true
                }
                ProviderBoundaryUpdate::ToolCatalog { .. } => false,
                ProviderBoundaryUpdate::McpManager {
                    revision,
                    manager,
                    tools,
                } if revision > self.mcp_manager_revision => {
                    self.mcp_manager_revision = revision;
                    // The manager update owns this exact catalog snapshot.
                    // Do not let an earlier ToolCatalog promotion be applied
                    // again by the engine as a mismatched pair.
                    promoted.tools = None;
                    promoted.mcp = Some((manager, tools));
                    true
                }
                ProviderBoundaryUpdate::McpManager { .. } => false,
            };
            if accepted {
                promoted.accepted += 1;
            }
        }
        if promoted.accepted > 0 {
            self.generation = self.generation.saturating_add(1);
        }
        promoted
    }

    /// Append host-state revisions after history. Each revision explicitly
    /// supersedes its prior category state while old submitted bytes remain
    /// in place. Tool-catalog promotion occurs before this projection.
    pub fn inject_into(&self, messages: &mut Vec<TranscriptMessage>) {
        self.inject_into_budgeted(messages, None);
    }

    /// Like `inject_into`, but caps the injected host state to
    /// `remaining_tokens` (`None` = unbounded). Items are selected
    /// highest-priority first (prompt, world state, context; newest revision
    /// first within a kind); dropped items are announced by a visible marker
    /// when the marker itself fits. Selected items keep their original order.
    /// Returns the number of dropped items.
    pub fn inject_into_budgeted(
        &self,
        messages: &mut Vec<TranscriptMessage>,
        remaining_tokens: Option<u64>,
    ) -> usize {
        let existing = messages
            .iter()
            .filter_map(|message| match message {
                TranscriptMessage::System { content, .. } => Some(content.as_str()),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>();
        let mut projected = Vec::new();
        for (id, values) in [
            ("provider-boundary-prompt", self.prompt.as_slice()),
            ("provider-boundary-context", self.context.as_slice()),
            ("provider-boundary-world-state", self.world_state.as_slice()),
        ] {
            for value in values {
                let content = format!("[Host state update {id} revision {} supersedes earlier {id} revisions; apply this revision from here forward.]\n{}", value.revision, value.content);
                if existing.contains(content.as_str()) {
                    continue;
                }
                projected.push(TranscriptMessage::System {
                    id: id.into(),
                    created_at: format!("boundary-revision-{}", value.revision),
                    content,
                    pinned: false,
                });
            }
        }
        let Some(budget) = remaining_tokens else {
            messages.extend(projected);
            return 0;
        };
        // Priority rank: lower is more important.
        let rank = |id: &str| match id {
            "provider-boundary-prompt" => 0u8,
            "provider-boundary-world-state" => 1,
            _ => 2,
        };
        let mut order: Vec<usize> = (0..projected.len()).collect();
        order.sort_by_key(|&i| {
            let id = match &projected[i] {
                TranscriptMessage::System { id, .. } => id.as_str(),
                _ => "",
            };
            (rank(id), std::cmp::Reverse(i))
        });
        let mut left = budget;
        let mut keep = vec![false; projected.len()];
        for i in order {
            if let TranscriptMessage::System { content, .. } = &projected[i] {
                let cost = estimate_boundary_tokens(content);
                if cost <= left {
                    left -= cost;
                    keep[i] = true;
                }
            }
        }
        let dropped = keep.iter().filter(|kept| !**kept).count();
        let mut selected: Vec<TranscriptMessage> = projected
            .into_iter()
            .zip(keep)
            .filter_map(|(message, kept)| kept.then_some(message))
            .collect();
        if dropped > 0 {
            tracing::warn!(
                dropped,
                budget,
                "host state injection trimmed to request budget"
            );
            let marker = format!(
                "[Host state budget: {dropped} lower-priority host state update(s) omitted to fit the request budget.]"
            );
            if estimate_boundary_tokens(&marker) <= left {
                selected.push(TranscriptMessage::System {
                    id: "provider-boundary-dropped".into(),
                    created_at: "boundary-dropped".into(),
                    content: marker,
                    pinned: false,
                });
            }
        }
        messages.extend(selected);
        dropped
    }

    /// Commit newly promoted revisions to canonical session history at the
    /// promotion boundary. Later requests then retain every old update at its
    /// original position while ordinary messages grow after it.
    pub fn append_promoted_to_transcript(&self, transcript: &mut crate::transcript::Transcript) {
        let mut messages = transcript.messages().to_vec();
        self.inject_into(&mut messages);
        for message in messages.into_iter().skip(transcript.messages().len()) {
            if let TranscriptMessage::System { content, .. } = message {
                transcript.push_system(content);
            }
        }
    }
}

/// Conservative chars/4 ceiling estimate for injected host-state text.
pub(crate) fn estimate_boundary_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4).max(1)
}

/// Remaining request budget = window - used - reserved output/safety margin.
pub(crate) fn remaining_injection_budget(window: u64, used: u64, reserve: u64) -> u64 {
    window.saturating_sub(used).saturating_sub(reserve)
}

fn apply_text(history: &mut Vec<ProjectedText>, update: BoundaryTextUpdate) -> bool {
    if history
        .last()
        .is_some_and(|current| current.revision >= update.revision)
    {
        return false;
    }
    history.push(ProjectedText {
        revision: update.revision,
        content: update.content,
        fingerprint: update.fingerprint,
    });
    true
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(bytes);
    format!("sha256:{}", hasher.finalize())
}
