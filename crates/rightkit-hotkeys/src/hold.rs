//! Hold tracking: pairs every `Pressed` with exactly one `Released` for hotkey APIs that only
//! deliver a press (Windows `RegisterHotKey` posts `WM_HOTKEY` on press and never on release).
//!
//! The tracker is pure and platform independent. The Windows `GlobalHotkeys` drives it from a short
//! thread timer that exists only while at least one chord is held; the OS key state is read through
//! the [`KeyStateSource`] seam so tests inject a fake.

/// Press or release of a registered chord.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HotkeyState {
    Pressed,
    Released,
}

/// Source of physical key state. On Windows this is `GetAsyncKeyState`; tests use a fake.
pub trait KeyStateSource {
    /// `true` while the key with this Windows virtual-key code is physically down.
    fn is_down(&self, vk: u16) -> bool;
}

/// What must stay down for a chord to count as held: the non-modifier key, and for each required
/// modifier at least one of its virtual keys (for example Ctrl is `VK_CONTROL`, Win is either
/// `VK_LWIN` or `VK_RWIN`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoldKeys {
    pub key: u16,
    pub modifier_groups: Vec<Vec<u16>>,
}

impl HoldKeys {
    fn held(&self, src: &dyn KeyStateSource) -> bool {
        src.is_down(self.key)
            && self
                .modifier_groups
                .iter()
                .all(|group| group.iter().any(|vk| src.is_down(*vk)))
    }
}

#[derive(Debug)]
struct Held {
    id: i32,
    keys: HoldKeys,
}

/// Tracks which hotkey ids are currently held.
#[derive(Debug, Default)]
pub struct HoldTracker {
    held: Vec<Held>,
}

impl HoldTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a press. Returns `true` when this is a new hold (the caller delivers `Pressed`);
    /// `false` when `id` is already held, so a repeated press never yields a second `Pressed`.
    pub fn press(&mut self, id: i32, keys: HoldKeys) -> bool {
        if self.held.iter().any(|h| h.id == id) {
            return false;
        }
        self.held.push(Held { id, keys });
        true
    }

    /// Poll the key state. Returns the ids whose chord is no longer held (each exactly once; they
    /// are removed from the tracker).
    pub fn poll(&mut self, src: &dyn KeyStateSource) -> Vec<i32> {
        let mut released = Vec::new();
        self.held.retain(|h| {
            if h.keys.held(src) {
                true
            } else {
                released.push(h.id);
                false
            }
        });
        released
    }

    /// Force-release one id (unregister, failed timer). `true` if it was held.
    pub fn release(&mut self, id: i32) -> bool {
        let before = self.held.len();
        self.held.retain(|h| h.id != id);
        self.held.len() != before
    }

    /// Force-release everything (shutdown, input desktop lost). Returns the released ids.
    pub fn release_all(&mut self) -> Vec<i32> {
        self.held.drain(..).map(|h| h.id).collect()
    }

    /// `true` when nothing is held, so the poll timer must not run.
    pub fn is_idle(&self) -> bool {
        self.held.is_empty()
    }
}
