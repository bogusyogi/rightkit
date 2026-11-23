//! macOS microphone consent through `AVCaptureDevice`.

pub(super) fn authorization_status() -> Result<isize, String> {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;
    use objc2_foundation::NSString;

    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    let cls = AnyClass::get(c"AVCaptureDevice").ok_or("AVCaptureDevice unavailable")?;
    let media = NSString::from_str("soun");
    let status: isize = unsafe { msg_send![cls, authorizationStatusForMediaType: &*media] };
    Ok(status)
}

pub(super) fn status_label(status: isize) -> &'static str {
    match status {
        3 => "authorized",
        2 => "denied",
        // Restricted (MDM/parental control block): the user cannot grant this themselves,
        // unlike `denied`, which they declined and can undo.
        1 => "restricted",
        0 => "not_determined",
        _ => "unknown",
    }
}

/// Request microphone consent the native way: `AVCaptureDevice.requestAccess`, which shows the
/// real macOS dialog (a WKWebView `getUserMedia` is auto-denied in wry, so the WebView path never
/// prompts). Returns "authorized" | "denied" | "restricted" | "pending" (no answer within 120 s).
/// The consent gesture needs the `com.apple.security.device.audio-input` entitlement under the
/// hardened runtime.
///
/// Synchronous: the `recv_timeout` wait on the completion handler blocks the calling thread, so
/// hosts call this from a blocking worker.
pub(super) fn request_access(event_sink: fn(&str, &str)) -> Result<String, String> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, Bool};
    use objc2_foundation::NSString;

    // Force-link AVFoundation so the AVCaptureDevice class is registered.
    #[link(name = "AVFoundation", kind = "framework")]
    extern "C" {}

    let cls = AnyClass::get(c"AVCaptureDevice").ok_or("AVCaptureDevice unavailable")?;
    // AVMediaTypeAudio is the four-character code "soun".
    let media = NSString::from_str("soun");
    let status = authorization_status()?;
    event_sink(
        "microphone_permission_status",
        &format!("preflight={status}"),
    );
    match status {
        3 => {
            event_sink("microphone_permission_result", "authorized_existing");
            return Ok("authorized".into());
        }
        // Restricted (MDM/parental controls) is not "denied": the user cannot grant it, so
        // onboarding must not send them to Settings.
        1 => {
            event_sink("microphone_permission_result", "restricted_existing");
            return Ok("restricted".into());
        }
        2 => {
            event_sink("microphone_permission_result", "denied_existing");
            return Ok("denied".into());
        }
        _ => {}
    }
    event_sink("microphone_permission_prompt", "requesting");
    let (tx, rx) = std::sync::mpsc::channel::<bool>();
    let handler = block2::RcBlock::new(move |granted: Bool| {
        let _ = tx.send(granted.as_bool());
    });
    let _: () =
        unsafe { msg_send![cls, requestAccessForMediaType: &*media, completionHandler: &*handler] };
    match rx.recv_timeout(std::time::Duration::from_secs(120)) {
        Ok(true) => {
            event_sink("microphone_permission_result", "authorized_prompt");
            Ok("authorized".into())
        }
        Ok(false) => {
            event_sink("microphone_permission_result", "denied_prompt");
            Ok("denied".into())
        }
        Err(_) => {
            event_sink("microphone_permission_result", "pending_timeout");
            Ok("pending".into())
        }
    }
}
