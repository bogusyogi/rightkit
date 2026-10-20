//! Native application menu: standard App / Edit / View / Window / Help structure, no browser
//! verbs (no Reload, no zoom, no Back), plus app-defined groups.
//!
//! Donors: ViewRight's macOS menu (Hide, Hide Others, Quit, Edit, Window), CodeRight's
//! `build_application_menu` (typed ids forwarded to the renderer exactly once) and HeardRight's
//! menu/tray gating. Custom items carry stable string ids; a click is delivered as the
//! `rightkit-shell://menu` event and to an optional Rust callback.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use tauri::menu::{
    CheckMenuItem, Menu, MenuItem, MenuItemKind, PredefinedMenuItem, Submenu, SubmenuBuilder,
};
use tauri::{AppHandle, Wry};

/// One row of an app-defined menu. Marked `#[non_exhaustive]`: match with a wildcard arm and build
/// rows with the constructors ([`MenuEntry::item`], [`MenuEntry::check`], [`MenuEntry::submenu`]
/// and the `with_` / `disabled` adjusters) so a later variant or field stays additive.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
#[non_exhaustive]
pub enum MenuEntry {
    Item {
        id: String,
        label: String,
        #[serde(default)]
        accelerator: Option<String>,
        #[serde(default = "yes")]
        enabled: bool,
    },
    /// A checkable item (a tick mark), clicked like an item. Its tick state is changed at runtime
    /// by id (`rk_shell_set_menu_item_checked`); a native click toggles the tick itself.
    #[non_exhaustive]
    Check {
        id: String,
        label: String,
        #[serde(default)]
        accelerator: Option<String>,
        #[serde(default = "yes")]
        enabled: bool,
        #[serde(default)]
        checked: bool,
    },
    /// A nested submenu, up to [`MAX_SUBMENU_DEPTH`] levels below a top-level menu.
    #[non_exhaustive]
    Submenu {
        label: String,
        #[serde(default)]
        entries: Vec<MenuEntry>,
    },
    Separator,
}

/// Deepest allowed `Submenu` nesting inside a group, Help or tray menu (the menu's own entries are
/// depth 1, so a submenu directly in them is level 1).
pub const MAX_SUBMENU_DEPTH: usize = 4;

/// Id of the app-menu quit item (macOS "Quit <app>", Windows/Linux File > "Exit <app>"). A click
/// is handled by the shell as `lifecycle::request_quit` and is not emitted as a menu event.
pub const ID_QUIT: &str = "rk.menu.quit";

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
    /// A check item, enabled, with no accelerator.
    pub fn check(id: &str, label: &str, checked: bool) -> Self {
        MenuEntry::Check {
            id: id.into(),
            label: label.into(),
            accelerator: None,
            enabled: true,
            checked,
        }
    }
    /// A nested submenu (see [`MAX_SUBMENU_DEPTH`]).
    pub fn submenu(label: &str, entries: Vec<MenuEntry>) -> Self {
        MenuEntry::Submenu {
            label: label.into(),
            entries,
        }
    }
    /// Set the accelerator of an item or check row; other rows are returned unchanged.
    pub fn with_accelerator(mut self, accel: &str) -> Self {
        if let MenuEntry::Item { accelerator, .. } | MenuEntry::Check { accelerator, .. } =
            &mut self
        {
            *accelerator = Some(accel.into());
        }
        self
    }
    /// Start an item or check row disabled; other rows are returned unchanged.
    pub fn disabled(mut self) -> Self {
        if let MenuEntry::Item { enabled, .. } | MenuEntry::Check { enabled, .. } = &mut self {
            *enabled = false;
        }
        self
    }
}

/// Every item and check id in `entries`, nested submenus included, in menu order. Run
/// [`validate_entries`] first when the entries come from outside: this recurses without a bound.
pub fn entry_ids(entries: &[MenuEntry]) -> Vec<String> {
    let mut out = Vec::new();
    for e in entries {
        match e {
            MenuEntry::Item { id, .. } | MenuEntry::Check { id, .. } => out.push(id.clone()),
            MenuEntry::Submenu { entries, .. } => out.extend(entry_ids(entries)),
            _ => {}
        }
    }
    out
}

/// Refuse submenus nested deeper than [`MAX_SUBMENU_DEPTH`]. The walk stops at the first row past
/// the limit, so a hostile nest costs no more than the limit.
pub fn validate_entries(entries: &[MenuEntry]) -> Result<(), String> {
    fn depth_ok(entries: &[MenuEntry], depth: usize) -> bool {
        entries.iter().all(|e| match e {
            MenuEntry::Submenu { entries, .. } => {
                depth <= MAX_SUBMENU_DEPTH && depth_ok(entries, depth + 1)
            }
            _ => true,
        })
    }
    if depth_ok(entries, 1) {
        Ok(())
    } else {
        Err(format!("submenus nest deeper than {MAX_SUBMENU_DEPTH}"))
    }
}

/// Reject what would build an ambiguous or unbounded menu: an empty id, [`ID_QUIT`] (the shell's
/// own quit item), an id used twice anywhere in `groups` / `help` / the settings item / `overrides`
/// (a runtime update by id would hit only one), an override role given twice, or submenus nested
/// deeper than [`MAX_SUBMENU_DEPTH`]. [`build`] runs this first.
pub fn validate(spec: &MenuSpec) -> Result<(), String> {
    let mut all: Vec<String> = Vec::new();
    if let Some((id, _)) = &spec.settings {
        all.push(id.clone());
    }
    for g in &spec.groups {
        validate_entries(&g.entries).map_err(|e| format!("menu {}: {e}", g.label))?;
        all.extend(entry_ids(&g.entries));
    }
    validate_entries(&spec.help).map_err(|e| format!("help menu: {e}"))?;
    all.extend(entry_ids(&spec.help));
    let mut roles = HashSet::new();
    for o in &spec.overrides {
        if !roles.insert(o.role) {
            return Err(format!("menu override {:?} given twice", o.role));
        }
        all.push(o.id.clone());
    }
    let mut seen = HashSet::new();
    for id in all {
        if id.trim().is_empty() {
            return Err("menu item with an empty id".into());
        }
        if id == ID_QUIT {
            return Err(format!("menu item id {ID_QUIT} is reserved for the shell"));
        }
        if !seen.insert(id.clone()) {
            return Err(format!("duplicate menu item id {id}"));
        }
    }
    Ok(())
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
    /// Replace native Undo / Redo / Quit / Close Window with app-id items that emit menu events.
    pub overrides: Vec<MenuOverride>,
}

/// The native items an app may route to itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum OverrideRole {
    Undo,
    Redo,
    /// Replaces the shell's own quit item ([`ID_QUIT`]), so the shell's ordered quit
    /// (`lifecycle::request_quit`) no longer runs from the menu: the app calls its quit command.
    Quit,
    /// Windows/Linux: replaces File > Close Window (and Window > Close Window is dropped, so the id
    /// exists once). macOS has no File menu here: replaces Window > Close Window.
    CloseWindow,
}

/// An app-routed replacement for a native item, wherever the shell would place that item. The
/// click arrives as `rightkit-shell://menu` with `id`. `label` / `accelerator` default to the
/// native ones (Undo `CmdOrCtrl+Z`, Redo `CmdOrCtrl+Shift+Z`, Quit `CmdOrCtrl+Q` on macOS and none
/// elsewhere, Close Window `CmdOrCtrl+W`). App items have no responder-chain enablement: the item
/// starts enabled and the app drives it with `rk_shell_set_menu_state`.
///
/// An app that overrides Undo/Redo owns their enablement and behaviour: when focus is in a plain
/// text input it must forward to the webview's native undo/redo instead of its own history, or the
/// input's undo is stolen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MenuOverride {
    pub role: OverrideRole,
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub accelerator: Option<String>,
}

impl MenuOverride {
    pub fn new(role: OverrideRole, id: &str) -> Self {
        Self {
            role,
            id: id.into(),
            label: None,
            accelerator: None,
        }
    }
    pub fn label(mut self, label: &str) -> Self {
        self.label = Some(label.into());
        self
    }
    pub fn accelerator(mut self, accelerator: &str) -> Self {
        self.accelerator = Some(accelerator.into());
        self
    }
}

/// The label and accelerator the native item of `role` carries; an override keeps them unless it
/// sets its own.
pub fn role_defaults(
    role: OverrideRole,
    app_name: &str,
    mac: bool,
) -> (String, Option<&'static str>) {
    match role {
        OverrideRole::Undo => ("Undo".into(), Some("CmdOrCtrl+Z")),
        OverrideRole::Redo => ("Redo".into(), Some("CmdOrCtrl+Shift+Z")),
        OverrideRole::Quit if mac => (format!("Quit {app_name}"), Some("CmdOrCtrl+Q")),
        OverrideRole::Quit => (format!("Exit {app_name}"), None),
        OverrideRole::CloseWindow => ("Close Window".into(), Some("CmdOrCtrl+W")),
    }
}

/// The app-id item that replaces the native `role` item, or `None` when the spec keeps the native
/// one. Pure, so the replacement rules are testable without a native menu.
pub fn override_entry(spec: &MenuSpec, role: OverrideRole, mac: bool) -> Option<MenuEntry> {
    let o = spec.overrides.iter().find(|o| o.role == role)?;
    let (label, accelerator) = role_defaults(role, &spec.app_name, mac);
    Some(MenuEntry::Item {
        id: o.id.clone(),
        label: o.label.clone().unwrap_or(label),
        accelerator: o
            .accelerator
            .clone()
            .or_else(|| accelerator.map(Into::into)),
        enabled: true,
    })
}

#[non_exhaustive]
pub struct BuiltMenu {
    pub menu: Menu<Wry>,
    pub items: HashMap<String, MenuItem<Wry>>,
    /// Check items by id (kept apart from `items`: a different Tauri type).
    pub checks: HashMap<String, CheckMenuItem<Wry>>,
}

/// Every menu item the shell built, by id, for runtime state changes.
#[derive(Default)]
struct MenuHandles {
    items: HashMap<String, MenuItem<Wry>>,
    checks: HashMap<String, CheckMenuItem<Wry>>,
}

fn add_entries(
    app: &AppHandle<Wry>,
    sub: &Submenu<Wry>,
    entries: &[MenuEntry],
    handles: &mut MenuHandles,
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
                handles.items.insert(id.clone(), mi);
            }
            MenuEntry::Check {
                id,
                label,
                accelerator,
                enabled,
                checked,
            } => {
                let ci = CheckMenuItem::with_id(
                    app,
                    id,
                    label,
                    *enabled,
                    *checked,
                    accelerator.as_deref(),
                )
                .map_err(s)?;
                sub.append(&ci).map_err(s)?;
                handles.checks.insert(id.clone(), ci);
            }
            MenuEntry::Submenu { label, entries } => {
                let child = SubmenuBuilder::new(app, label).build().map_err(s)?;
                add_entries(app, &child, entries, handles)?;
                sub.append(&child).map_err(s)?;
            }
        }
    }
    Ok(())
}

fn s<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

pub fn build(app: &AppHandle<Wry>, spec: &MenuSpec) -> Result<BuiltMenu, String> {
    validate(spec)?;
    let mac = cfg!(target_os = "macos");
    let mut handles = MenuHandles::default();
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
                &mut handles,
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
        // Not the predefined quit: that terminates without the shell's ordered quit, so with
        // close-to-tray on a Cmd+Q would hide the window instead of draining. The item routes to
        // `lifecycle::request_quit` (see `ID_QUIT` and `lifecycle::on_menu_event`). An app
        // override replaces it with the app's own id (the app then owns the quit).
        match override_entry(spec, OverrideRole::Quit, mac) {
            Some(quit) => add_entries(app, &app_menu, &[quit], &mut handles)?,
            None => app_menu
                .append(
                    &MenuItem::with_id(
                        app,
                        ID_QUIT,
                        format!("Quit {}", spec.app_name),
                        true,
                        Some("CmdOrCtrl+Q"),
                    )
                    .map_err(s)?,
                )
                .map_err(s)?,
        }
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
                &mut handles,
            )?;
        }
        match override_entry(spec, OverrideRole::CloseWindow, mac) {
            Some(close) => add_entries(app, &file, &[close], &mut handles)?,
            None => file
                .append(&PredefinedMenuItem::close_window(app, None).map_err(s)?)
                .map_err(s)?,
        }
        // Routed to `lifecycle::request_quit`, as on macOS (see `ID_QUIT`), unless overridden.
        match override_entry(spec, OverrideRole::Quit, mac) {
            Some(quit) => add_entries(app, &file, &[quit], &mut handles)?,
            None => file
                .append(
                    &MenuItem::with_id(
                        app,
                        ID_QUIT,
                        format!("Exit {}", spec.app_name),
                        true,
                        None::<&str>,
                    )
                    .map_err(s)?,
                )
                .map_err(s)?,
        }
        menu.append(&file).map_err(s)?;
    }

    let edit = SubmenuBuilder::new(app, "Edit").build().map_err(s)?;
    match override_entry(spec, OverrideRole::Undo, mac) {
        Some(undo) => add_entries(app, &edit, &[undo], &mut handles)?,
        None => edit
            .append(&PredefinedMenuItem::undo(app, None).map_err(s)?)
            .map_err(s)?,
    }
    match override_entry(spec, OverrideRole::Redo, mac) {
        Some(redo) => add_entries(app, &edit, &[redo], &mut handles)?,
        None => edit
            .append(&PredefinedMenuItem::redo(app, None).map_err(s)?)
            .map_err(s)?,
    }
    edit.append(&PredefinedMenuItem::separator(app).map_err(s)?)
        .map_err(s)?;
    edit.append(&PredefinedMenuItem::cut(app, None).map_err(s)?)
        .map_err(s)?;
    edit.append(&PredefinedMenuItem::copy(app, None).map_err(s)?)
        .map_err(s)?;
    edit.append(&PredefinedMenuItem::paste(app, None).map_err(s)?)
        .map_err(s)?;
    edit.append(&PredefinedMenuItem::select_all(app, None).map_err(s)?)
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
        add_entries(app, &sub, &g.entries, &mut handles)?;
        menu.append(&sub).map_err(s)?;
    }

    let window = SubmenuBuilder::new(app, "Window")
        .minimize()
        .maximize()
        .build()
        .map_err(s)?;
    // Windows/Linux: an overridden Close Window already sits in File (one item per id).
    let close = override_entry(spec, OverrideRole::CloseWindow, mac);
    if mac || close.is_none() {
        window
            .append(&PredefinedMenuItem::separator(app).map_err(s)?)
            .map_err(s)?;
        match close {
            Some(close) => add_entries(app, &window, &[close], &mut handles)?,
            None => window
                .append(&PredefinedMenuItem::close_window(app, None).map_err(s)?)
                .map_err(s)?,
        }
    }
    menu.append(&window).map_err(s)?;

    let help = SubmenuBuilder::new(app, "Help").build().map_err(s)?;
    add_entries(app, &help, &spec.help, &mut handles)?;
    menu.append(&help).map_err(s)?;

    Ok(BuiltMenu {
        menu,
        items: handles.items,
        checks: handles.checks,
    })
}

/// A runtime change to one menu item, by id. `None` leaves that property alone.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct MenuItemState {
    pub id: String,
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Only a `Check` entry has a tick.
    #[serde(default)]
    pub checked: Option<bool>,
}

impl MenuItemState {
    pub fn new(id: &str) -> Self {
        Self {
            id: id.into(),
            enabled: None,
            checked: None,
        }
    }
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = Some(enabled);
        self
    }
    pub fn with_checked(mut self, checked: bool) -> Self {
        self.checked = Some(checked);
        self
    }
}

/// Check one update against the known ids before touching a native item: the id must exist and
/// `checked` needs a check item. `Err` carries the reason.
pub fn check_state(
    update: &MenuItemState,
    is_item: impl Fn(&str) -> bool,
    is_check: impl Fn(&str) -> bool,
) -> Result<(), String> {
    let id = update.id.as_str();
    if !is_item(id) && !is_check(id) {
        return Err(format!("no menu item {id}"));
    }
    if update.checked.is_some() && !is_check(id) {
        return Err(format!("menu item {id} is not a check item"));
    }
    Ok(())
}

/// Apply `updates` to the shell-built items. Every valid update is applied even when another is
/// refused; the refusals come back joined in one error.
pub fn apply_state(
    items: &HashMap<String, MenuItem<Wry>>,
    checks: &HashMap<String, CheckMenuItem<Wry>>,
    updates: &[MenuItemState],
) -> Result<(), String> {
    let mut errors: Vec<String> = Vec::new();
    for u in updates {
        if let Err(e) = check_state(u, |id| items.contains_key(id), |id| checks.contains_key(id)) {
            errors.push(e);
            continue;
        }
        if let Some(on) = u.enabled {
            let r = match (items.get(&u.id), checks.get(&u.id)) {
                (Some(i), _) => i.set_enabled(on),
                (_, Some(c)) => c.set_enabled(on),
                _ => Ok(()),
            };
            if let Err(e) = r {
                errors.push(format!("{}: {e}", u.id));
            }
        }
        if let (Some(on), Some(c)) = (u.checked, checks.get(&u.id)) {
            if let Err(e) = c.set_checked(on) {
                errors.push(format!("{}: {e}", u.id));
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
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
