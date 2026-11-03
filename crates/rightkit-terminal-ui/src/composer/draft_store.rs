//! S:I7 (draft-preservation half) — per-view composer draft preservation.
//!
//! The A6 wiring note treats this as the same gap CG-A05 flagged: switching
//! views/panels and losing unsent composer text. `Composer` itself only
//! ever holds one buffer for whatever view is currently focused; this
//! module is the missing per-view stash so switching away and back restores
//! exactly what was being typed, keyed by a view id the host already has
//! (session id, panel name -- whatever `tui_host.rs` uses to identify a
//! view).
//!
//! Deliberately separate from `super::buffer::Buffer`: the store holds
//! *expanded* text (after paste markers are resolved), not a live buffer
//! with its own paste registry/undo history, because a saved draft is
//! inert until it is restored into a fresh `Composer`, and restoring a
//! marker-collapsed string without its originating `PasteRegistry` would
//! be meaningless.

use std::collections::HashMap;

/// Per-view unsent composer text, keyed by an opaque view id owned by the
/// host.
#[derive(Debug, Default)]
pub struct DraftStore {
    drafts: HashMap<String, String>,
}

impl DraftStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Save `text` as the draft for `view`. An empty draft removes any
    /// stored entry for that view instead of storing an empty string --
    /// saving nothing should not linger as a "restore to empty" trap later.
    pub fn save(&mut self, view: impl Into<String>, text: impl Into<String>) {
        let text = text.into();
        let view = view.into();
        if text.is_empty() {
            self.drafts.remove(&view);
        } else {
            self.drafts.insert(view, text);
        }
    }

    /// Take (remove and return) the draft for `view`, if any. Taking rather
    /// than peeking: once a draft is restored into the composer, the store
    /// should not also keep serving it stale on a later switch before the
    /// composer saves again.
    pub fn take(&mut self, view: &str) -> Option<String> {
        self.drafts.remove(view)
    }

    /// Peek without removing, for a host that wants to show "draft pending"
    /// affordance in a view switcher without consuming it yet.
    pub fn peek(&self, view: &str) -> Option<&str> {
        self.drafts.get(view).map(String::as_str)
    }

    pub fn has_draft(&self, view: &str) -> bool {
        self.drafts.contains_key(view)
    }

    pub fn clear(&mut self, view: &str) {
        self.drafts.remove(view);
    }

    pub fn len(&self) -> usize {
        self.drafts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.drafts.is_empty()
    }
}
