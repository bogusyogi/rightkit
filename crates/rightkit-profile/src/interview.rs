//! The interview: which questions are due. Each area carries its questions as templates; an entry that is
//! missing is an unanswered question and a stale entry is a question again. The wording is the host's copy,
//! and the host decides how many to ask at once (the plan says "a few"; the number is an open point).

use crate::area::Area;
use crate::entry::{entry_id, State};
use crate::profile::ProfileState;

/// One question an area asks, filed under the key its answer will have.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuestionTemplate {
    pub area: Area,
    pub key: String,
    pub prompt: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// No entry exists for the key.
    Unanswered,
    /// The entry is past its review date or a newer suggestion disagrees with it.
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Question {
    pub area: Area,
    pub key: String,
    /// The template's wording; `None` for a stale entry no template covers, which the host words itself.
    pub prompt: Option<String>,
    pub reason: Reason,
    /// How many suggestions wait for this entry. Their values are read with
    /// [`ProfileState::proposals_for`]; a suggestion is never an answer.
    pub pending_proposals: usize,
}

impl ProfileState {
    /// The questions due, stale ones first, then unanswered ones in template order, at most `limit`.
    pub fn questions(&self, templates: &[QuestionTemplate], limit: usize) -> Vec<Question> {
        let mut due = Vec::new();
        for entry in self.entries.values().filter(|e| e.state == State::Stale) {
            let prompt = templates
                .iter()
                .find(|t| t.area == entry.area && t.key == entry.key)
                .map(|t| t.prompt.clone());
            due.push(Question {
                area: entry.area,
                key: entry.key.clone(),
                prompt,
                reason: Reason::Stale,
                pending_proposals: self.proposals_for(entry.area, &entry.key).len(),
            });
        }
        for template in templates {
            if self
                .entries
                .contains_key(&entry_id(template.area, &template.key))
            {
                continue;
            }
            let already = due
                .iter()
                .any(|q: &Question| q.area == template.area && q.key == template.key);
            if already {
                continue;
            }
            due.push(Question {
                area: template.area,
                key: template.key.clone(),
                prompt: Some(template.prompt.clone()),
                reason: Reason::Unanswered,
                pending_proposals: self.proposals_for(template.area, &template.key).len(),
            });
        }
        due.truncate(limit);
        due
    }
}
