//! System tray / menu-bar extra with a dynamic menu.
//!
//! Donors: CodeRight `os_shell/tray` (menu rebuilt from a snapshot, typed actions back to the
//! renderer, never a second authority), HeardRight `tray.rs` (state icons, left click opens the
//! app, Quit runs the ordered shutdown first). HeardRight's out-of-process macOS menu-bar helper
//! (a workaround for a Control Center identity bug) is an app packaging choice and stays in
//! HeardRight; this tray is the in-process item every other app uses.

use std::collections::HashSet;
use std::sync::{Arc, Mutex};

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
    /// Left click reveals the main window and never hides it (a tray that only brings the app forward).
    Show,
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
            left_click: if cfg!(target_os = "macos") {
                LeftClick::ShowMenu
            } else {
                LeftClick::ToggleWindow
            },
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

fn build_menu(
    app: &AppHandle<Wry>,
    spec_items: &[MenuEntry],
    show: &str,
    quit: &str,
) -> Result<(Menu<Wry>, HashSet<String>), String> {
    let ids: HashSet<String> = spec_items
        .iter()
        .filter_map(|e| {
            if let MenuEntry::Item { id, .. } = e {
                Some(id.clone())
            } else {
                None
            }
        })
        .collect();
    let menu = Menu::new(app).map_err(s)?;
    menu.append(&MenuItem::with_id(app, ID_SHOW, show, true, None::<&str>).map_err(s)?)
        .map_err(s)?;
    if !spec_items.is_empty() {
        menu.append(&PredefinedMenuItem::separator(app).map_err(s)?)
            .map_err(s)?;
    }
    for e in spec_items {
        match e {
            MenuEntry::Separator => menu
                .append(&PredefinedMenuItem::separator(app).map_err(s)?)
                .map_err(s)?,
            MenuEntry::Item {
                id,
                label,
                accelerator,
                enabled,
            } => {
                menu.append(
                    &MenuItem::with_id(app, id, label, *enabled, accelerator.as_deref())
                        .map_err(s)?,
                )
                .map_err(s)?;
            }
        }
    }
    menu.append(&PredefinedMenuItem::separator(app).map_err(s)?)
        .map_err(s)?;
    menu.append(&MenuItem::with_id(app, ID_QUIT, quit, true, None::<&str>).map_err(s)?)
        .map_err(s)?;
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

pub fn install(
    app: &AppHandle<Wry>,
    spec: &TraySpec,
    main_label: &str,
) -> Result<TrayIcon<Wry>, String> {
    let (menu, ids) = build_menu(app, &spec.items, &spec.show_label, &spec.quit_label)?;
    let mut b = TrayIconBuilder::with_id(TRAY_ID)
        .menu(&menu)
        .tooltip(&spec.tooltip)
        .show_menu_on_left_click(spec.left_click == LeftClick::ShowMenu);
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
                ID_SHOW => {
                    crate::windows::reveal_with(app, None, crate::lifecycle::RevealSource::Tray)
                }
                ID_QUIT => crate::lifecycle::request_quit(app, "tray"),
                other
                    if app.state::<ShellState>().tray.as_ref().is_some_and(|t| {
                        t.ids
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .contains(other)
                    }) =>
                {
                    let _ = app.emit(EVENT_TRAY, serde_json::json!({"kind": "menu", "id": other}));
                }
                _ => {}
            }
        })
        .on_tray_icon_event(move |tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                let app = tray.app_handle();
                let _ = app.emit(EVENT_TRAY, serde_json::json!({"kind": "click"}));
                if left == LeftClick::ToggleWindow {
                    crate::windows::toggle(app, &main);
                } else if left == LeftClick::Show {
                    crate::windows::reveal_with(app, None, crate::lifecycle::RevealSource::Tray);
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
        tray.set_icon(Some(Image::from_bytes(png).map_err(s)?.to_owned()))
            .map_err(s)?;
    }
    if let Some(items) = &u.items {
        let (menu, ids) = build_menu(app, items, &rt.show_label, &rt.quit_label)?;
        tray.set_menu(Some(menu)).map_err(s)?;
        *rt.ids.lock().unwrap_or_else(|e| e.into_inner()) = ids;
    }
    Ok(())
}

/// Options for an independently owned tray. Existing `install` & `TraySpec` are unchanged.
#[derive(Debug, Clone)]
pub struct TrayOptions {
    pub id: String,
    /// Include shell Show/Quit actions. `false` uses exactly `TraySpec::items`.
    pub standard_items: bool,
}

impl Default for TrayOptions {
    fn default() -> Self {
        Self {
            id: TRAY_ID.into(),
            standard_items: true,
        }
    }
}

impl TrayOptions {
    pub fn custom_only(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            standard_items: false,
        }
    }

    /// Resolve visible rows & reject ambiguous ids before allocating native resources.
    pub fn menu_items(&self, spec: &TraySpec) -> Result<Vec<MenuEntry>, String> {
        if self.id.is_empty() {
            return Err("tray id must not be empty".into());
        }
        let mut ids = HashSet::new();
        for entry in &spec.items {
            if let MenuEntry::Item { id, .. } = entry {
                if id.is_empty() || !ids.insert(id.clone()) {
                    return Err(format!("empty or duplicate tray item id: {id:?}"));
                }
                if self.standard_items && matches!(id.as_str(), ID_SHOW | ID_QUIT) {
                    return Err(format!("reserved tray item id: {id}"));
                }
            }
        }
        if !self.standard_items {
            return Ok(spec.items.clone());
        }
        let item = |id: &str, label: &str| MenuEntry::Item {
            id: id.into(),
            label: label.into(),
            accelerator: None,
            enabled: true,
        };
        let mut rows = vec![item(ID_SHOW, &spec.show_label)];
        if !spec.items.is_empty() {
            rows.push(MenuEntry::Separator);
        }
        rows.extend(spec.items.clone());
        rows.push(MenuEntry::Separator);
        rows.push(item(ID_QUIT, &spec.quit_label));
        Ok(rows)
    }

    // Tauri broadcasts menu events to all trays. Namespace native ids even when logical
    // ids match across trays, and keep these distinct from legacy shell menu ids.
    fn native_id(&self, logical: &str) -> String {
        format!("rk.tray.{}:{}:{}", self.id.len(), self.id, logical)
    }
}

pub type TrayMenuCallback = Arc<dyn Fn(&AppHandle<Wry>, &str) + Send + Sync>;

/// Retain this handle to update one tray; ids & callbacks never use the shell's primary tray.
pub struct TrayHandle {
    icon: TrayIcon<Wry>,
    options: TrayOptions,
    spec: Mutex<TraySpec>,
    ids: Arc<Mutex<std::collections::HashMap<String, String>>>,
}

fn independent_menu(
    app: &AppHandle<Wry>,
    spec: &TraySpec,
    options: &TrayOptions,
) -> Result<(Menu<Wry>, std::collections::HashMap<String, String>), String> {
    let rows = options.menu_items(spec)?;
    let menu = Menu::new(app).map_err(s)?;
    let mut ids = std::collections::HashMap::new();
    for row in rows {
        match row {
            MenuEntry::Separator => menu
                .append(&PredefinedMenuItem::separator(app).map_err(s)?)
                .map_err(s)?,
            MenuEntry::Item {
                id,
                label,
                accelerator,
                enabled,
            } => {
                let native = options.native_id(&id);
                menu.append(
                    &MenuItem::with_id(app, native.clone(), label, enabled, accelerator.as_deref())
                        .map_err(s)?,
                )
                .map_err(s)?;
                ids.insert(native, id);
            }
        }
    }
    Ok((menu, ids))
}

impl TrayHandle {
    pub fn id(&self) -> &str {
        &self.options.id
    }
    pub fn icon(&self) -> &TrayIcon<Wry> {
        &self.icon
    }

    /// Rebuild just this tray's menu. Custom-only mode & logical ids survive updates.
    pub fn update(&self, app: &AppHandle<Wry>, update: &TrayUpdate) -> Result<(), String> {
        let mut spec = self.spec.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(items) = &update.items {
            let mut next = spec.clone();
            next.items = items.clone();
            let (menu, ids) = independent_menu(app, &next, &self.options)?;
            self.icon.set_menu(Some(menu)).map_err(s)?;
            *self.ids.lock().unwrap_or_else(|e| e.into_inner()) = ids;
            *spec = next;
        }
        if let Some(tooltip) = &update.tooltip {
            self.icon.set_tooltip(Some(tooltip.as_str())).map_err(s)?;
        }
        #[cfg(target_os = "macos")]
        if let Some(title) = &update.title {
            self.icon.set_title(Some(title.as_str())).map_err(s)?;
        }
        if let Some(png) = &update.icon_png {
            self.icon
                .set_icon(Some(Image::from_bytes(png).map_err(s)?.to_owned()))
                .map_err(s)?;
        }
        Ok(())
    }
}

/// Install an additional tray without replacing the shell tray. Menu events retain logical
/// `id` & add `trayId`; `on_menu` also handles native actions without a renderer subscription.
/// Call during setup/on the main thread. A duplicate tray id returns an error.
pub fn install_with_options(
    app: &AppHandle<Wry>,
    spec: &TraySpec,
    main_label: &str,
    options: TrayOptions,
    on_menu: Option<TrayMenuCallback>,
) -> Result<TrayHandle, String> {
    if app.tray_by_id(&options.id).is_some() {
        return Err(format!("tray already installed: {}", options.id));
    }
    let (menu, initial_ids) = independent_menu(app, spec, &options)?;
    let ids = Arc::new(Mutex::new(initial_ids));
    let routed_ids = ids.clone();
    let tray_id = options.id.clone();
    let click_id = options.id.clone();
    let standard = options.standard_items;
    let left = spec.left_click;
    let main = main_label.to_owned();
    let mut builder = TrayIconBuilder::with_id(options.id.clone())
        .menu(&menu)
        .tooltip(&spec.tooltip)
        .show_menu_on_left_click(left == LeftClick::ShowMenu);
    #[cfg(target_os = "macos")]
    {
        builder = builder.icon_as_template(spec.template);
    }
    if let Some(png) = &spec.icon_png {
        builder = builder.icon(Image::from_bytes(png).map_err(s)?.to_owned());
    } else if let Some(icon) = app.default_window_icon() {
        builder = builder.icon(icon.clone());
    }
    let icon = builder
        .on_menu_event(move |app, event| {
            let logical = routed_ids
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(event.id.as_ref())
                .cloned();
            if let Some(id) = logical {
                match id.as_str() {
                    ID_SHOW if standard => {
                        crate::windows::reveal_with(app, None, crate::lifecycle::RevealSource::Tray)
                    }
                    ID_QUIT if standard => crate::lifecycle::request_quit(app, "tray"),
                    _ => {
                        if let Some(callback) = &on_menu {
                            callback(app, &id);
                        }
                        let _ = app.emit(
                            EVENT_TRAY,
                            serde_json::json!({"kind": "menu", "id": id, "trayId": tray_id}),
                        );
                    }
                }
            }
        })
        .on_tray_icon_event(move |tray, event| {
            if matches!(
                event,
                TrayIconEvent::Click {
                    button: MouseButton::Left,
                    button_state: MouseButtonState::Up,
                    ..
                }
            ) {
                let app = tray.app_handle();
                let _ = app.emit(
                    EVENT_TRAY,
                    serde_json::json!({"kind": "click", "trayId": click_id}),
                );
                if left == LeftClick::ToggleWindow {
                    crate::windows::toggle(app, &main);
                } else if left == LeftClick::Show {
                    crate::windows::reveal_with(app, None, crate::lifecycle::RevealSource::Tray);
                }
            }
        })
        .build(app)
        .map_err(s)?;
    Ok(TrayHandle {
        icon,
        options,
        spec: Mutex::new(spec.clone()),
        ids,
    })
}

#[cfg(test)]
mod independent_tests {
    use super::*;

    #[test]
    fn native_ids_are_unambiguous_and_separate_from_legacy_ids() {
        let a = TrayOptions::custom_only("x:y");
        let b = TrayOptions::custom_only("x");
        assert_ne!(a.native_id("z"), b.native_id("y:z"));
        assert_ne!(a.native_id(ID_SHOW), ID_SHOW);
        assert_eq!(a.native_id("stop"), a.native_id("stop"));
    }
}
