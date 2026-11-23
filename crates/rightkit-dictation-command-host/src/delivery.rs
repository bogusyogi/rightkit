// Legacy delivery API, explicit clipboard copy & foreground target utilities.
// Automatic paste requires paste_delivery::DeliveryHost policy; legacy input
// entry points refuse before effects when no host-bound evidence is available.

pub use rightkit_transcript_library::delivery::*;

mod clipboard;
mod deliver;
mod insert;
// Compatibility stubs reject calls without host-bound evidence.
#[allow(dead_code)]
#[cfg(any(target_os = "windows", target_os = "macos"))]
mod keystrokes;
mod targets;

#[cfg(any(target_os = "windows", target_os = "macos"))]
pub use clipboard::fetch_selected_text_via_copy;
pub use deliver::{
    copy_fallback_record, copy_text, deliver_text, deliver_text_with_enter, paste_text,
};
pub use targets::{restore_and_verify, snapshot_target};
