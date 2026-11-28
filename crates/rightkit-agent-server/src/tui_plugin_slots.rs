//! Live plugin UI slots in the terminal UI (A201-1, contract rule R37 in
//! `docs/contracts/mcp-plugins-skills.md`).
//!
//! `status` slots are drawn as footer text in the item `plugin`, `band` slots
//! in the same item after the status items, `toast` slots as a rate-limited
//! system notice, and `pane` slots stay on demand through
//! `/plugins ui <slot-id>`. All text comes from a plugin, so it is treated as
//! untrusted: escape sequences and control characters are stripped before it
//! reaches the terminal.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// How often the slot list is re-read, so a disabled plugin's slots disappear.
pub const LIST_INTERVAL: Duration = Duration::from_secs(60);
/// How often one slot's text is refreshed from the plugin command.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(10);
/// Minimum gap between two toasts of the same slot.
pub const TOAST_MIN_GAP: Duration = Duration::from_secs(30);
/// Most slots tracked at once.
pub const MAX_SPECS: usize = 6;
/// Most items drawn in the footer.
pub const MAX_FOOTER_ITEMS: usize = 3;
/// Most characters of one item.
pub const MAX_CHARS: usize = 80;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    Status,
    Band,
    Toast,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlotSpec {
    pub kind: SlotKind,
    pub id: String,
    pub plugin: String,
}

#[derive(Default)]
pub struct PluginSlotFeed {
    specs: Vec<SlotSpec>,
    texts: HashMap<String, String>,
    last_list: Option<Instant>,
    last_run: HashMap<String, Instant>,
    last_toast: HashMap<String, Instant>,
    toasts: Vec<String>,
}

/// Parse the output of the daemon command `/plugins ui`: list lines are
/// `- {kind} {id} ({plugin}): {title}`. A `pane` line and anything else is
/// ignored.
pub fn parse_slot_list(text: &str) -> Vec<SlotSpec> {
    let mut specs = Vec::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("- ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let (Some(kind), Some(id), Some(plugin_word)) = (words.next(), words.next(), words.next())
        else {
            continue;
        };
        let kind = match kind {
            "status" => SlotKind::Status,
            "band" => SlotKind::Band,
            "toast" => SlotKind::Toast,
            _ => continue,
        };
        let Some(plugin) = plugin_word
            .strip_prefix('(')
            .and_then(|word| word.strip_suffix("):"))
        else {
            continue;
        };
        if plugin.is_empty() {
            continue;
        }
        specs.push(SlotSpec {
            kind,
            id: id.to_string(),
            plugin: plugin.to_string(),
        });
        if specs.len() >= MAX_SPECS {
            break;
        }
    }
    specs
}

/// Remove terminal escape sequences: ESC `[` parameters up to a final byte
/// 0x40-0x7e, ESC `]` up to BEL or ESC `\`, and a lone ESC with the next
/// character.
fn strip_escape_sequences(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('[') => {
                for next in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&next) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(next) = chars.next() {
                    if next == '\u{7}' {
                        break;
                    }
                    if next == '\u{1b}' {
                        // ESC `\` ends the sequence; consume the `\`.
                        chars.next();
                        break;
                    }
                }
            }
            // A lone ESC with the next character: both are dropped.
            Some(_) | None => {}
        }
    }
    out
}

/// Parse the output of `/plugins ui <slot-id>`: `{title} ({plugin}):\n{command
/// output}` for a slot with a command. Returns the first non-empty line of the
/// command output with escape sequences and control characters removed, cut to
/// [`MAX_CHARS`] characters.
pub fn parse_slot_text(output: &str) -> Option<String> {
    let (_, body) = output.split_once('\n')?;
    let stripped = strip_escape_sequences(body);
    let line = stripped.split('\n').find_map(|line| {
        let cleaned: String = line.chars().filter(|c| !c.is_control()).collect();
        let trimmed = cleaned.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })?;
    if line.chars().count() > MAX_CHARS {
        let mut cut: String = line.chars().take(MAX_CHARS - 1).collect();
        cut.push('\u{2026}');
        Some(cut)
    } else {
        Some(line)
    }
}

impl PluginSlotFeed {
    /// True when no list was read yet or [`LIST_INTERVAL`] has elapsed.
    pub fn list_due(&self, now: Instant) -> bool {
        match self.last_list {
            None => true,
            Some(last) => now.saturating_duration_since(last) >= LIST_INTERVAL,
        }
    }

    /// Replace the slot list. Stored text of every slot no longer present is
    /// dropped, so a disabled plugin's text disappears at the next list.
    pub fn set_specs(&mut self, mut specs: Vec<SlotSpec>, now: Instant) {
        specs.truncate(MAX_SPECS);
        self.texts
            .retain(|id, _| specs.iter().any(|spec| &spec.id == id));
        self.last_run
            .retain(|id, _| specs.iter().any(|spec| &spec.id == id));
        self.last_toast
            .retain(|id, _| specs.iter().any(|spec| &spec.id == id));
        self.specs = specs;
        self.last_list = Some(now);
    }

    /// The first slot whose command has not run yet or last ran
    /// [`REFRESH_INTERVAL`] ago. Records the run before returning, so a loop
    /// over this method ends.
    pub fn next_due(&mut self, now: Instant) -> Option<String> {
        let id = self
            .specs
            .iter()
            .find(|spec| match self.last_run.get(&spec.id) {
                None => true,
                Some(last) => now.saturating_duration_since(*last) >= REFRESH_INTERVAL,
            })
            .map(|spec| spec.id.clone())?;
        self.last_run.insert(id.clone(), now);
        Some(id)
    }

    /// Fold the output of one slot's command into the feed. An unknown id is
    /// ignored; no usable text removes the stored text.
    pub fn apply_output(&mut self, slot_id: &str, output: Option<&str>, now: Instant) {
        let Some(spec) = self.specs.iter().find(|spec| spec.id == slot_id).cloned() else {
            return;
        };
        let Some(text) = output.and_then(parse_slot_text) else {
            self.texts.remove(slot_id);
            return;
        };
        if spec.kind == SlotKind::Toast {
            let changed = self.texts.get(slot_id) != Some(&text);
            let gap_open = match self.last_toast.get(slot_id) {
                None => true,
                Some(last) => now.saturating_duration_since(*last) >= TOAST_MIN_GAP,
            };
            if changed && gap_open {
                self.toasts
                    .push(format!("[plugin {}] {}", spec.plugin, text));
                self.last_toast.insert(slot_id.to_string(), now);
            }
        }
        self.texts.insert(slot_id.to_string(), text);
    }

    /// Drain the toasts raised since the last call.
    pub fn take_toasts(&mut self) -> Vec<String> {
        std::mem::take(&mut self.toasts)
    }

    /// Footer text: status slots in declared order, then band slots in
    /// declared order, at most [`MAX_FOOTER_ITEMS`], joined with a middle dot.
    pub fn footer_part(&self) -> Option<String> {
        let mut items: Vec<&str> = Vec::new();
        for kind in [SlotKind::Status, SlotKind::Band] {
            for spec in self.specs.iter().filter(|spec| spec.kind == kind) {
                if let Some(text) = self.texts.get(&spec.id) {
                    items.push(text.as_str());
                }
            }
        }
        items.truncate(MAX_FOOTER_ITEMS);
        if items.is_empty() {
            None
        } else {
            Some(items.join(" \u{b7} "))
        }
    }
}
