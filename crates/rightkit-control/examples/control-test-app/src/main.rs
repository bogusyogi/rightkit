//! Tiny standalone Tauri app proving the rightkit-control in-app server.
//! The control plugin is only added when the launcher enables it by env.
use rightkit_control::embedded::Control;

// Native readback for proof: page focus alone cannot prove NSWindow key state.
#[cfg(target_os = "macos")]
fn window_state(app: &tauri::AppHandle) -> Result<String, String> {
    use std::ffi::{c_char, c_void};
    use tauri::Manager;
    #[link(name = "objc")]
    extern "C" {
        fn sel_registerName(name: *const c_char) -> *const c_void;
        fn objc_msgSend();
    }
    let win = app
        .get_webview_window("main")
        .ok_or("missing main window")?;
    let (tx, rx) = std::sync::mpsc::channel();
    win.with_webview(move |pv| unsafe {
        let send: unsafe extern "C" fn(*mut c_void, *const c_void) -> *mut c_void =
            std::mem::transmute(objc_msgSend as *const c_void);
        let get_bool: unsafe extern "C" fn(*mut c_void, *const c_void) -> bool =
            std::mem::transmute(objc_msgSend as *const c_void);
        let window = send(pv.inner(), sel_registerName(c"window".as_ptr()));
        if window.is_null() {
            let _ = tx.send(Err("webview has no window".to_string()));
            return;
        }
        let _ = tx.send(Ok(format!(
            "{{\"key\":{},\"visible\":{}}}",
            get_bool(window, sel_registerName(c"isKeyWindow".as_ptr())),
            get_bool(window, sel_registerName(c"isVisible".as_ptr()))
        )));
    })
    .map_err(|e| e.to_string())?;
    rx.recv_timeout(std::time::Duration::from_secs(10))
        .map_err(|e| e.to_string())?
}

fn main() {
    let mut builder = tauri::Builder::default();
    // Hidden QA launches must not activate this app or steal foreground focus.
    #[cfg(target_os = "macos")]
    {
        builder = builder.activate_ignoring_other_apps(false);
    }
    let control = Control::<tauri::Wry>::new()
        .command("ping", |_app, _body| Ok(r#""pong""#.into()))
        .command("echo", |_app, body| {
            Ok(format!(
                "{{\"echo\":\"{}\"}}",
                rightkit_control::json::esc(
                    &rightkit_control::json::str_field(body, "args").unwrap_or_default()
                )
            ))
        });
    #[cfg(target_os = "macos")]
    let control = control.command("window_state", |app, _| window_state(app));
    if let Some(plugin) = control.build_if_enabled() {
        builder = builder.plugin(plugin);
    }
    builder
        .run(tauri::generate_context!())
        .expect("run control-test-app");
}
