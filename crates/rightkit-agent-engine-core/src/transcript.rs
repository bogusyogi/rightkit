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
        });
        self.messages.last().unwrap()
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
        let trust = tool_result_trust(&tool_call_id, &summary);
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

    /// Append messages (redo of a rewind).
    pub fn extend_messages(&mut self, messages: Vec<TranscriptMessage>) {
        self.messages.extend(messages);
    }
}

pub fn tool_result_trust(source: &str, content: &str) -> TrustEnvelope {
    let mut hasher = rightkit_fs::digest::Sha256Hasher::new();
    hasher.update(content.as_bytes());
    TrustEnvelope {
        trust: ContentTrust::ToolUntrusted,
        source: format!("tool:{source}"),
        instructions_are_data: true,
        content_hash: format!("sha256:{}", hasher.finalize()),
    }
}
