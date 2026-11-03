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

/// Id of the app-menu quit item (macOS "Quit `<app>`", Windows/Linux File > "Exit `<app>`"). A click
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
    let mut labels = HashSet::new();
    for g in &spec.groups {
        if !labels.insert(g.label.as_str()) {
            return Err(format!("menu group {} given twice", g.label));
        }
    }
    for slot in &spec.order {
        if let MenuSlot::Group(label) = slot {
            if !labels.contains(label.as_str()) {
                return Err(format!("menu order names the unknown group {label}"));
            }
        }
    }
    let mut placed = HashSet::new();
    for p in &spec.placements {
        if !placed.insert(p.menu) {
            return Err(format!("menu placement {:?} given twice", p.menu));
        }
        for list in [&p.start, &p.end] {
            validate_entries(list).map_err(|e| format!("menu {:?}: {e}", p.menu))?;
            all.extend(entry_ids(list));
        }
        for item in &p.omit {
            if !item.lives_in(p.menu) {
                return Err(format!("{item:?} is not an item of the {:?} menu", p.menu));
            }
        }
    }
    if spec.omits_menu(BuiltinMenu::App) {
        return Err("the App menu holds Quit and cannot be omitted".into());
    }
    if spec.omits_menu(BuiltinMenu::File)
        && (spec.settings.is_some()
            || spec
                .overrides
                .iter()
                .any(|o| matches!(o.role, OverrideRole::Quit | OverrideRole::CloseWindow)))
    {
        return Err(
            "the File menu cannot be omitted while settings or a Quit / Close Window override would live in it"
                .into(),
        );
    }
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

/// An operating system a menu group may be limited to ([`MenuGroup::platforms`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum MenuPlatform {
    Macos,
    Windows,
    Linux,
}

impl MenuPlatform {
    /// The operating system this build runs on.
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            MenuPlatform::Macos
        } else if cfg!(windows) {
            MenuPlatform::Windows
        } else {
            MenuPlatform::Linux
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuGroup {
    pub label: String,
    pub entries: Vec<MenuEntry>,
    /// The operating systems that get this menu. Empty (the default) means every one. The ids in a
    /// gated-off group still count for [`validate`], so one spec is valid on every OS.
    #[serde(default)]
    pub platforms: Vec<MenuPlatform>,
}

impl MenuGroup {
    /// A top-level menu on every operating system.
    pub fn new(label: &str, entries: Vec<MenuEntry>) -> Self {
        Self {
            label: label.into(),
            entries,
            platforms: Vec::new(),
        }
    }
    /// Limit the menu to `platforms` (replaces an earlier limit).
    pub fn only_on(mut self, platforms: &[MenuPlatform]) -> Self {
        self.platforms = platforms.to_vec();
        self
    }
    /// Whether this OS gets the menu.
    pub fn applies(&self, platform: MenuPlatform) -> bool {
        self.platforms.is_empty() || self.platforms.contains(&platform)
    }
}

/// A menu the shell builds itself. `App` exists on macOS only (the application menu), `File` on
/// Windows and Linux only; a placement or omission naming the menu of another OS is ignored there,
/// so one spec serves every OS. `View` is the macOS menu holding Fullscreen; on Windows and Linux
/// it is built only when a [`MenuPlacement`] gives it entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum BuiltinMenu {
    App,
    File,
    Edit,
    View,
    Window,
    Help,
}

/// A shell-built extra an app may drop from one built-in menu ([`MenuPlacement::omit`]). The
/// editing items (Undo, Redo, Cut, Copy, Paste, Select All), the shell's quit item and an
/// overridden item are never droppable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[non_exhaustive]
pub enum BuiltinItem {
    /// App menu: About.
    About,
    /// App menu: Services.
    Services,
    /// App menu: Hide, Hide Others and Show All.
    Hide,
    /// View menu: Enter Full Screen.
    Fullscreen,
    /// Window menu: Minimize.
    Minimize,
    /// Window menu: Maximize.
    Maximize,
    /// File menu (Windows, Linux) or Window menu (macOS): Close Window.
    CloseWindow,
}

impl BuiltinItem {
    /// Whether the shell puts this item into `menu`.
    pub fn lives_in(self, menu: BuiltinMenu) -> bool {
        match self {
            BuiltinItem::About | BuiltinItem::Services | BuiltinItem::Hide => {
                menu == BuiltinMenu::App
            }
            BuiltinItem::Fullscreen => menu == BuiltinMenu::View,
            BuiltinItem::Minimize | BuiltinItem::Maximize => menu == BuiltinMenu::Window,
            BuiltinItem::CloseWindow => matches!(menu, BuiltinMenu::File | BuiltinMenu::Window),
        }
    }
}

/// Entries an app adds to a shell-built menu, and the shell extras it drops from it. The entries
/// are ordinary [`MenuEntry`] rows (checks and nested submenus included) and are built by the
/// shell, so `rk_shell_set_menu_state` and `rk_shell_set_menu_item_checked` reach them by id.
///
/// `start` goes above the shell's items. `end` goes below them, except in the App menu (macOS) and
/// the File menu (Windows, Linux), where it goes just above the quit item. In Help, `start` and
/// `end` surround [`MenuSpec::help`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MenuPlacement {
    pub menu: BuiltinMenu,
    #[serde(default)]
    pub start: Vec<MenuEntry>,
    #[serde(default)]
    pub end: Vec<MenuEntry>,
    #[serde(default)]
    pub omit: Vec<BuiltinItem>,
}

impl MenuPlacement {
    pub fn new(menu: BuiltinMenu) -> Self {
        Self {
            menu,
            start: Vec::new(),
            end: Vec::new(),
            omit: Vec::new(),
        }
    }
    pub fn with_start(mut self, entries: Vec<MenuEntry>) -> Self {
        self.start = entries;
        self
    }
    pub fn with_end(mut self, entries: Vec<MenuEntry>) -> Self {
        self.end = entries;
        self
    }
    pub fn omitting(mut self, items: &[BuiltinItem]) -> Self {
        self.omit = items.to_vec();
        self
    }
}

/// One position in the menu bar ([`MenuSpec::order`]).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MenuSlot {
    App,
    File,
    Edit,
    View,
    Window,
    Help,
    /// An app group, by its [`MenuGroup::label`].
    Group(String),
}

#[derive(Debug, Clone, Default)]
pub struct MenuSpec {
    pub app_name: String,
    /// Extra top-level menus, placed after Edit/View (see [`MenuSpec::order`]).
    pub groups: Vec<MenuGroup>,
    /// Entries appended to Help.
    pub help: Vec<MenuEntry>,
    /// `Some((id, accelerator))` adds "Settings…" to the app menu (macOS) / File menu.
    pub settings: Option<(String, String)>,
    /// Replace native Undo / Redo / Quit / Close Window with app-id items that emit menu events.
    pub overrides: Vec<MenuOverride>,
    /// The order of the top-level menus. The slots listed come first, in the listed order; every
    /// other menu follows in the default order (App or File, Edit, View, the groups, Window,
    /// Help). A slot that does not exist on this OS is skipped. On macOS the App menu is always
    /// first, as the OS requires. Empty keeps the default order.
    pub order: Vec<MenuSlot>,
    /// Built-in menus the shell does not build. The App menu on macOS cannot be omitted (it holds
    /// Quit); the File menu cannot be omitted while `settings` or a Quit / Close Window override
    /// would live in it.
    pub omit_menus: Vec<BuiltinMenu>,
    /// Entries to add to, and extras to drop from, the built-in menus; one per menu.
    pub placements: Vec<MenuPlacement>,
}

impl MenuSpec {
    fn placement(&self, menu: BuiltinMenu) -> Option<&MenuPlacement> {
        self.placements.iter().find(|p| p.menu == menu)
    }
    fn omits(&self, menu: BuiltinMenu, item: BuiltinItem) -> bool {
        self.placement(menu).is_some_and(|p| p.omit.contains(&item))
    }
    fn omits_menu(&self, menu: BuiltinMenu) -> bool {
        self.omit_menus.contains(&menu)
    }
}

/// The top-level order that results from `order` over the menus actually built, in their default
/// sequence (`default`). Pure, so the ordering rules are checkable without a native menu.
pub fn resolve_order(order: &[MenuSlot], default: &[MenuSlot], mac: bool) -> Vec<MenuSlot> {
    let mut out: Vec<MenuSlot> = Vec::new();
    if mac && default.first() == Some(&MenuSlot::App) {
        out.push(MenuSlot::App);
    }
    for slot in order {
        if default.contains(slot) && !out.contains(slot) {
            out.push(slot.clone());
        }
    }
    for slot in default {
        if !out.contains(slot) {
            out.push(slot.clone());
        }
    }
    out
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

/// Append a separator unless the menu is empty or already ends in one. `open` is true then.
fn separator(app: &AppHandle<Wry>, sub: &Submenu<Wry>, open: &mut bool) -> Result<(), String> {
    if !*open {
        sub.append(&PredefinedMenuItem::separator(app).map_err(s)?)
            .map_err(s)?;
        *open = true;
    }
    Ok(())
}

/// Append app entries as one block: separated from the items before it, and `open` ends false.
fn block(
    app: &AppHandle<Wry>,
    sub: &Submenu<Wry>,
    entries: &[MenuEntry],
    open: &mut bool,
    handles: &mut MenuHandles,
) -> Result<(), String> {
    if entries.is_empty() {
        return Ok(());
    }
    add_entries(app, sub, entries, handles)?;
    *open = matches!(entries.last(), Some(MenuEntry::Separator));
    Ok(())
}

pub fn build(app: &AppHandle<Wry>, spec: &MenuSpec) -> Result<BuiltMenu, String> {
    validate(spec)?;
    let mac = cfg!(target_os = "macos");
    let platform = MenuPlatform::current();
    let mut handles = MenuHandles::default();
    let menu = Menu::new(app).map_err(s)?;
    // Built in the default sequence, then ordered by `spec.order`.
    let mut built: Vec<(MenuSlot, Submenu<Wry>)> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        use BuiltinItem as I;
        use BuiltinMenu as M;
        let place = spec.placement(M::App);
        let mut builder = SubmenuBuilder::new(app, &spec.app_name);
        if !spec.omits(M::App, I::About) {
            builder = builder.about(None);
        }
        let app_menu = builder.build().map_err(s)?;
        let mut open = app_menu.items().map_err(s)?.is_empty();
        if let Some(p) = place {
            block(app, &app_menu, &p.start, &mut open, &mut handles)?;
        }
        if let Some((id, accel)) = &spec.settings {
            separator(app, &app_menu, &mut open)?;
            add_entries(
                app,
                &app_menu,
                &[MenuEntry::accel(id, "Settings\u{2026}", accel)],
                &mut handles,
            )?;
            open = false;
        }
        if !spec.omits(M::App, I::Services) {
            separator(app, &app_menu, &mut open)?;
            app_menu
                .append(&PredefinedMenuItem::services(app, None).map_err(s)?)
                .map_err(s)?;
            open = false;
        }
        if !spec.omits(M::App, I::Hide) {
            separator(app, &app_menu, &mut open)?;
            app_menu
                .append(&PredefinedMenuItem::hide(app, None).map_err(s)?)
                .map_err(s)?;
            app_menu
                .append(&PredefinedMenuItem::hide_others(app, None).map_err(s)?)
                .map_err(s)?;
            app_menu
                .append(&PredefinedMenuItem::show_all(app, None).map_err(s)?)
                .map_err(s)?;
            open = false;
        }
        if let Some(p) = place {
            if !p.end.is_empty() {
                separator(app, &app_menu, &mut open)?;
                block(app, &app_menu, &p.end, &mut open, &mut handles)?;
            }
        }
        separator(app, &app_menu, &mut open)?;
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
        built.push((MenuSlot::App, app_menu));
    }
    #[cfg(not(target_os = "macos"))]
    if !spec.omits_menu(BuiltinMenu::File) {
        use BuiltinItem as I;
        use BuiltinMenu as M;
        let place = spec.placement(M::File);
        let file = SubmenuBuilder::new(app, "File").build().map_err(s)?;
        let mut open = true;
        if let Some(p) = place {
            block(app, &file, &p.start, &mut open, &mut handles)?;
        }
        if let Some((id, accel)) = &spec.settings {
            separator(app, &file, &mut open)?;
            add_entries(
                app,
                &file,
                &[MenuEntry::accel(id, "Settings\u{2026}", accel)],
                &mut handles,
            )?;
            open = false;
        }
        if let Some(p) = place {
            if !p.end.is_empty() {
                separator(app, &file, &mut open)?;
                block(app, &file, &p.end, &mut open, &mut handles)?;
            }
        }
        if !open {
            separator(app, &file, &mut open)?;
        }
        match override_entry(spec, OverrideRole::CloseWindow, mac) {
            Some(close) => add_entries(app, &file, &[close], &mut handles)?,
            None if !spec.omits(M::File, I::CloseWindow) => file
                .append(&PredefinedMenuItem::close_window(app, None).map_err(s)?)
                .map_err(s)?,
            None => {}
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
        built.push((MenuSlot::File, file));
    }

    if !spec.omits_menu(BuiltinMenu::Edit) {
        let place = spec.placement(BuiltinMenu::Edit);
        let edit = SubmenuBuilder::new(app, "Edit").build().map_err(s)?;
        let mut open = true;
        if let Some(p) = place {
            block(app, &edit, &p.start, &mut open, &mut handles)?;
            separator(app, &edit, &mut open)?;
        }
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
        if let Some(p) = place {
            if !p.end.is_empty() {
                edit.append(&PredefinedMenuItem::separator(app).map_err(s)?)
                    .map_err(s)?;
                add_entries(app, &edit, &p.end, &mut handles)?;
            }
        }
        built.push((MenuSlot::Edit, edit));
    }

    if !spec.omits_menu(BuiltinMenu::View) {
        let place = spec.placement(BuiltinMenu::View);
        let view = SubmenuBuilder::new(app, "View").build().map_err(s)?;
        let mut open = true;
        if let Some(p) = place {
            block(app, &view, &p.start, &mut open, &mut handles)?;
        }
        // Fullscreen is the only built-in View verb; reload/zoom are deliberately absent. Only
        // macOS has the View menu unless an app places its own entries in it.
        if mac && !spec.omits(BuiltinMenu::View, BuiltinItem::Fullscreen) {
            separator(app, &view, &mut open)?;
            view.append(&PredefinedMenuItem::fullscreen(app, None).map_err(s)?)
                .map_err(s)?;
            open = false;
        }
        if let Some(p) = place {
            if !p.end.is_empty() {
                separator(app, &view, &mut open)?;
                block(app, &view, &p.end, &mut open, &mut handles)?;
            }
        }
        if !view.items().map_err(s)?.is_empty() {
            built.push((MenuSlot::View, view));
        }
    }

    for g in spec.groups.iter().filter(|g| g.applies(platform)) {
        let sub = SubmenuBuilder::new(app, &g.label).build().map_err(s)?;
        add_entries(app, &sub, &g.entries, &mut handles)?;
        built.push((MenuSlot::Group(g.label.clone()), sub));
    }

    if !spec.omits_menu(BuiltinMenu::Window) {
        use BuiltinItem as I;
        use BuiltinMenu as M;
        let place = spec.placement(M::Window);
        let window = SubmenuBuilder::new(app, "Window").build().map_err(s)?;
        let mut open = true;
        if let Some(p) = place {
            block(app, &window, &p.start, &mut open, &mut handles)?;
        }
        if !spec.omits(M::Window, I::Minimize) {
            separator(app, &window, &mut open)?;
            window
                .append(&PredefinedMenuItem::minimize(app, None).map_err(s)?)
                .map_err(s)?;
            open = false;
        }
        if !spec.omits(M::Window, I::Maximize) {
            separator(app, &window, &mut open)?;
            window
                .append(&PredefinedMenuItem::maximize(app, None).map_err(s)?)
                .map_err(s)?;
            open = false;
        }
        // Windows/Linux: an overridden Close Window already sits in File (one item per id), and
        // File's own Close Window is the native one.
        let close = override_entry(spec, OverrideRole::CloseWindow, mac);
        if (mac || close.is_none()) && !spec.omits(M::Window, I::CloseWindow) {
            separator(app, &window, &mut open)?;
            match close {
                Some(close) => add_entries(app, &window, &[close], &mut handles)?,
                None => window
                    .append(&PredefinedMenuItem::close_window(app, None).map_err(s)?)
                    .map_err(s)?,
            }
            open = false;
        }
        if let Some(p) = place {
            if !p.end.is_empty() {
                separator(app, &window, &mut open)?;
                block(app, &window, &p.end, &mut open, &mut handles)?;
            }
        }
        if !window.items().map_err(s)?.is_empty() {
            built.push((MenuSlot::Window, window));
        }
    }

    if !spec.omits_menu(BuiltinMenu::Help) {
        let place = spec.placement(BuiltinMenu::Help);
        let help = SubmenuBuilder::new(app, "Help").build().map_err(s)?;
        if let Some(p) = place {
            add_entries(app, &help, &p.start, &mut handles)?;
        }
        add_entries(app, &help, &spec.help, &mut handles)?;
        if let Some(p) = place {
            add_entries(app, &help, &p.end, &mut handles)?;
        }
        built.push((MenuSlot::Help, help));
    }

    let default: Vec<MenuSlot> = built.iter().map(|(slot, _)| slot.clone()).collect();
    for slot in resolve_order(&spec.order, &default, mac) {
        if let Some(at) = built.iter().position(|(have, _)| *have == slot) {
            let (_, sub) = built.remove(at);
            menu.append(&sub).map_err(s)?;
        }
    }

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
