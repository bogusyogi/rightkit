//! Tray kill button: a "Stop demo" menu item that runs the same `DemoHost::kill` as the hotkey.
//! Compiled only under the `demo` feature, which is the only thing that enables Tauri's `tray-icon`.

use std::sync::Arc;

use rightkit_tauri_shell::tray::{install_with_options, TrayHandle, TrayOptions};
use rightkit_tauri_shell::{LeftClick, MenuEntry, TraySpec};
use tauri::{AppHandle, Manager, Wry};

use super::host::DemoHost;

const STOP_ID: &str = "demo-stop";

/// Adds the tray icon. A failure is returned to the caller, which logs it: the hotkey and the badge
/// remain the other kill controls.
struct DemoTray {
    _handle: TrayHandle,
}

pub fn install(app: &AppHandle<Wry>) -> Result<(), String> {
    let tray = install_with_options(
        app,
        &TraySpec {
            tooltip: format!("{} demo", crate::identity().product_name),
            items: vec![MenuEntry::item(STOP_ID, "Stop demo")],
            left_click: LeftClick::ShowMenu,
            ..Default::default()
        },
        "main",
        TrayOptions::custom_only(format!("{}-demo", crate::identity().slug)),
        Some(Arc::new(|app, id| {
            if id == STOP_ID {
                if let Some(host) = app.try_state::<Arc<DemoHost>>() {
                    host.inner().kill("tray");
                }
            }
        })),
    )?;
    app.manage(DemoTray { _handle: tray });
    Ok(())
}
