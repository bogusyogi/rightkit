//! Design-mode element-picker primitives: a trusted binding bridge with one-shot submit
//! authentication, device-pixel capture geometry, and the picker script builder.
//!
//! The picker runs in an isolated world that [`BrowserPage::enable_design_mode`](crate::BrowserPage::enable_design_mode)
//! creates. Page script cannot read that world, and the picker reports through a
//! `Runtime.addBinding` function bound only to it, so page code can neither observe nor forge
//! messages (console output is never trusted). A prompt submission becomes an automatic agent
//! message only when it carries the enable nonce, comes from the isolated context, has an id
//! above every earlier accepted submission, and consumes one native input gesture
//! ([`NativeGestureLatch`]); one gesture authorizes exactly one submission.

use crate::error::{BrowserError, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::Mutex;
use std::time::Instant;

/// How long after a native input event a prompt submission is still accepted.
pub const DESIGN_MODE_GESTURE_WINDOW_MS: u64 = 1_500;

/// The picker script template (see `design_mode_picker.js`); the binding and nonce tokens are
/// replaced by [`build_design_mode_script`].
const PICKER_TEMPLATE: &str = include_str!("design_mode_picker.js");

/// Monotonic milliseconds, never zero (zero means "no gesture").
pub fn now_ms() -> u64 {
    static BASE: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    BASE.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
}

/// One-shot record of the latest native input gesture.
#[derive(Debug, Default)]
pub struct NativeGestureLatch(Mutex<u64>);

impl NativeGestureLatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a native gesture at `now_ms`.
    pub fn stamp_at(&self, now_ms: u64) {
        *self.0.lock().unwrap() = now_ms;
    }

    pub fn stamp(&self) {
        self.stamp_at(now_ms());
    }

    /// Consume the gesture: true once if one is recent at `now_ms`; the stamp is cleared either way.
    pub fn take(&self, now_ms: u64) -> bool {
        let mut at = self.0.lock().unwrap();
        let recent = gesture_is_recent(*at, now_ms);
        *at = 0;
        recent
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DesignModeMarker {
    ElementSelected(Value),
    PromptSubmitted(Value),
    Dismissed,
}

/// Receiver of picker messages for one enable operation.
pub struct DesignModeSession {
    pub events: tokio::sync::mpsc::UnboundedReceiver<DesignModeMarker>,
}

/// Validates picker binding calls for one enable operation.
#[derive(Debug, Clone)]
pub struct DesignModeBridge {
    nonce: String,
    last_submit_id: u64,
}

fn check_nonce(nonce: &str) -> Result<()> {
    if nonce.is_empty() || !nonce.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(BrowserError::Invalid(
            "design-mode nonce must be non-empty ASCII alphanumeric".into(),
        ));
    }
    Ok(())
}

/// Name of the `Runtime.addBinding` function for `nonce`.
pub fn binding_name_for_nonce(nonce: &str) -> String {
    format!("__omni_{nonce}_bridge__")
}

/// Name of the isolated world (and binding context) for `nonce`.
pub fn world_name_for_nonce(nonce: &str) -> String {
    format!("__omni_{nonce}_world__")
}

impl DesignModeBridge {
    pub fn new(nonce: &str) -> Result<Self> {
        check_nonce(nonce)?;
        Ok(Self {
            nonce: nonce.to_string(),
            last_submit_id: 0,
        })
    }

    pub fn binding_name(&self) -> String {
        binding_name_for_nonce(&self.nonce)
    }

    pub fn world_name(&self) -> String {
        world_name_for_nonce(&self.nonce)
    }

    /// Decode one `Runtime.bindingCalled` event. `trusted_context` is true only when the call
    /// came from the isolated context this bridge created. Select and dismiss need the
    /// binding, context and nonce; a submission also needs a fresh id and consumes one gesture
    /// from `gesture`. A rejected submission does not advance the id or burn a later gesture.
    pub fn accept(
        &mut self,
        binding: &str,
        payload: &str,
        trusted_context: bool,
        gesture: &NativeGestureLatch,
        now_ms: u64,
    ) -> Option<DesignModeMarker> {
        if !trusted_context || binding != self.binding_name() {
            return None;
        }
        let v: Value = serde_json::from_str(payload).ok()?;
        if v.get("nonce").and_then(Value::as_str) != Some(self.nonce.as_str()) {
            return None;
        }
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        match v.get("kind").and_then(Value::as_str)? {
            "select" => Some(DesignModeMarker::ElementSelected(data)),
            "dismiss" => Some(DesignModeMarker::Dismissed),
            "submit" => {
                let id = data.get("id").and_then(Value::as_u64)?;
                if id <= self.last_submit_id || !gesture.take(now_ms) {
                    return None;
                }
                self.last_submit_id = id;
                Some(DesignModeMarker::PromptSubmitted(data))
            }
            _ => None,
        }
    }
}

/// Native input types that establish the gesture gate (`Input.dispatch*Event` types).
pub fn is_native_gesture_input(input_type: &str) -> bool {
    matches!(input_type, "mouseDown" | "keyDown" | "rawKeyDown")
}

/// Whether a gesture stamped at `last_gesture_at_ms` is still inside the window at `now_ms`.
/// A zero timestamp is never valid. `saturating_sub` keeps a clock step into the future from
/// underflowing.
pub fn gesture_is_recent(last_gesture_at_ms: u64, now_ms: u64) -> bool {
    last_gesture_at_ms != 0
        && now_ms.saturating_sub(last_gesture_at_ms) <= DESIGN_MODE_GESTURE_WINDOW_MS
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ElementRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureRect {
    pub x: i64,
    pub y: i64,
    pub width: u64,
    pub height: u64,
}

/// Convert CSS-pixel coordinates to device-pixel capture coordinates: all four values are
/// multiplied by the device scale and rounded, so a crop stays aligned after zoom changes.
/// Non-finite or non-positive rectangles are not capture requests and return `None`. A
/// non-finite or non-positive scale is treated as 1.
pub fn normalize_capture_rect(rect: ElementRect, device_scale: f64) -> Option<CaptureRect> {
    if !rect.x.is_finite()
        || !rect.y.is_finite()
        || !rect.width.is_finite()
        || !rect.height.is_finite()
        || rect.width <= 0.0
        || rect.height <= 0.0
    {
        return None;
    }
    let scale = if device_scale.is_finite() && device_scale > 0.0 {
        device_scale
    } else {
        1.0
    };
    Some(CaptureRect {
        x: (rect.x * scale).round() as i64,
        y: (rect.y * scale).round() as i64,
        width: (rect.width * scale).round().max(1.0) as u64,
        height: (rect.height * scale).round().max(1.0) as u64,
    })
}

/// Build the picker script for `nonce`. The nonce must be non-empty ASCII alphanumeric. Inject
/// it only into the isolated world whose binding [`DesignModeBridge`] validates.
pub fn build_design_mode_script(nonce: &str) -> Result<String> {
    check_nonce(nonce)?;
    Ok(PICKER_TEMPLATE
        .replace(
            "\"@@BINDING@@\"",
            &js_string(&binding_name_for_nonce(nonce)),
        )
        .replace("\"@@NONCE@@\"", &js_string(nonce)))
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(nonce: &str, kind: &str, data: Value) -> String {
        json!({"nonce": nonce, "kind": kind, "data": data}).to_string()
    }

    #[test]
    fn bridge_rejects_wrong_binding_context_nonce_and_kind() {
        let g = NativeGestureLatch::new();
        let mut b = DesignModeBridge::new("abc123").unwrap();
        let name = b.binding_name();
        let sel = call("abc123", "select", json!({"tag": "button"}));
        assert!(matches!(
            b.accept(&name, &sel, true, &g, 10),
            Some(DesignModeMarker::ElementSelected(_))
        ));
        assert!(b.accept(&name, &sel, false, &g, 10).is_none());
        assert!(b
            .accept("__omni_other_bridge__", &sel, true, &g, 10)
            .is_none());
        let forged = call("zzz", "select", json!({}));
        assert!(b.accept(&name, &forged, true, &g, 10).is_none());
        assert!(b.accept(&name, "not-json", true, &g, 10).is_none());
        assert!(b
            .accept(&name, &call("abc123", "bogus", Value::Null), true, &g, 10)
            .is_none());
        assert_eq!(
            b.accept(&name, &call("abc123", "dismiss", Value::Null), true, &g, 10),
            Some(DesignModeMarker::Dismissed)
        );
    }

    #[test]
    fn submit_needs_a_fresh_id_and_consumes_one_gesture() {
        let g = NativeGestureLatch::new();
        let mut b = DesignModeBridge::new("n").unwrap();
        let name = b.binding_name();
        let s1 = call("n", "submit", json!({"id": 1, "prompt": "x"}));
        assert!(b.accept(&name, &s1, true, &g, 2_000).is_none());
        g.stamp_at(1_000);
        assert!(matches!(
            b.accept(&name, &s1, true, &g, 2_000),
            Some(DesignModeMarker::PromptSubmitted(_))
        ));
        // Replay of the same message, even with a new gesture, is rejected.
        g.stamp_at(2_000);
        assert!(b.accept(&name, &s1, true, &g, 2_100).is_none());
        // One gesture authorizes one submission only.
        g.stamp_at(3_000);
        let s2 = call("n", "submit", json!({"id": 2}));
        let s3 = call("n", "submit", json!({"id": 3}));
        assert!(b.accept(&name, &s2, true, &g, 3_100).is_some());
        assert!(b.accept(&name, &s3, true, &g, 3_100).is_none());
        // An expired gesture is rejected and cleared.
        g.stamp_at(1_000);
        assert!(b.accept(&name, &s3, true, &g, 9_000).is_none());
    }

    #[test]
    fn only_real_native_input_types_stamp_the_gesture() {
        assert!(is_native_gesture_input("mouseDown"));
        assert!(is_native_gesture_input("keyDown"));
        assert!(is_native_gesture_input("rawKeyDown"));
        assert!(!is_native_gesture_input("mouseUp"));
        assert!(!is_native_gesture_input("wheel"));
        assert!(!gesture_is_recent(0, 1_000));
        assert!(gesture_is_recent(1_000, 2_500));
        assert!(!gesture_is_recent(1_000, 2_501));
    }

    #[test]
    fn capture_geometry_preserves_zoom_and_rejects_empty_rectangles() {
        assert_eq!(
            normalize_capture_rect(
                ElementRect {
                    x: 10.25,
                    y: -2.25,
                    width: 40.4,
                    height: 20.4
                },
                2.0,
            ),
            Some(CaptureRect {
                x: 21,
                y: -5,
                width: 81,
                height: 41
            })
        );
        assert_eq!(
            normalize_capture_rect(
                ElementRect {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 2.0
                },
                1.0
            ),
            None
        );
        assert_eq!(
            normalize_capture_rect(
                ElementRect {
                    x: 0.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0
                },
                f64::NAN,
            ),
            Some(CaptureRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1
            })
        );
    }

    #[test]
    fn picker_script_uses_the_binding_not_the_console() {
        let script = build_design_mode_script("abc123").unwrap();
        assert!(script.contains("var __OMNI_BINDING = \"__omni_abc123_bridge__\";"));
        assert!(script.contains("var __OMNI_NONCE = \"abc123\";"));
        assert!(!script.contains("@@"));
        assert!(!script.contains("console."));
        assert!(script.contains("window.__omniDisableDesignMode"));
        assert!(script.contains("split(/\\s+/)"));
    }

    #[test]
    fn picker_script_rejects_unsafe_nonces() {
        assert!(build_design_mode_script("").is_err());
        assert!(build_design_mode_script("ab\"cd").is_err());
        assert!(build_design_mode_script("ab_cd").is_err());
    }
}
