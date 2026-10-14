//! Tiny standalone Tauri app proving the rightkit-qa-driver in-app server.
//! The control plugin is only added when the launcher enables it by env.
use rightkit_qa_driver::embedded::Control;

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
        let occlusion: unsafe extern "C" fn(*mut c_void, *const c_void) -> u64 =
            std::mem::transmute(objc_msgSend as *const c_void);
        #[repr(C)]
        struct Rect(f64, f64, f64, f64);
        let frame: unsafe extern "C" fn(*mut c_void, *const c_void) -> Rect =
            std::mem::transmute(objc_msgSend as *const c_void);
        let f = frame(window, sel_registerName(c"frame".as_ptr()));
        let _ = tx.send(Ok(format!(
            "{{\"frame\":[{},{},{},{}],\"key\":{},\"visible\":{},\"occlusion\":{}}}",
            f.0, f.1, f.2, f.3,
            get_bool(window, sel_registerName(c"isKeyWindow".as_ptr())),
            get_bool(window, sel_registerName(c"isVisible".as_ptr())),
            occlusion(window, sel_registerName(c"occlusionState".as_ptr()))
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
                rightkit_qa_driver::json::esc(
                    &rightkit_qa_driver::json::str_field(body, "args").unwrap_or_default()
                )
            ))
        });
    #[cfg(target_os = "macos")]
    let control = control.command("window_state", |app, _| window_state(app));
    if let Some(plugin) = control.build_if_enabled() {
        builder = builder.plugin(plugin);
    }
    builder
        .setup(|app| {
            // Mimics apps (Pulse) that `window.show()` a `visible:false` window at startup in hidden
            // QA runs: the window is ordered in while the app stays accessory and unfocused.
            #[cfg(target_os = "macos")]
            if std::env::var("RK_FIXTURE_SHOW").as_deref() == Ok("1") {
                use tauri::Manager;
                // Pulse order: show the window, then drop to Accessory.
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.show();
                }
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            let _ = app;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("run control-test-app");
}
