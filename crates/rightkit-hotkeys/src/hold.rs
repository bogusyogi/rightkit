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

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::HashSet;

    #[derive(Default)]
    struct Fake(RefCell<HashSet<u16>>);

    impl Fake {
        fn down(&self, vk: u16) {
            self.0.borrow_mut().insert(vk);
        }
        fn up(&self, vk: u16) {
            self.0.borrow_mut().remove(&vk);
        }
        fn all_up(&self) {
            self.0.borrow_mut().clear();
        }
    }

    impl KeyStateSource for Fake {
        fn is_down(&self, vk: u16) -> bool {
            self.0.borrow().contains(&vk)
        }
    }

    const CTRL: u16 = 0x11;
    const SPACE: u16 = 0x20;

    fn ctrl_space() -> HoldKeys {
        HoldKeys {
            key: SPACE,
            modifier_groups: vec![vec![CTRL]],
        }
    }

    #[test]
    fn press_then_release_exactly_once() {
        let fake = Fake::default();
        fake.down(CTRL);
        fake.down(SPACE);
        let mut t = HoldTracker::new();
        assert!(t.press(1, ctrl_space()));
        assert!(t.poll(&fake).is_empty());
        assert!(t.poll(&fake).is_empty());
        fake.up(SPACE);
        assert_eq!(t.poll(&fake), vec![1]);
        assert!(t.poll(&fake).is_empty(), "no second Released");
        assert!(t.is_idle());
    }

    #[test]
    fn repeated_press_while_held_is_not_a_second_press() {
        let mut t = HoldTracker::new();
        assert!(t.press(1, ctrl_space()));
        assert!(!t.press(1, ctrl_space()));
    }

    #[test]
    fn releasing_the_modifier_first_ends_the_hold() {
        let fake = Fake::default();
        fake.down(CTRL);
        fake.down(SPACE);
        let mut t = HoldTracker::new();
        t.press(1, ctrl_space());
        fake.up(CTRL);
        assert_eq!(t.poll(&fake), vec![1]);
    }

    #[test]
    fn modifier_group_accepts_either_side() {
        let fake = Fake::default();
        fake.down(0x5C); // VK_RWIN
        fake.down(SPACE);
        let mut t = HoldTracker::new();
        t.press(
            2,
            HoldKeys {
                key: SPACE,
                modifier_groups: vec![vec![0x5B, 0x5C]],
            },
        );
        assert!(t.poll(&fake).is_empty());
    }

    #[test]
    fn focus_or_desktop_loss_reads_all_keys_up_and_releases_once() {
        // Secure desktop / lock screen / UAC: the OS reports every key up to this process.
        let fake = Fake::default();
        fake.down(CTRL);
        fake.down(SPACE);
        let mut t = HoldTracker::new();
        t.press(1, ctrl_space());
        t.press(
            2,
            HoldKeys {
                key: 0x41,
                modifier_groups: vec![],
            },
        );
        fake.all_up();
        let mut released = t.poll(&fake);
        released.sort();
        assert_eq!(released, vec![1, 2]);
        assert!(t.poll(&fake).is_empty());
    }

    #[test]
    fn forced_release_is_exactly_once() {
        let fake = Fake::default();
        fake.down(CTRL);
        fake.down(SPACE);
        let mut t = HoldTracker::new();
        t.press(1, ctrl_space());
        assert!(t.release(1));
        assert!(!t.release(1));
        assert!(t.poll(&fake).is_empty());
        t.press(3, ctrl_space());
        assert_eq!(t.release_all(), vec![3]);
        assert!(t.release_all().is_empty());
        assert!(t.is_idle());
    }
}
