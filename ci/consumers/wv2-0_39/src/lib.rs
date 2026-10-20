//! Gate consumer for `rightkit-tauri-shell` with `wv2-0_39` on a Tauri 2.12 lock.
//! CI checks plain & engine feature rows through this standalone workspace.

/// Compiles only when Tauri's `PlatformWebview::controller()` and this crate's `webview2-com` 0.39 /
/// `windows` 0.62 are the same generation: `cast` needs the `Interface` trait of the type's own
/// windows-core. Against Tauri 2.11 (webview2-com 0.38) this is the E0599 the wv2 features exist to avoid.
#[cfg(windows)]
pub fn boundary_is_one_generation(pv: &tauri::webview::PlatformWebview) -> bool {
    use webview2_com::Microsoft::Web::WebView2::Win32::ICoreWebView2_3;
    use windows::core::Interface;
    // SAFETY: a COM call on the controller Tauri owns; this function is only type-checked by the gate.
    unsafe { pv.controller().CoreWebView2() }
        .and_then(|core| core.cast::<ICoreWebView2_3>())
        .is_ok()
}
