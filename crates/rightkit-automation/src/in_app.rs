//! Tier 2: the in-app control channel, as a trait.
//!
//! Some apps can take trusted input straight into their own UI layer (for a webview app,
//! injected into the webview) with no focus change and no accessibility grant. This crate
//! does not know how: it only defines what the ladder needs from such a channel. The
//! implementation belongs to whoever owns the channel (the dev-only control server in
//! `rightkit-qa`, behind its `control` feature), and is registered with
//! [`Automation::add_in_app`](crate::Automation::add_in_app). No production build of this
//! crate links any such server.
//!
//! The engine, not the channel, enforces policy. Before tier 2 is tried the engine has
//! already passed the kill switch, the allowlist and deny list, the lease fence, the
//! freshness guard, the secure-field, credential and risk gates and the caller's admission
//! hook, and it records the tier in the receipt. A channel must not widen what it is asked
//! to do and must not be reachable except through [`Automation`](crate::Automation).
//!
//! Contract for an implementation:
//!
//! - [`InApp::handles`] is cheap, side-effect free and exact: `true` only for a pid whose
//!   app this channel really controls. With no channel for a pid the ladder moves on.
//! - [`InApp::perform`] delivers one [`InputEvent`] to that app and must not activate it,
//!   raise its window, move the user's cursor or post to the global input queue.
//! - An event the channel cannot express (a key it has no name for, a pointer event with
//!   no viewport mapping) returns [`Error::Unsupported`], so the ladder moves to tier 3
//!   instead of failing the action.
//! - A refusal by the app's own policy returns [`Error::Blocked`]; an app that is not
//!   reachable returns [`Error::NotFound`]. Anything else is a hard failure of this tier.
//! - The returned string is a short, content-free note for the receipt (a method name, never
//!   typed text, coordinates of a secret field or a payload).

use crate::model::*;
use crate::platform::InputEvent;

/// A per-process in-app channel.
pub trait InApp: Send + Sync {
    /// Whether this channel controls the app with this process id.
    fn handles(&self, pid: i32) -> bool;
    /// Deliver one input event to the app. See the module contract for the error kinds.
    fn perform(&self, pid: i32, ev: &InputEvent) -> Result<String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::parse_chord;
    use std::sync::{Arc, Mutex};

    struct Fake {
        pid: i32,
        seen: Mutex<Vec<InputEvent>>,
    }

    impl InApp for Fake {
        fn handles(&self, pid: i32) -> bool {
            pid == self.pid
        }
        fn perform(&self, pid: i32, ev: &InputEvent) -> Result<String> {
            if !self.handles(pid) {
                return Err(Error::Unsupported("channel belongs to another pid"));
            }
            self.seen.lock().unwrap().push(ev.clone());
            Ok("fake".into())
        }
    }

    #[test]
    fn the_trait_is_object_safe_and_shareable() {
        let f = Arc::new(Fake {
            pid: 7,
            seen: Mutex::new(vec![]),
        });
        let dynamic: Arc<dyn InApp> = f.clone();
        assert!(dynamic.handles(7));
        assert!(!dynamic.handles(8));
        let chord = parse_chord("cmd+k", true).unwrap();
        assert_eq!(dynamic.perform(7, &InputEvent::Key(chord)).unwrap(), "fake");
        assert!(matches!(
            dynamic.perform(8, &InputEvent::Text("x".into())),
            Err(Error::Unsupported(_))
        ));
        assert_eq!(f.seen.lock().unwrap().len(), 1);
    }
}
