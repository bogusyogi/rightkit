//! The session transcript — the ordered list of messages the engine maintains.

use rightkit_agent_engine_protocol::{
    ContentTrust, OutputRef, ToolCall, TranscriptMessage, TrustEnvelope,
};

/// An append-only (except for compaction) record of a session's messages.
#[derive(Debug, Default, Clone)]
pub struct Transcript {
    messages: Vec<TranscriptMessage>,
    next_id: usize,
}

impl Transcript {
    pub fn new() -> Self {
        Self::default()
    }

    fn fresh_id(&mut self, prefix: &str) -> String {
        self.next_id += 1;
        format!("{prefix}-{}", self.next_id)
    }

    pub fn push_system(&mut self, content: impl Into<String>) -> &TranscriptMessage {
        let id = self.fresh_id("sys");
        self.messages.push(TranscriptMessage::System {
            id,
            created_at: "t".into(),
            content: content.into(),
            pinned: true,
        });
        self.messages.last().unwrap()
    }

    pub fn push_user(&mut self, content: impl Into<String>) -> &TranscriptMessage {
        let id = self.fresh_id("usr");
        self.messages.push(TranscriptMessage::User {
            id,
            created_at: "t".into(),
            content: content.into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        });
        self.messages.last().unwrap()
    }

    /// A harness nudge (a loop warning, an output-limit retry hint, a verification or gate
    /// instruction): a user-role message the person did not write. It is tagged by its id
    /// (`nudge-<n>`, see [`is_nudge`]) and stamped with the real time.
    pub fn push_nudge(&mut self, content: impl Into<String>) -> &TranscriptMessage {
        self.push_nudge_at(content, rightkit_agent_engine_protocol::now_iso())
    }

    /// [`Self::push_nudge`] with the time given: a nudge that is also recorded as an event
    /// (a loop warning) carries the event's own time, so a replay rebuilds it byte for byte.
    pub fn push_nudge_at(
        &mut self,
        content: impl Into<String>,
        created_at: String,
    ) -> &TranscriptMessage {
        let id = self.fresh_id("nudge");
        self.messages.push(TranscriptMessage::User {
            id,
            created_at,
            content: content.into(),
            trust: None,
            pinned: false,
            attachments: Vec::new(),
        });
        self.messages.last().unwrap()
    }

    pub fn push_pinned_user(&mut self, content: impl Into<String>) -> &TranscriptMessage {
        let id = self.fresh_id("usr");
        self.messages.push(TranscriptMessage::User {
            id,
            created_at: "t".into(),
            content: content.into(),
            trust: None,
            pinned: true,
            attachments: Vec::new(),
        });
        self.messages.last().unwrap()
    }

    pub fn push_assistant(
        &mut self,
        content: impl Into<String>,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
    ) -> &TranscriptMessage {
        let id = self.fresh_id("ast");
        self.messages.push(TranscriptMessage::Assistant {
            id,
            created_at: "t".into(),
            content: content.into(),
            reasoning_content: reasoning,
            tool_calls,
            pinned: false,
            origin: None,
        });
        self.messages.last().unwrap()
    }

    /// Stamp the newest message, when it is an assistant message without an origin, with the
    /// provider and model that produced it.
    pub fn tag_last_assistant_origin(
        &mut self,
        origin: Option<rightkit_agent_engine_protocol::MessageOrigin>,
    ) {
        let Some(origin) = origin else { return };
        if let Some(TranscriptMessage::Assistant {
            origin: slot @ None,
            ..
        }) = self.messages.last_mut()
        {
            *slot = Some(origin);
        }
    }

    pub fn push_tool_result(
        &mut self,
        tool_call_id: impl Into<String>,
        summary: impl Into<String>,
        is_error: bool,
    ) -> &TranscriptMessage {
        self.push_tool_result_with_output(tool_call_id, summary, None, is_error)
    }

    pub fn push_tool_result_with_output(
        &mut self,
        tool_call_id: impl Into<String>,
        summary: impl Into<String>,
        output: Option<OutputRef>,
        is_error: bool,
    ) -> &TranscriptMessage {
        let id = self.fresh_id("tool");
        let summary = summary.into();
        let tool_call_id = tool_call_id.into();
        // The envelope is keyed by the call's tool name, exactly as the durable `ToolResultAdded`
        // event records it, so a resumed session sends the provider the same bytes the live one
        // did (the call id here made every tool result differ after a resume: a full cache miss).
        // An id with no matching call keeps the id.
        let tool_name = self
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                TranscriptMessage::Assistant { tool_calls, .. } => tool_calls
                    .iter()
                    .find(|call| call.id == tool_call_id)
                    .map(|call| call.name.clone()),
                _ => None,
            });
        let trust = tool_result_trust(tool_name.as_deref().unwrap_or(&tool_call_id), &summary);
        self.messages.push(TranscriptMessage::Tool {
            id,
            created_at: "t".into(),
            tool_call_id,
            summary,
            output,
            trust: Some(trust),
            is_error,
            pinned: false,
            process_id: None,
        });
        self.messages.last().unwrap()
    }

    pub fn messages(&self) -> &[TranscriptMessage] {
        &self.messages
    }

    /// Update an existing streaming assistant without copying prior history.
    pub(crate) fn assistant_mut(&mut self, assistant_id: &str) -> Option<&mut TranscriptMessage> {
        self.messages.iter_mut().rev().find(|message| {
            matches!(message, TranscriptMessage::Assistant { id, .. } if id == assistant_id)
        })
    }

    pub fn len(&self) -> usize {
        self.messages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Replace the messages with a compacted set (used by the compactor).
    pub fn replace(&mut self, messages: Vec<TranscriptMessage>) {
        self.messages = messages;
    }

    /// Remove everything from the last user message onward.
    /// Returns the text of the removed user message, or None if there was none.
    pub fn trim_before_last_user(&mut self) -> Option<String> {
        let pos = self
            .messages
            .iter()
            .rposition(|m| matches!(m, TranscriptMessage::User { .. }))?;
        let text = match &self.messages[pos] {
            TranscriptMessage::User { content, .. } => content.clone(),
            _ => return None,
        };
        self.messages.truncate(pos);
        Some(text)
    }

    /// Remove the matching user message and everything after it (rewind to an
    /// earlier prompt). `matches` is tested against each user message's raw
    /// content; `nth_from_end` skips that many matches counting from the end
    /// (0 = the latest match), so repeated identical prompts stay addressable.
    /// Returns the removed message's raw content, or `None` when no such
    /// message is in context (it may have been compacted away).
    pub fn trim_before_user_matching(
        &mut self,
        matches: impl Fn(&str) -> bool,
        nth_from_end: usize,
    ) -> Option<String> {
        let tail = self.take_tail_from_user_matching(matches, nth_from_end)?;
        match tail.first() {
            Some(TranscriptMessage::User { content, .. }) => Some(content.clone()),
            _ => None,
        }
    }

    /// Like [`Self::trim_before_user_matching`], but hands back the removed
    /// messages (the matched prompt and everything after it) so a redo can put
    /// them back with [`Self::extend_messages`].
    pub fn take_tail_from_user_matching(
        &mut self,
        matches: impl Fn(&str) -> bool,
        nth_from_end: usize,
    ) -> Option<Vec<TranscriptMessage>> {
        let pos = self
            .messages
            .iter()
            .enumerate()
            .rev()
            .filter(
                |(_, m)| matches!(m, TranscriptMessage::User { content, .. } if matches(content)),
            )
            .nth(nth_from_end)
            .map(|(pos, _)| pos)?;
        Some(self.messages.split_off(pos))
    }

    /// Drop a trailing assistant appended after `since` that carries no
    /// visible text and no tool call (a reasoning-only reply), so a retry
    /// re-sends the same history as the failed attempt.
    pub fn pop_reasoning_only_assistant(&mut self, since: usize) {
        let reasoning_only = self.messages.len() > since
            && matches!(
                self.messages.last(),
                Some(TranscriptMessage::Assistant { content, tool_calls, .. })
                    if content.trim().is_empty() && tool_calls.is_empty()
            );
        if reasoning_only {
            self.messages.pop();
        }
    }

    /// Append messages (redo of a rewind).
    pub fn extend_messages(&mut self, messages: Vec<TranscriptMessage>) {
        self.messages.extend(messages);
    }
}

/// The untrusted-content envelope for a tool result keyed by its tool name. Every result is
/// untrusted data, except (with the host option `todo_write_result_trusted` on) a `todo_write`
/// result, which answers with the model's own list and fixed status text, nothing read from the
/// workspace or a person. File tools stay wrapped: their results carry diffs and nearby file text.
pub fn tool_result_trust(source: &str, content: &str) -> TrustEnvelope {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(content.as_bytes());
    let engine_authored =
        source == "todo_write" && crate::host_options::todo_write_result_trusted();
    TrustEnvelope {
        trust: if engine_authored {
            ContentTrust::ToolTrusted
        } else {
            ContentTrust::ToolUntrusted
        },
        source: format!("tool:{source}"),
        instructions_are_data: !engine_authored,
        content_hash: format!("sha256:{}", hasher.finalize()),
    }
}

/// The `fields` key of a loop observation (`loop_warning`, or the meltdown's `mop_context_reset` /
/// `mop_strategy_reset`) that holds the exact nudge text the model was sent. Session replay
/// (`rehydrate_engine_context`) puts that nudge back where the observation stands, so a resumed
/// process sends the history the live one did.
pub const LOOP_WARNING_NUDGE_FIELD: &str = "nudge";

/// The nudge a stored loop observation stands for, rebuilt as the live transcript held
/// it (`nudge-<seq>`, the observation's time, the recorded text). `None` for any other
/// observation and for one recorded before the text was stored.
pub fn loop_warning_nudge(
    observation: &rightkit_agent_engine_protocol::OrchestrationObservation,
    seq: i64,
) -> Option<TranscriptMessage> {
    if !matches!(
        observation.kind.as_str(),
        "loop_warning" | "mop_context_reset" | "mop_strategy_reset"
    ) {
        return None;
    }
    let content = observation.fields.get(LOOP_WARNING_NUDGE_FIELD)?.clone();
    Some(TranscriptMessage::User {
        id: format!("nudge-{seq}"),
        created_at: observation.created_at.clone(),
        content,
        trust: None,
        pinned: false,
        attachments: Vec::new(),
    })
}

/// True for a message pushed by [`Transcript::push_nudge`]: the harness said it, not the person.
pub fn is_nudge(message: &TranscriptMessage) -> bool {
    matches!(message, TranscriptMessage::User { id, .. } if id.starts_with("nudge-"))
}
