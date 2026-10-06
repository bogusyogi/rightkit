//! macOS primitives: `CGEventTap` ([`tap`]), fn chord + push-to-talk ([`fn_hotkey`]), middle-click
//! ([`middle_mouse`]), Carbon global shortcuts ([`global`]) and synthetic input ([`synth`]).

pub mod ffi;
pub mod fn_hotkey;
pub mod global;
pub mod middle_mouse;
pub mod synth;
pub mod tap;

pub use fn_hotkey::{FnHotkeyCallbacks, FnHotkeyTap, PttKey};
pub use global::{GlobalHotkeys, HotkeyId, HotkeyState, RegisterError};
pub use middle_mouse::{ax_link_at, MiddleMouseTap};
pub use tap::{EventTap, TapConfig, TapDecision, TapError, TapEvent, TapKind, TapLocation};
