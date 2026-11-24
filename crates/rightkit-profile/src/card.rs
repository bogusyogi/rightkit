//! The card: the confirmed entries a run receives. Only confirmed entries are read; a proposal or a stale
//! entry is never shown to a model. Health and money entries are left out unless their switch is on, and
//! the area's own sensitivity floor applies whatever label an entry carries.

use crate::area::Area;
use crate::entry::{Sensitivity, State};
use crate::profile::ProfileState;

/// The two switches the plan names (ME-3 item 5). Both default off; the host reads them from its settings.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Switches {
    pub health: bool,
    pub money: bool,
}

impl Switches {
    pub fn allows(self, sensitivity: Sensitivity) -> bool {
        match sensitivity {
            Sensitivity::Ordinary => true,
            Sensitivity::Health => self.health,
            Sensitivity::Money => self.money,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardEntry {
    pub area: Area,
    pub key: String,
    pub value: String,
    pub true_from: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Card {
    pub entries: Vec<CardEntry>,
}

impl Card {
    /// One line per entry, `<area>/<key>: <value>`, a newline in a value shown as a space.
    pub fn to_text(&self) -> String {
        let mut out = String::new();
        for entry in &self.entries {
            out.push_str(entry.area.slug());
            out.push('/');
            out.push_str(&entry.key);
            out.push_str(": ");
            out.push_str(&entry.value.replace('\n', " "));
            out.push('\n');
        }
        out
    }
}

impl ProfileState {
    /// The confirmed entries of `areas` that `switches` allow, ordered by area then key. A run's areas come
    /// from the host's table of task kinds; [`Area::CARD`] is the identity card.
    pub fn card(&self, areas: &[Area], switches: Switches) -> Card {
        let mut entries: Vec<CardEntry> = self
            .entries
            .values()
            .filter(|e| {
                e.state == State::Confirmed
                    && areas.contains(&e.area)
                    && switches.allows(e.effective_sensitivity())
            })
            .map(|e| CardEntry {
                area: e.area,
                key: e.key.clone(),
                value: e.value.clone(),
                true_from: e.true_from.clone(),
            })
            .collect();
        entries.sort_by(|a, b| a.area.cmp(&b.area).then_with(|| a.key.cmp(&b.key)));
        Card { entries }
    }
}
