//! System tray / menu-bar extra with a dynamic menu.
//!
//! Donors: CodeRight `os_shell/tray` (menu rebuilt from a snapshot, typed actions back to the
//! renderer, never a second authority), HeardRight `tray.rs` (state icons, left click opens the
//! app, Quit runs the ordered shutdown first). HeardRight's out-of-process macOS menu-bar helper
//! (a workaround for a Control Center identity bug) is an app packaging choice and stays in
//! HeardRight; this tray is the in-process item every other app uses.

use std::collections::HashSet;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::image::Image;
use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Wry};

use crate::menu::MenuEntry;
use crate::{ShellState, EVENT_TRAY};

pub const TRAY_ID: &str = "rk-shell-tray";
pub const ID_SHOW: &str = "rk.tray.show";
pub const ID_QUIT: &str = "rk.tray.quit";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LeftClick {
    /// Left click toggles the main window (Windows/Linux convention).
    ToggleWindow,
    /// Left click opens the menu (macOS convention).
    ShowMenu,
}

#[derive(Debug, Clone)]
pub struct TraySpec {
    pub tooltip: String,
    /// PNG bytes. `None` uses the app's default window icon.
    pub icon_png: Option<Vec<u8>>,
    /// Render the icon as a macOS template (monochrome, tinted by the system).
    pub template: bool,
    pub items: Vec<MenuEntry>,
    pub left_click: LeftClick,
    pub show_label: String,
    pub quit_label: String,
}

impl Default for TraySpec {
    fn default() -> Self {
        Self {
            tooltip: String::new(),
            icon_png: None,
            template: false,
            items: Vec::new(),
            left_click: if cfg!(target_os = "macos") { LeftClick::ShowMenu } else { LeftClick::ToggleWindow },
            show_label: "Show Window".into(),
            quit_label: "Quit".into(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrayUpdate {
    pub tooltip: Option<String>,
    /// macOS menu-bar title text next to the icon.
    pub title: Option<String>,
    pub icon_png: Option<Vec<u8>>,
    pub items: Option<Vec<MenuEntry>>,
}

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

fn build_menu(app: &AppHandle<Wry>, spec_items: &[MenuEntry], show: &str, quit: &str) -> Result<(Menu<Wry>, HashSet<String>), String> {
    let ids: HashSet<String> = spec_items.iter().filter_map(|e| if let MenuEntry::Item { id, .. } = e { Some(id.clone()) } else { None }).collect();
    let menu = Menu::new(app).map_err(s)?;
    menu.append(&MenuItem::with_id(app, ID_SHOW, show, true, None::<&str>).map_err(s)?).map_err(s)?;
    if !spec_items.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app).map_err(s)?).map_err(s)?;
    }
    for e in spec_items {
        match e {
            MenuEntry::Separator => menu.append(&PredefinedMenuItem::separator(app).map_err(s)?).map_err(s)?,
            MenuEntry::Item { id, label, accelerator, enabled } => {
                menu.append(&MenuItem::with_id(app, id, label, *enabled, accelerator.as_deref()).map_err(s)?).map_err(s)?;
            }
        }
    }
    menu.append(&PredefinedMenuItem::separator(app).map_err(s)?).map_err(s)?;
    menu.append(&MenuItem::with_id(app, ID_QUIT, quit, true, None::<&str>).map_err(s)?).map_err(s)?;
    Ok((menu, ids))
}

pub struct TrayRuntime {
    pub icon: Mutex<Option<TrayIcon<Wry>>>,
    /// Ids of the items currently in the tray menu. Tauri delivers EVERY menu event (including the
    /// app menu's) to a tray's handler, so only these ids are tray actions.
    pub ids: Mutex<HashSet<String>>,
    pub show_label: String,
    pub quit_label: String,
}

pub fn install(app: &AppHandle<Wry>, spec: &TraySpec, main_label: &str) -> Result<TrayIcon<Wry>, String> {
    let (menu, ids) = build_menu(app, &spec.items, &spec.show_label, &spec.quit_label)?;
    let mut b = TrayIconBuilder::with_id(TRAY_ID).menu(&menu).tooltip(&spec.tooltip).show_menu_on_left_click(spec.left_click == LeftClick::ShowMenu);
    #[cfg(target_os = "macos")]
    {
        b = b.icon_as_template(spec.template);
    }
    if let Some(png) = &spec.icon_png {
        b = b.icon(Image::from_bytes(png).map_err(s)?.to_owned());
    } else if let Some(icon) = app.default_window_icon() {
        b = b.icon(icon.clone());
    }
    let main = main_label.to_owned();
    let left = spec.left_click;
    let tray = b
        .on_menu_event(|app, event| {
            let id = event.id.as_ref();
            match id {
                ID_SHOW => crate::windows::reveal(app, None),
                ID_QUIT => crate::lifecycle::quit(app),
                other if app.state::<ShellState>().tray.as_ref().is_some_and(|t| t.ids.lock().unwrap_or_else(|e| e.into_inner()).contains(other)) => {
                    let _ = app.emit(EVENT_TRAY, serde_json::json!({"kind": "menu", "id": other}));
                }
                _ => {}
            }
        })
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                let app = tray.app_handle();
                let _ = app.emit(EVENT_TRAY, serde_json::json!({"kind": "click"}));
                if left == LeftClick::ToggleWindow {
                    crate::windows::toggle(app, &main);
                }
            }
        })
        .build(app)
        .map_err(s)?;
    if let Some(rt) = &app.state::<ShellState>().tray {
        *rt.ids.lock().unwrap_or_else(|e| e.into_inner()) = ids;
    }
    Ok(tray)
}

pub fn update(app: &AppHandle<Wry>, u: &TrayUpdate) -> Result<(), String> {
    let state = app.state::<ShellState>();
    let rt = state.tray.as_ref().ok_or("no tray installed")?;
    let guard = rt.icon.lock().unwrap_or_else(|e| e.into_inner());
    let tray = guard.as_ref().ok_or("tray not ready")?;
    if let Some(t) = &u.tooltip {
        tray.set_tooltip(Some(t.as_str())).map_err(s)?;
    }
    #[cfg(target_os = "macos")]
    if let Some(t) = &u.title {
        tray.set_title(Some(t.as_str())).map_err(s)?;
    }
    if let Some(png) = &u.icon_png {
        tray.set_icon(Some(Image::from_bytes(png).map_err(s)?.to_owned())).map_err(s)?;
    }
    if let Some(items) = &u.items {
        let (menu, ids) = build_menu(app, items, &rt.show_label, &rt.quit_label)?;
        tray.set_menu(Some(menu)).map_err(s)?;
        *rt.ids.lock().unwrap_or_else(|e| e.into_inner()) = ids;
    }
    Ok(())
}
