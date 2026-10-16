//! Tier 2 client (feature `control`): `rightkit_automation::in_app::InApp` implemented over the
//! embedded control server. QA ONLY, NEVER SHIPPED: it is compiled with the same `control`
//! feature as [`crate::embedded`], so a binary that carries it also carries the release marker.
//!
//! A Tauri app that embeds the server (launched with `RIGHTKIT_CONTROL_SERVICE=<name>`) accepts
//! trusted input injected straight into its webview, with no focus and no AX grant.
//! [`ServiceBridge`] is the client: it speaks the shared `rightkit-daemon` convention
//! (discovery file, current-user-only Unix socket or named pipe, per-app request allowlist).
//! There is no TCP and no bearer token. Register it with
//! `rightkit_automation::Automation::add_in_app`. Moved from `rightkit-automation`'s former
//! `in_app` client (0.3.0, feature `in-app`).

use rightkit_automation::in_app::InApp;
use rightkit_automation::keys::{Chord, Key, Named};
use rightkit_automation::{Button, Error, InputEvent, Result};
use rightkit_daemon::{Client, ServiceError, Suite, ERROR_NOT_ALLOWED};
use serde_json::{json, Value};

/// Client for one launched app instance, reached through its control service.
pub struct ServiceBridge {
    pub pid: i32,
    suite: Suite,
    service: String,
    app: String,
}

impl ServiceBridge {
    /// `service` is the lowercase name the launcher gave the app in
    /// `RIGHTKIT_CONTROL_SERVICE`; `app` is this caller's allowlist label
    /// (`right-qa` or `right-ctl` unless the app allowed another).
    pub fn new(pid: i32, suite: Suite, service: &str, app: &str) -> Result<Self> {
        if service.is_empty()
            || !service
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        {
            return Err(Error::Invalid(
                "control service name must be lowercase [a-z0-9-]".into(),
            ));
        }
        Ok(ServiceBridge {
            pid,
            suite,
            service: service.into(),
            app: app.into(),
        })
    }

    fn call(&self, method: &str, params: Value) -> Result<String> {
        let client = Client::connect(&self.suite, &self.service, &self.app).map_err(map_err)?;
        client.call(method, params).map_err(map_err)?;
        Ok(format!("service {method}"))
    }
}

fn map_err(e: ServiceError) -> Error {
    if e.code == ERROR_NOT_ALLOWED {
        Error::Blocked(format!("control service refused: {}", e.message))
    } else if e.retryable {
        Error::NotFound(format!("control service unavailable: {}", e.message))
    } else {
        Error::Platform(-1, "in_app request")
    }
}

fn key_name(c: Chord) -> Option<String> {
    let mut parts: Vec<String> = vec![];
    if c.mods.meta {
        parts.push("cmd".into());
    }
    if c.mods.ctrl {
        parts.push("ctrl".into());
    }
    if c.mods.alt {
        parts.push("alt".into());
    }
    if c.mods.shift {
        parts.push("shift".into());
    }
    parts.push(match c.key {
        Key::Named(n) => match n {
            Named::Return => "return",
            Named::Tab => "tab",
            Named::Space => "space",
            Named::Backspace => "backspace",
            Named::Escape => "escape",
            Named::Left => "left",
            Named::Right => "right",
            Named::Up => "up",
            Named::Down => "down",
            Named::Home => "home",
            Named::End => "end",
            Named::PageUp => "pageup",
            Named::PageDown => "pagedown",
            Named::Delete => return None,
        }
        .to_string(),
        Key::Char(c) => c.to_string(),
        Key::F(_) => return None,
    });
    Some(parts.join("+"))
}

impl InApp for ServiceBridge {
    fn handles(&self, pid: i32) -> bool {
        pid == self.pid
    }
    fn perform(&self, pid: i32, ev: &InputEvent) -> Result<String> {
        if !self.handles(pid) {
            return Err(Error::Unsupported("bridge belongs to another pid"));
        }
        match ev {
            InputEvent::Text(t) => self.call("type", json!({"text": t})),
            InputEvent::Key(c) => {
                let name = key_name(*c)
                    .ok_or(Error::Unsupported("key not expressible in control channel"))?;
                self.call("key", json!({"key": name}))
            }
            InputEvent::Click {
                x,
                y,
                button,
                count,
            } => self.call(
                "click",
                json!({"x": x, "y": y, "button": if *button == Button::Right { "right" } else { "left" }, "count": count}),
            ),
            InputEvent::Scroll { x, y, dx, dy } => {
                self.call("wheel", json!({"x": x, "y": y, "dx": dx, "dy": dy}))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rightkit_automation::keys;
    use rightkit_daemon::{host, Allowlist, CallContext, Emit, Handler, ServiceConfig};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    struct Rec(Mutex<Vec<(String, Value)>>);
    impl Handler for Rec {
        fn call(
            &self,
            _cx: &CallContext,
            method: &str,
            params: &Value,
            _e: &Emit,
        ) -> std::result::Result<Value, ServiceError> {
            self.0.lock().unwrap().push((method.into(), params.clone()));
            Ok(json!({"ok": true}))
        }
    }

    /// A real service host (socket/pipe + service.json) that the bridge reaches by discovery.
    fn start(allow: Allowlist) -> (Suite, Arc<Rec>, rightkit_daemon::Hosted) {
        let suite = Suite::per_run_temp("inapp").unwrap();
        let rec = Arc::new(Rec(Mutex::new(vec![])));
        let mut cfg = ServiceConfig::new("control-fake", "control:main", 0, "test", allow);
        cfg.takeover_timeout = Duration::from_secs(5);
        let hosted = host(&suite, cfg, rec.clone()).unwrap();
        (suite, rec, hosted)
    }

    #[test]
    fn type_reaches_the_service_with_exact_json() {
        let (suite, rec, hosted) = start(Allowlist::new().allow("right-qa", ["type", "key"]));
        let b = ServiceBridge::new(7, suite, "control-fake", "right-qa").unwrap();
        b.perform(7, &InputEvent::Text("hi \"there\" \\ \u{1f600}".into()))
            .unwrap();
        let seen = rec.0.lock().unwrap().clone();
        assert_eq!(
            seen,
            vec![(
                "type".to_string(),
                json!({"text": "hi \"there\" \\ \u{1f600}"})
            )]
        );
        hosted.shutdown();
    }

    #[test]
    fn unlisted_app_is_blocked_and_wrong_pid_unsupported() {
        let (suite, rec, hosted) = start(Allowlist::new().allow("right-qa", ["type"]));
        let chord = keys::parse_chord("cmd+k", true).unwrap();
        let denied = ServiceBridge::new(7, suite.clone(), "control-fake", "right-qa").unwrap();
        assert!(
            matches!(
                denied.perform(7, &InputEvent::Key(chord)),
                Err(Error::Blocked(_))
            ),
            "method outside allowlist"
        );
        let stranger = ServiceBridge::new(7, suite, "control-fake", "someone-else").unwrap();
        assert!(matches!(
            stranger.perform(7, &InputEvent::Text("x".into())),
            Err(Error::Blocked(_))
        ));
        assert!(matches!(
            denied.perform(8, &InputEvent::Text("x".into())),
            Err(Error::Unsupported(_))
        ));
        assert!(
            rec.0.lock().unwrap().is_empty(),
            "nothing reached the handler"
        );
        assert!(
            ServiceBridge::new(1, Suite::per_run_temp("x").unwrap(), "Bad Name", "right-qa")
                .is_err()
        );
        hosted.shutdown();
    }

    #[test]
    fn key_names_follow_control_channel_grammar() {
        assert_eq!(
            key_name(keys::parse_chord("cmd+shift+k", true).unwrap()).unwrap(),
            "cmd+shift+k"
        );
        assert_eq!(
            key_name(keys::parse_chord("return", true).unwrap()).unwrap(),
            "return"
        );
        assert!(key_name(keys::parse_chord("f5", true).unwrap()).is_none());
    }
}
