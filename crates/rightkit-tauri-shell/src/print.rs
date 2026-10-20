//! Printing the current webview (Orthic-Labs/rightkit#386).
//!
//! Donor: ViewRight `commands/print.rs`. On Windows the system print dialog cannot preview WebView2
//! content, so the default is WebView2's own in-app print preview (`ICoreWebView2_16::ShowPrintUI`
//! with the browser kind). On macOS the webview's own print operation runs (WKWebView shows the
//! system print panel, macOS 11 and later). Other platforms answer `Unsupported`. Print CSS in the
//! page owns header suppression and page layout.
//!
//! This is what the app calls from its own Ctrl/Cmd+P handler once
//! `Hardening::disable_browser_accelerators` has turned the engine's built-in print key off.

use serde::Deserialize;
use tauri::{Emitter, Manager, Runtime, WebviewWindow, Wry};

use crate::ShellError;

/// Emitted by [`rk_shell_print`] when WebView2 refuses the print UI after the call has returned
/// (the payload is the error text). Windows only.
pub const EVENT_PRINT_ERROR: &str = "rightkit-shell://print-error";

/// Which print UI to show on Windows. Other platforms always use the system panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum PrintUi {
    /// WebView2's in-app print preview (the default).
    #[default]
    Browser,
    /// The Windows system print dialog (no preview of WebView2 content).
    System,
}

/// Show the print UI for `window`'s webview. On Windows the call is dispatched to the webview's
/// thread and its outcome is not returned (`Err` means the webview could not be reached); a failure
/// inside WebView2 is written to stderr as `rightkit-tauri-shell: print UI failed: ...`. Use
/// [`show_print_ui_with`] to receive it. `Err(Unsupported)` off Windows and macOS.
pub fn show_print_ui<R: Runtime>(window: &WebviewWindow<R>, ui: PrintUi) -> Result<(), ShellError> {
    show_print_ui_with(window, ui, |error| {
        eprintln!("rightkit-tauri-shell: print UI failed: {error}")
    })
}

/// Like [`show_print_ui`], with `on_error` called (on the webview's thread) when WebView2 refuses.
pub fn show_print_ui_with<R: Runtime>(
    window: &WebviewWindow<R>,
    ui: PrintUi,
    on_error: impl FnOnce(String) + Send + 'static,
) -> Result<(), ShellError> {
    #[cfg(windows)]
    {
        use crate::winapi::webview2_com::Microsoft::Web::WebView2::Win32::{
            ICoreWebView2_16, COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER,
            COREWEBVIEW2_PRINT_DIALOG_KIND_SYSTEM,
        };
        use crate::winapi::windows::core::Interface;

        let kind = match ui {
            PrintUi::Browser => COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER,
            PrintUi::System => COREWEBVIEW2_PRINT_DIALOG_KIND_SYSTEM,
        };
        window
            .with_webview(move |webview| {
                // SAFETY: Tauri hands over the live controller; used only inside this call.
                let result = unsafe {
                    webview.controller().CoreWebView2().and_then(|core| {
                        let printable: ICoreWebView2_16 = core.cast()?;
                        printable.ShowPrintUI(kind)
                    })
                };
                if let Err(error) = result {
                    on_error(format!("{error:?}"));
                }
            })
            .map_err(|e| ShellError::Other(format!("webview unavailable: {e}")))
    }
    #[cfg(target_os = "macos")]
    {
        let _ = (ui, on_error);
        window
            .print()
            .map_err(|e| ShellError::Other(format!("print failed: {e}")))
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        let _ = (window, ui, on_error);
        Err(ShellError::Unsupported(
            "printing is supported on Windows and macOS only".into(),
        ))
    }
}

/// Print the calling window's webview. `ui` picks the Windows print UI (default: the in-app
/// preview) and is ignored on macOS. A WebView2 refusal after dispatch arrives as the
/// [`EVENT_PRINT_ERROR`] event.
#[tauri::command]
pub fn rk_shell_print(
    webview: tauri::Webview<Wry>,
    ui: Option<PrintUi>,
    window: WebviewWindow<Wry>,
) -> Result<(), ShellError> {
    crate::commands::guard_command(&webview, "rk_shell_print")?;
    let app = window.app_handle().clone();
    show_print_ui_with(&window, ui.unwrap_or_default(), move |error| {
        let _ = app.emit(EVENT_PRINT_ERROR, error);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn print_ui_defaults_to_the_in_app_preview_and_parses_kebab_case() {
        assert_eq!(PrintUi::default(), PrintUi::Browser);
        let ui: PrintUi = serde_json::from_str("\"system\"").unwrap();
        assert_eq!(ui, PrintUi::System);
        let ui: PrintUi = serde_json::from_str("\"browser\"").unwrap();
        assert_eq!(ui, PrintUi::Browser);
        assert!(serde_json::from_str::<PrintUi>("\"pdf\"").is_err());
        let none: Option<PrintUi> = serde_json::from_str("null").unwrap();
        assert_eq!(none.unwrap_or_default(), PrintUi::Browser);
    }
}
