//! Acting on observed refs: one action, then the diff it caused; and the action-loop watchdog.
//!
//! The ref check itself (epoch, replaced frame, tag, role and name must still match) lives in
//! `page.rs` beside `observe`. This module holds the types of [`BrowserPage::act`](crate::BrowserPage::act)
//! and the repeat counter that stops a run which keeps doing the same thing to the same element with no
//! visible effect.

use crate::error::{BrowserError, Result};
use crate::page::ObservedNode;
use crate::policy::ActionKind;
use serde::Serialize;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

/// How many times the same action may run on the same ref with no diff before the next one is refused.
pub const DEFAULT_ACTION_LOOP_LIMIT: u32 = 5;

/// The one action [`BrowserPage::act`](crate::BrowserPage::act) runs on a ref.
#[derive(Clone, Debug, PartialEq)]
pub enum RefAction {
    Click,
    DoubleClick,
    Hover,
    /// Click to focus, optionally select-all and delete, then type.
    Fill { text: String, clear: bool },
    /// Choose a `<select>` option by value or label.
    SelectOption { value: String },
}

impl RefAction {
    /// The admission kind and detail for this action. A fill reports its length, never its text.
    pub(crate) fn admission(&self) -> (ActionKind, Option<String>) {
        match self {
            RefAction::Click => (ActionKind::Click, None),
            RefAction::DoubleClick => (ActionKind::DoubleClick, None),
            RefAction::Hover => (ActionKind::Hover, None),
            RefAction::Fill { text, clear } => (
                ActionKind::Fill,
                Some(format!("chars={} clear={clear}", text.chars().count())),
            ),
            RefAction::SelectOption { value } => {
                (ActionKind::Select, Some(format!("value={value}")))
            }
        }
    }

    /// What the watchdog compares: the action, its ref and a hash of any text (the text itself is not kept).
    pub(crate) fn watch_key(&self, node_ref: &str) -> String {
        let what = match self {
            RefAction::Click => "click".to_string(),
            RefAction::DoubleClick => "double_click".to_string(),
            RefAction::Hover => "hover".to_string(),
            RefAction::Fill { text, clear } => format!("fill:{clear}:{}", hash_of(text)),
            RefAction::SelectOption { value } => format!("select:{}", hash_of(value)),
        };
        format!("{node_ref}|{what}")
    }
}

fn hash_of(text: &str) -> u64 {
    let mut h = DefaultHasher::new();
    text.hash(&mut h);
    h.finish()
}

/// What an observed element looked like. Names of password-like fields are already masked.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct NodeState {
    pub role: String,
    pub name: String,
    pub visible: bool,
    pub disabled: bool,
}

impl NodeState {
    pub(crate) fn of(node: &ObservedNode) -> NodeState {
        NodeState {
            role: node.role.clone(),
            name: node.name.clone(),
            visible: node.visible,
            disabled: node.disabled,
        }
    }
}

/// An element that was there before the action and is still there, but looks different.
#[derive(Clone, Debug, Serialize)]
pub struct NodeChange {
    /// The element's ref in the new epoch.
    pub node_ref: String,
    pub selector: String,
    pub before: NodeState,
    pub after: NodeState,
}

/// What one action changed. Elements are matched by selector; an element with no selector (it cannot be
/// targeted by ref) is not part of the diff.
#[derive(Clone, Debug, Serialize)]
pub struct ObservationDiff {
    pub epoch_before: u64,
    pub epoch_after: u64,
    pub url_before: String,
    pub url_after: String,
    /// The main document was replaced (a navigation or reload). Every earlier ref is refused.
    pub document_replaced: bool,
    /// Elements that were not there before, with their refs in `epoch_after`.
    pub added: Vec<ObservedNode>,
    /// Selectors of elements that are gone.
    pub removed: Vec<String>,
    pub changed: Vec<NodeChange>,
}

impl ObservationDiff {
    /// Nothing observable happened. This is what the watchdog counts.
    pub fn is_empty(&self) -> bool {
        !self.document_replaced
            && self.url_before == self.url_after
            && self.added.is_empty()
            && self.removed.is_empty()
            && self.changed.is_empty()
    }
}

/// The result of one [`BrowserPage::act`](crate::BrowserPage::act).
#[derive(Clone, Debug, Serialize)]
pub struct ActResult {
    pub page_id: String,
    /// The ref the action ran on.
    pub node_ref: String,
    /// The page's epoch after the action. When it differs from `diff.epoch_before`, every earlier ref is
    /// refused: use the refs in `diff`, or call `observe` for the whole tree.
    pub epoch: u64,
    pub diff: ObservationDiff,
    /// How many times in a row this same action has now run on this same ref with no diff (0 after a diff).
    pub repeats: u32,
}

/// Repeat counter behind the action-loop watchdog. Pure state: no page access, no clock.
#[derive(Debug)]
pub(crate) struct ActionWatch {
    limit: u32,
    last: Option<String>,
    repeats: u32,
    stopped: bool,
}

impl Default for ActionWatch {
    fn default() -> Self {
        ActionWatch {
            limit: DEFAULT_ACTION_LOOP_LIMIT,
            last: None,
            repeats: 0,
            stopped: false,
        }
    }
}

impl ActionWatch {
    /// `limit` must be at least 1.
    pub(crate) fn set_limit(&mut self, limit: u32) -> Result<()> {
        if limit == 0 {
            return Err(BrowserError::Invalid(
                "the action-loop limit must be at least 1".into(),
            ));
        }
        self.limit = limit;
        Ok(())
    }

    /// Clear the count and the stop latch (a person or the host decided the run may go on).
    pub(crate) fn reset(&mut self) {
        self.last = None;
        self.repeats = 0;
        self.stopped = false;
    }

    /// Called before an action runs. Refuses it when the run is stopped, or when it would be the
    /// `limit + 1`th identical no-diff action; the second case stops the run until `reset`.
    pub(crate) fn before(&mut self, key: &str) -> Result<()> {
        if self.stopped {
            return Err(BrowserError::ActionLoop(format!(
                "the run was stopped after {} identical actions with no effect",
                self.limit
            )));
        }
        if self.last.as_deref() == Some(key) && self.repeats >= self.limit {
            self.stopped = true;
            return Err(BrowserError::ActionLoop(format!(
                "the same action on the same element ran {} times with no effect",
                self.repeats
            )));
        }
        Ok(())
    }

    /// Called after an action ran. `no_effect` is true for an empty diff or a failed action. Returns the
    /// consecutive repeat count.
    pub(crate) fn after(&mut self, key: String, no_effect: bool) -> u32 {
        if !no_effect {
            self.last = None;
            self.repeats = 0;
        } else if self.last.as_deref() == Some(key.as_str()) {
            self.repeats = self.repeats.saturating_add(1);
        } else {
            self.last = Some(key);
            self.repeats = 1;
        }
        self.repeats
    }
}
