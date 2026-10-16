//! Native application menu: standard App / Edit / View / Window / Help structure, no browser
//! verbs (no Reload, no zoom, no Back), plus app-defined groups.
//!
//! Donors: ViewRight's macOS menu (Hide, Hide Others, Quit, Edit, Window), CodeRight's
//! `build_application_menu` (typed ids forwarded to the renderer exactly once) and HeardRight's
//! menu/tray gating. Custom items carry stable string ids; a click is delivered as the
//! `rightkit-shell://menu` event and to an optional Rust callback.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use tauri::menu::{Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu, SubmenuBuilder};
use tauri::{AppHandle, Wry};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum MenuEntry {
    Item {
        id: String,
        label: String,
        #[serde(default)]
        accelerator: Option<String>,
        #[serde(default = "yes")]
        enabled: bool,
    },
    Separator,
}

fn yes() -> bool {
    true
}

impl MenuEntry {
    pub fn item(id: &str, label: &str) -> Self {
        MenuEntry::Item {
            id: id.into(),
            label: label.into(),
            accelerator: None,
            enabled: true,
        }
    }
    pub fn accel(id: &str, label: &str, accelerator: &str) -> Self {
        MenuEntry::Item {
            id: id.into(),
            label: label.into(),
            accelerator: Some(accelerator.into()),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuGroup {
    pub label: String,
    pub entries: Vec<MenuEntry>,
}

#[derive(Debug, Clone, Default)]
pub struct MenuSpec {
    pub app_name: String,
    /// Extra top-level menus, placed after Edit/View.
    pub groups: Vec<MenuGroup>,
    /// Entries appended to Help.
    pub help: Vec<MenuEntry>,
    /// `Some((id, accelerator))` adds "Settings…" to the app menu (macOS) / File menu.
    pub settings: Option<(String, String)>,
}

pub struct BuiltMenu {
    pub menu: Menu<Wry>,
    pub items: HashMap<String, MenuItem<Wry>>,
}

fn add_entries(
    app: &AppHandle<Wry>,
    sub: &Submenu<Wry>,
    entries: &[MenuEntry],
    items: &mut HashMap<String, MenuItem<Wry>>,
) -> Result<(), String> {
    for e in entries {
        match e {
            MenuEntry::Separator => sub
                .append(&PredefinedMenuItem::separator(app).map_err(s)?)
                .map_err(s)?,
            MenuEntry::Item {
                id,
                label,
                accelerator,
                enabled,
            } => {
                let mi = MenuItem::with_id(app, id, label, *enabled, accelerator.as_deref())
                    .map_err(s)?;
                sub.append(&mi).map_err(s)?;
                items.insert(id.clone(), mi);
            }
        }
    }
    Ok(())
}

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

pub fn build(app: &AppHandle<Wry>, spec: &MenuSpec) -> Result<BuiltMenu, String> {
    let mut items = HashMap::new();
    let menu = Menu::new(app).map_err(s)?;

    #[cfg(target_os = "macos")]
    {
        let app_menu = SubmenuBuilder::new(app, &spec.app_name)
            .about(None)
            .build()
            .map_err(s)?;
        if let Some((id, accel)) = &spec.settings {
            app_menu
                .append(&PredefinedMenuItem::separator(app).map_err(s)?)
                .map_err(s)?;
            add_entries(
                app,
                &app_menu,
                &[MenuEntry::accel(id, "Settings\u{2026}", accel)],
                &mut items,
            )?;
        }
        app_menu
            .append(&PredefinedMenuItem::separator(app).map_err(s)?)
            .map_err(s)?;
        app_menu
            .append(&PredefinedMenuItem::services(app, None).map_err(s)?)
            .map_err(s)?;
        app_menu
            .append(&PredefinedMenuItem::separator(app).map_err(s)?)
            .map_err(s)?;
        app_menu
            .append(&PredefinedMenuItem::hide(app, None).map_err(s)?)
            .map_err(s)?;
        app_menu
            .append(&PredefinedMenuItem::hide_others(app, None).map_err(s)?)
            .map_err(s)?;
        app_menu
            .append(&PredefinedMenuItem::show_all(app, None).map_err(s)?)
            .map_err(s)?;
        app_menu
            .append(&PredefinedMenuItem::separator(app).map_err(s)?)
            .map_err(s)?;
        app_menu
            .append(
                &PredefinedMenuItem::quit(app, Some(&format!("Quit {}", spec.app_name)))
                    .map_err(s)?,
            )
            .map_err(s)?;
        menu.append(&app_menu).map_err(s)?;
    }
    #[cfg(not(target_os = "macos"))]
    {
        let file = SubmenuBuilder::new(app, "File").build().map_err(s)?;
        if let Some((id, accel)) = &spec.settings {
            add_entries(
                app,
                &file,
                &[
                    MenuEntry::accel(id, "Settings\u{2026}", accel),
                    MenuEntry::Separator,
                ],
                &mut items,
            )?;
        }
        file.append(&PredefinedMenuItem::close_window(app, None).map_err(s)?)
            .map_err(s)?;
        file.append(
            &PredefinedMenuItem::quit(app, Some(&format!("Exit {}", spec.app_name))).map_err(s)?,
        )
        .map_err(s)?;
        menu.append(&file).map_err(s)?;
    }

    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()
        .map_err(s)?;
    menu.append(&edit).map_err(s)?;

    #[cfg(target_os = "macos")]
    {
        // Fullscreen is the only View verb; reload/zoom are deliberately absent.
        let view = SubmenuBuilder::new(app, "View")
            .fullscreen()
            .build()
            .map_err(s)?;
        menu.append(&view).map_err(s)?;
    }

    for g in &spec.groups {
        let sub = SubmenuBuilder::new(app, &g.label).build().map_err(s)?;
        add_entries(app, &sub, &g.entries, &mut items)?;
        menu.append(&sub).map_err(s)?;
    }

    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .separator()
        .close_window()
        .build()
        .map_err(s)?;
    menu.append(&window).map_err(s)?;

    let help = SubmenuBuilder::new(app, "Help").build().map_err(s)?;
    add_entries(app, &help, &spec.help, &mut items)?;
    menu.append(&help).map_err(s)?;

    Ok(BuiltMenu { menu, items })
}

/// Flatten a menu to `{path -> label}` style rows for diagnostics and tests.
pub fn describe(menu: &Menu<Wry>) -> Vec<(String, String)> {
    fn walk(kind: &MenuItemKind<Wry>, prefix: &str, out: &mut Vec<(String, String)>) {
        match kind {
            MenuItemKind::Submenu(sm) => {
                let title = sm.text().unwrap_or_default();
                let path = format!("{prefix}{title}/");
                out.push((path.clone(), title));
                for child in sm.items().unwrap_or_default() {
                    walk(&child, &path, out);
                }
            }
            MenuItemKind::MenuItem(mi) => out.push((
                format!("{prefix}{}", mi.id().as_ref()),
                mi.text().unwrap_or_default(),
            )),
            MenuItemKind::Predefined(p) => out.push((
                format!("{prefix}(predefined)"),
                p.text().unwrap_or_default(),
            )),
            MenuItemKind::Check(c) => out.push((
                format!("{prefix}{}", c.id().as_ref()),
                c.text().unwrap_or_default(),
            )),
            MenuItemKind::Icon(i) => out.push((
                format!("{prefix}{}", i.id().as_ref()),
                i.text().unwrap_or_default(),
            )),
        }
    }
    let mut out = Vec::new();
    for item in menu.items().unwrap_or_default() {
        walk(&item, "", &mut out);
    }
    out
}
