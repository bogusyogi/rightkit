//! Webview hardening: no stray webview context menu, reload / zoom /
//! history keys blocked, drag-drop cannot navigate, external links leave for the system browser.
//!
//! Three layers, because no single one covers every engine:
//! 1. an initialisation script (all frames by default) that cancels the DOM events;
//! 2. native webview settings (WKWebView preferences, WebView2 settings) for what JavaScript
//!    cannot cancel (Cmd+Opt+I inspector, WebView2 accelerator keys, pinch zoom, swipe);
//! 3. the Rust navigation hook (`NavPolicy`) that catches whatever navigates anyway.

use serde::Serialize;
use tauri::{Runtime, Webview};

use crate::devtools::WebviewDecision;

/// Frame scope for the full shell plugin's DOM hardening script.
/// Native webview settings & Rust navigation policy are configured separately.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum HardeningScript {
    /// Inject into every frame, including document iframes (legacy default).
    #[default]
    AllFrames,
    /// Inject into the top frame of each webview only.
    TopFrameOnly,
    /// Do not inject the DOM hardening script.
    Off,
}

#[derive(Debug, Clone)]
pub struct Hardening {
    /// Cancel `contextmenu` outside designated surfaces.
    pub block_context_menu: bool,
    /// Keep the native Cut/Copy/Paste menu inside inputs, textareas and contenteditable
    /// (HeardRight behaviour). Mark any other surface with `data-rk-native-menu`.
    pub allow_editable_menu: bool,
    /// Block F5 and Cmd/Ctrl+R (+Shift) reload shortcuts only. Print and view-source have
    /// their own switches below, so an app can keep reload blocked without losing them.
    pub block_reload: bool,
    /// Block Cmd/Ctrl+P (print). Split from `block_reload`: MailRight needs print while
    /// reload stays blocked.
    pub block_print: bool,
    /// Block Cmd/Ctrl+U (view source) outside editable surfaces. Inside an input, textarea or
    /// contenteditable the same chord is the underline command and is never cancelled, so a
    /// compose editor keeps underline while view-source stays blocked elsewhere. Inspector
    /// shortcuts (F12, Cmd/Ctrl+Shift+I/J/C, Cmd+Opt+I/J/C) are not governed by this switch:
    /// the DevTools layer always cancels them outside registered content webviews.
    pub block_view_source: bool,
    /// Block Cmd/Ctrl +/-/0, ctrl+wheel and trackpad pinch zoom.
    pub block_zoom: bool,
    /// Block Alt+Left/Right, Cmd+[ / ], browser back/forward keys and mouse buttons 4/5.
    pub block_history_nav: bool,
    /// A dropped file must never navigate the webview; only `data-rk-dropzone` accepts drops.
    pub block_drop_navigation: bool,
    /// Route http(s)/mailto link clicks and `window.open` to the system browser.
    pub intercept_external_links: bool,
    /// Windows (WebView2): turn the engine's built-in browser accelerator keys off
    /// (`SetAreBrowserAcceleratorKeysEnabled(false)`), so Ctrl+F (find) and Ctrl+P (print) do
    /// nothing natively and the app handles them. The page still receives the key events; pair it
    /// with `block_print: false` so the shell's own script does not cancel Ctrl+P. WebView2 treats
    /// this as all-or-nothing for the webview: the same switch also covers its reload, zoom,
    /// inspector and back/forward keys, in a webview whose DevTools the policy allows too. The
    /// per-key rules (`block_reload`, the DevTools key policy) keep running and stay the shell's
    /// authority. No effect on macOS. Default `false`.
    pub disable_browser_accelerators: bool,
}

impl Default for Hardening {
    fn default() -> Self {
        Self {
            block_context_menu: true,
            allow_editable_menu: true,
            block_reload: true,
            // Compatible defaults: `block_reload` used to cover print and view-source too, so
            // the split switches default on and `Hardening::default()` blocks exactly what it
            // blocked before.
            block_print: true,
            block_view_source: true,
            block_zoom: true,
            block_history_nav: true,
            block_drop_navigation: true,
            intercept_external_links: true,
            disable_browser_accelerators: false,
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ScriptConfig {
    block_context_menu: bool,
    allow_editable_menu: bool,
    block_reload: bool,
    block_print: bool,
    block_view_source: bool,
    block_zoom: bool,
    block_history_nav: bool,
    content_devtools_labels: Vec<String>,
    block_drop_navigation: bool,
    intercept_external_links: bool,
}

const SCRIPT_BODY: &str = include_str!("harden.js");

impl Hardening {
    /// The initialisation script for this configuration (idempotent per document). DevTools
    /// shortcuts are always cancelled; see [`Hardening::script_for`] for content webviews.
    pub fn script(&self) -> String {
        self.script_for(&[])
    }

    /// Like [`Hardening::script`], for an app whose `DevToolsPolicy::ContentOnly` registered the
    /// webviews in `content_labels`. In those webviews the script leaves the context menu and the
    /// DevTools shortcuts alone (the native layer decides what they may do); everywhere else
    /// F12, Ctrl+Shift+I/J/C and Cmd+Opt+I/J/C/U are cancelled.
    pub fn script_for(&self, content_labels: &[String]) -> String {
        let cfg = ScriptConfig {
            block_context_menu: self.block_context_menu,
            allow_editable_menu: self.allow_editable_menu,
            block_reload: self.block_reload,
            block_print: self.block_print,
            block_view_source: self.block_view_source,
            block_zoom: self.block_zoom,
            block_history_nav: self.block_history_nav,
            content_devtools_labels: content_labels.to_vec(),
            block_drop_navigation: self.block_drop_navigation,
            intercept_external_links: self.intercept_external_links,
        };
        let json = serde_json::to_string(&cfg).expect("static config serialises");
        SCRIPT_BODY.replace("__RK_SHELL_CONFIG__", &json)
    }
}

/// Apply the native (non-DOM, non-DevTools) hardening to a freshly created webview. The DevTools
/// settings are applied separately and unconditionally by [`crate::devtools::apply_native`];
/// `decision` only tells this layer to leave a content webview's context menu to that layer.
pub fn apply_native<R: Runtime>(webview: &Webview<R>, h: &Hardening, decision: WebviewDecision) {
    #[cfg(target_os = "macos")]
    macos::apply(webview, h);
    #[cfg(windows)]
    windows_impl::apply(webview, h, decision);
    #[cfg(not(windows))]
    let _ = decision;
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (webview, h);
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    pub fn apply<R: Runtime>(webview: &Webview<R>, h: &Hardening) {
        let zoom_off = h.block_zoom;
        let nav_off = h.block_history_nav;
        let _ = webview.with_webview(move |platform| unsafe {
            // SAFETY: Tauri hands over the live WKWebView for this webview; used only inside this
            // main-thread closure.
            let wk = &*(platform.inner() as *mut AnyObject);
            if zoom_off {
                let (): () = msg_send![wk, setAllowsMagnification: false];
            }
            if nav_off {
                let (): () = msg_send![wk, setAllowsBackForwardNavigationGestures: false];
            }
        });
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use crate::webview2_menu::{self, SuppressRule};
    use crate::winapi::webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2Settings3, ICoreWebView2Settings5, ICoreWebView2Settings6,
    };
    use crate::winapi::windows::core::Interface;

    /// WebView2 side of the same policy. NOT compiled on this (macOS) host: compile and verify on
    /// the Windows agent with `rustright cargo check -p rightkit-tauri-shell` (see API.md).
    pub fn apply<R: Runtime>(webview: &Webview<R>, h: &Hardening, decision: WebviewDecision) {
        let h = h.clone();
        let content = decision != WebviewDecision::Deny;
        // F5, Ctrl+R, Alt+Left and the zoom keys are stopped one by one by the shell's single key
        // handler (the DevTools layer adds it); browser keys are never turned off as a whole, so
        // print and find keep working.
        crate::devtools::set_hardening_keys(
            webview.label(),
            h.block_reload,
            h.block_history_nav,
            h.block_zoom,
        );
        let label = webview.label().to_owned();
        let _ = webview.with_webview(move |platform| unsafe {
            let Ok(core) = platform.controller().CoreWebView2() else {
                return;
            };
            let Ok(settings) = core.Settings() else {
                return;
            };
            let _ = settings.SetIsStatusBarEnabled(false);
            if h.disable_browser_accelerators {
                let off = settings
                    .cast::<ICoreWebView2Settings3>()
                    .and_then(|s3| s3.SetAreBrowserAcceleratorKeysEnabled(false));
                if let Err(e) = off {
                    eprintln!("rightkit-tauri-shell: browser accelerator keys stay on: {e}");
                }
            }
            if h.block_zoom {
                let _ = settings.SetIsZoomControlEnabled(false);
                if let Ok(s5) = settings.cast::<ICoreWebView2Settings5>() {
                    let _ = s5.SetIsPinchZoomEnabled(false);
                }
            }
            if h.block_history_nav {
                if let Ok(s6) = settings.cast::<ICoreWebView2Settings6>() {
                    let _ = s6.SetIsSwipeNavigationEnabled(false);
                }
            }
            // Content webviews: the DevTools layer owns their menu filter. Everywhere else the
            // engine menu stays hidden, except an editable field when allowed. This is only the
            // hiding rule: `webview2_menu` runs it in the one handler of the webview, after the
            // DevTools filter and the app's items, which it lets show.
            let hide = (h.block_context_menu && !content).then_some(SuppressRule {
                allow_editable: h.allow_editable_menu,
            });
            webview2_menu::set_suppress(&label, hide);
            webview2_menu::sync(&label, &core);
        });
    }
}
