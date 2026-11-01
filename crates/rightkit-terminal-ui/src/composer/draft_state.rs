//! Editable composer draft state kept separate from composer control flow.
//!
//! Adapted from the Codex donor `bottom_pane/chat_composer/draft_state.rs`
//! @ 444da310. Attachments and mention bindings are not carried here: the
//! composer has no attachment pipeline, and the host app owns mention policy.
//! Paste payloads, burst state, and shell (`!`) mode retain donor semantics.

use std::cell::RefCell;

use crate::composer::paste_burst::PasteBurst;
use crate::composer::text_element::TextElement;
use crate::composer::textarea::TextArea;
use crate::composer::textarea::TextAreaState;

pub(super) struct DraftState {
    pub(super) textarea: TextArea,
    pub(super) textarea_state: RefCell<TextAreaState>,
    pub(super) is_bash_mode: bool,
    pub(super) pending_pastes: Vec<(String, String)>,
    pub(super) paste_burst: PasteBurst,
    pub(super) disable_paste_burst: bool,
}

impl DraftState {
    pub(super) fn new() -> Self {
        Self {
            textarea: TextArea::new(),
            textarea_state: RefCell::new(TextAreaState::default()),
            is_bash_mode: false,
            pending_pastes: Vec::new(),
            paste_burst: PasteBurst::default(),
            disable_paste_burst: false,
        }
    }
}

/// A full snapshot of the editable draft, used for Vim undo history and
/// history-search cancellation. Mirrors the donor `ComposerDraft`, reduced to
/// the fields the composer owns (no attachments/mention bindings).
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct ComposerDraft {
    pub(crate) text: String,
    pub(crate) cursor: usize,
    pub(crate) text_elements: Vec<TextElement>,
    pub(crate) pending_pastes: Vec<(String, String)>,
}

impl ComposerDraft {
    pub(super) fn vim_history_bytes(&self) -> usize {
        let mut bytes = self.text.len()
            + self.text_elements.len()
                * std::mem::size_of::<crate::composer::text_element::TextElement>();
        for (placeholder, pasted) in &self.pending_pastes {
            bytes += placeholder.len() + pasted.len();
        }
        bytes
    }
}
