//! Project home, pop-out sidebar, `/config` screen and goal chip helpers for
//! the inline TUI (D14, D15, D5, D6).
//!
//! Everything here is presentation data and pure helpers: the pickers reuse
//! the shared [`ListPicker`] widget, and every action they emit is executed
//! by `tui_host` through the [`crate::tui_host::TurnHost`] authority. Nothing
//! in this module owns a session, a route, or a store.

use std::collections::HashMap;

use rightkit_terminal_ui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rightkit_terminal_ui::nav::{ListPicker, PickerParams, SelectionItem};

/// One persisted chat (root session) as the browser lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatEntry {
    pub id: String,
    pub title: String,
    /// Sortable ISO-ish timestamp of the last activity (may be empty).
    pub updated: String,
}

/// One project (workspace root) and its chats, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectEntry {
    pub root: String,
    pub label: String,
    pub chats: Vec<ChatEntry>,
}

/// What accepting a row does. Executed by the drive loop, never here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickAction {
    /// Enter (or start a chat in) the project at this root.
    EnterProject(String),
    /// Resume this persisted chat.
    OpenChat(String),
    /// Change one `[tui]` setting: key, new value.
    SetConfig(String, String),
    /// Display-only row.
    Noop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    Home,
    Sidebar,
    Config,
}

/// A [`ListPicker`] plus the action each row carries.
pub struct ActionPicker {
    pub kind: PickKind,
    pub picker: ListPicker,
    pub actions: Vec<PickAction>,
}

/// Resolved `[tui]` preferences the drive loop consumes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TuiSettings {
    /// `""`/`auto` follow the terminal; `dark`/`light` force it.
    pub theme: String,
    /// `""` auto-detect, else `bel` | `osc9` | `off`.
    pub notify: String,
    pub vim: bool,
    pub fullscreen: bool,
    pub copy_on_select: bool,
    pub always_copy_full_response: bool,
    pub agents_view_default: bool,
    /// Footer items to show; empty means all.
    pub statusline: Vec<String>,
    /// Action name (`sidebar`, `projects`, `config`) -> key spec (`ctrl+b`).
    pub keymap: HashMap<String, String>,
}

impl TuiSettings {
    pub fn from_config(config: &rightkit_agent_config::TuiConfig) -> Self {
        let norm = |s: &str| s.trim().to_ascii_lowercase();
        Self {
            theme: norm(&config.theme),
            notify: norm(&config.notify),
            vim: config.vim.unwrap_or(false),
            fullscreen: config.fullscreen.unwrap_or(false),
            copy_on_select: config.copy_on_select.unwrap_or(true),
            always_copy_full_response: config.always_copy_full_response.unwrap_or(false),
            agents_view_default: config.agents_view_default.unwrap_or(false),
            statusline: config
                .statusline
                .iter()
                .map(|s| norm(s))
                .filter(|s| !s.is_empty())
                .collect(),
            keymap: config
                .keymap
                .iter()
                .map(|(k, v)| (norm(k), norm(v)))
                .filter(|(k, v)| !k.is_empty() && !v.is_empty())
                .collect(),
        }
    }

    /// Whether a footer item is shown (`cwd`, `branch`, `model`, `ctx`,
    /// `cache`, `cost`, `goal`). An empty list shows everything.
    pub fn shows(&self, item: &str) -> bool {
        self.statusline.is_empty() || self.statusline.iter().any(|s| s == "all" || s == item)
    }

    /// The key spec bound to an action, with the shipped default.
    pub fn key_for(&self, action: &str) -> String {
        if let Some(spec) = self.keymap.get(action) {
            return spec.clone();
        }
        match action {
            "sidebar" => "ctrl+b",
            "projects" => "ctrl+p",
            "config" => "f2",
            _ => "",
        }
        .to_string()
    }
}

/// Mutable TUI-side state for these surfaces; one field on `SessionState`.
#[derive(Default)]
pub struct ProjUi {
    pub picker: Option<ActionPicker>,
    pub settings: TuiSettings,
    /// Theme/notify changed; the drive loop re-derives them once.
    pub settings_dirty: bool,
    /// The home screen opened before any chats were known; refill when ready.
    pub home_refresh: bool,
}

// ---------------------------------------------------------------------------
// Key specs
// ---------------------------------------------------------------------------

/// Whether `key` is the chord `spec` (`ctrl+b`, `alt+p`, `f2`, `ctrl+shift+x`).
pub fn key_matches(spec: &str, key: &KeyEvent) -> bool {
    let spec = spec.trim().to_ascii_lowercase();
    if spec.is_empty() {
        return false;
    }
    let mut want = KeyModifiers::NONE;
    let mut code: Option<KeyCode> = None;
    for part in spec.split('+') {
        match part.trim() {
            "ctrl" | "control" => want |= KeyModifiers::CONTROL,
            "alt" | "meta" | "opt" | "option" => want |= KeyModifiers::ALT,
            "shift" => want |= KeyModifiers::SHIFT,
            "tab" => code = Some(KeyCode::Tab),
            "enter" | "return" => code = Some(KeyCode::Enter),
            "esc" | "escape" => code = Some(KeyCode::Esc),
            "space" => code = Some(KeyCode::Char(' ')),
            name if name.starts_with('f')
                && name.len() > 1
                && name[1..].chars().all(|c| c.is_ascii_digit()) =>
            {
                code = name[1..].parse::<u8>().ok().map(KeyCode::F);
            }
            name if name.chars().count() == 1 => code = name.chars().next().map(KeyCode::Char),
            _ => return false,
        }
    }
    let Some(code) = code else { return false };
    let have = key.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
    let code_ok = match (key.code, code) {
        (KeyCode::Char(a), KeyCode::Char(b)) => a.to_ascii_lowercase() == b,
        (a, b) => a == b,
    };
    if !code_ok {
        return false;
    }
    // Shift is part of the character itself for printable keys.
    if matches!(code, KeyCode::Char(_)) {
        (have - KeyModifiers::SHIFT) == (want - KeyModifiers::SHIFT)
    } else {
        have == want
    }
}

fn valid_key_spec(spec: &str) -> bool {
    let mut key_count = 0;
    for part in spec.split('+').map(str::trim) {
        match part {
            "ctrl" | "control" | "alt" | "meta" | "opt" | "option" | "shift" => {}
            "tab" | "enter" | "return" | "esc" | "escape" | "space" => key_count += 1,
            name if name.starts_with('f') && name.len() > 1 => {
                if !name[1..].parse::<u8>().is_ok_and(|n| (1..=24).contains(&n)) {
                    return false;
                }
                key_count += 1;
            }
            name if name.chars().count() == 1 => key_count += 1,
            _ => return false,
        }
    }
    key_count == 1
}

// ---------------------------------------------------------------------------
// Project / chat browser
// ---------------------------------------------------------------------------

/// Directory display label: the last path component.
pub fn project_label(root: &str) -> String {
    std::path::Path::new(root)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| root.to_string())
}

fn norm_path(path: &str) -> String {
    path.trim()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/")
        .trim_end_matches('/')
        .to_ascii_lowercase()
}

/// Whether two roots name the same directory (separator/case tolerant).
pub fn same_root(a: &str, b: &str) -> bool {
    norm_path(a) == norm_path(b)
}

/// Whether the typed filter text reads as a folder path rather than a search.
pub fn looks_like_path(query: &str) -> bool {
    let q = query.trim();
    q.starts_with('~')
        || q.contains('/')
        || q.contains('\\')
        || (q.len() >= 2 && q.as_bytes()[1] == b':')
}

/// Expand a leading `~` against the home directory.
pub fn expand_path(path: &str) -> String {
    let p = path.trim();
    if let Some(rest) = p.strip_prefix('~') {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(std::path::PathBuf::from);
        if let Some(home) = home {
            let rest = rest.trim_start_matches(['/', '\\']);
            return home.join(rest).display().to_string();
        }
    }
    p.to_string()
}

fn short_time(updated: &str) -> String {
    updated.replace('T', " ").chars().take(16).collect()
}

/// Order projects: the current one first, then by latest chat activity.
fn ordered(mut projects: Vec<ProjectEntry>, current: Option<&str>) -> Vec<ProjectEntry> {
    for project in &mut projects {
        project.chats.sort_by(|a, b| b.updated.cmp(&a.updated));
    }
    projects.sort_by(|a, b| {
        let a_cur = current.is_some_and(|c| same_root(c, &a.root));
        let b_cur = current.is_some_and(|c| same_root(c, &b.root));
        b_cur
            .cmp(&a_cur)
            .then_with(|| {
                let a_last = a.chats.first().map(|c| c.updated.as_str()).unwrap_or("");
                let b_last = b.chats.first().map(|c| c.updated.as_str()).unwrap_or("");
                b_last.cmp(a_last)
            })
            .then_with(|| a.label.cmp(&b.label))
    });
    projects
}

/// The launch home screen: recent projects, then recent chats across ALL
/// projects. The project containing the launch directory is first and
/// preselected, so Enter goes straight in.
pub fn build_home(
    projects: Vec<ProjectEntry>,
    current_dir: Option<&str>,
    active_session: Option<&str>,
) -> ActionPicker {
    // The launch folder is always offered, enrolled or not.
    let mut projects = projects;
    if let Some(dir) = current_dir {
        if !projects.iter().any(|p| same_root(&p.root, dir)) {
            projects.push(ProjectEntry {
                root: dir.to_string(),
                label: project_label(dir),
                chats: Vec::new(),
            });
        }
    }
    let projects = ordered(projects, current_dir);
    let mut items: Vec<SelectionItem> = Vec::new();
    let mut actions: Vec<PickAction> = Vec::new();
    for project in &projects {
        let is_current = current_dir.is_some_and(|c| same_root(c, &project.root));
        let mut description = project.root.clone();
        if is_current {
            description.push_str("  (this folder)");
        }
        items.push(
            SelectionItem::new(format!("Project  {}", project.label))
                .description(description)
                .search_value(format!("{} {}", project.label, project.root))
                .current(is_current),
        );
        actions.push(PickAction::EnterProject(project.root.clone()));
    }
    let mut chats: Vec<(&ProjectEntry, &ChatEntry)> = projects
        .iter()
        .flat_map(|p| p.chats.iter().map(move |c| (p, c)))
        .filter(|(_, c)| active_session.is_none_or(|id| id != c.id))
        .collect();
    chats.sort_by(|a, b| b.1.updated.cmp(&a.1.updated));
    for (project, chat) in chats.into_iter().take(15) {
        items.push(
            SelectionItem::new(format!("Chat  {}", chat.title))
                .description(format!("{} · {}", project.label, short_time(&chat.updated)))
                .search_value(format!("{} {}", chat.title, project.label)),
        );
        actions.push(PickAction::OpenChat(chat.id.clone()));
    }
    let initial = projects
        .iter()
        .position(|p| current_dir.is_some_and(|c| same_root(c, &p.root)))
        .or(Some(0))
        .filter(|_| !items.is_empty());
    let mut params = PickerParams::picker(40, "RightKit · choose a project or chat", items);
    params.subtitle = Some(
        "Enter opens · type to filter · type a folder path + Enter to add one · Esc stays"
            .to_string(),
    );
    params.initial_selected_idx = initial;
    params.footer_hint = Some("↑/↓ select · Enter open · Esc stay in this folder".to_string());
    ActionPicker {
        kind: PickKind::Home,
        picker: ListPicker::new(params),
        actions,
    }
}

/// The Ctrl+B sidebar: every project with its chats underneath.
pub fn build_sidebar(
    projects: Vec<ProjectEntry>,
    current_dir: Option<&str>,
    active_session: Option<&str>,
) -> ActionPicker {
    let projects = ordered(projects, current_dir);
    let mut items: Vec<SelectionItem> = Vec::new();
    let mut actions: Vec<PickAction> = Vec::new();
    let mut initial: Option<usize> = None;
    for project in &projects {
        let is_current = current_dir.is_some_and(|c| same_root(c, &project.root));
        items.push(
            SelectionItem::new(format!("▾ {}", project.label))
                .description(project.root.clone())
                .search_value(format!("{} {}", project.label, project.root))
                .current(is_current),
        );
        actions.push(PickAction::EnterProject(project.root.clone()));
        for chat in project.chats.iter().take(20) {
            if active_session.is_some_and(|id| id == chat.id) {
                initial = Some(items.len());
            }
            items.push(
                SelectionItem::new(format!("   {}", chat.title))
                    .description(format!("   {}", short_time(&chat.updated)))
                    .search_value(format!("{} {}", chat.title, project.label))
                    .current(active_session.is_some_and(|id| id == chat.id)),
            );
            actions.push(PickAction::OpenChat(chat.id.clone()));
        }
    }
    let mut params = PickerParams::picker(41, "Projects", items);
    params.subtitle =
        Some("Enter on a project starts a new chat there; Enter on a chat opens it".to_string());
    params.initial_selected_idx = initial;
    params.footer_hint = Some("↑/↓ select · Enter switch · Esc/Ctrl+B close".to_string());
    ActionPicker {
        kind: PickKind::Sidebar,
        picker: ListPicker::new(params),
        actions,
    }
}

// ---------------------------------------------------------------------------
// /config screen
// ---------------------------------------------------------------------------

fn next_in<'a>(values: &[&'a str], current: &str) -> &'a str {
    let at = values
        .iter()
        .position(|v| *v == current)
        .unwrap_or(values.len() - 1);
    values[(at + 1) % values.len()]
}

/// `/config`: each row shows `key: value`; Enter cycles/toggles it and
/// persists to `[tui]` in the user `config.toml`.
pub fn build_config(settings: &TuiSettings) -> ActionPicker {
    build_config_with_notice(settings, None)
}

/// `/config` screen that also shows the outcome of the change just made (the transcript notice
/// can sit behind the overlay, so the confirmation is rendered inside it).
pub fn build_config_with_notice(settings: &TuiSettings, notice: Option<&str>) -> ActionPicker {
    let theme = if settings.theme.is_empty() {
        "auto"
    } else {
        settings.theme.as_str()
    };
    let notify = if settings.notify.is_empty() {
        "auto"
    } else {
        settings.notify.as_str()
    };
    let vim = if settings.vim { "on" } else { "off" };
    let status_now =
        if settings.statusline.is_empty() || settings.statusline.iter().any(|v| v == "all") {
            "all".to_string()
        } else {
            settings.statusline.join(",")
        };
    let mut items = Vec::new();
    let mut actions = Vec::new();
    for (key, label, enabled) in [
        ("fullscreen", "fullscreen chat", settings.fullscreen),
        ("copy-on-select", "copy on select", settings.copy_on_select),
        (
            "always-copy-full-response",
            "always copy full response",
            settings.always_copy_full_response,
        ),
        (
            "agents-view-default",
            "open agents view by default",
            settings.agents_view_default,
        ),
    ] {
        items.push(
            SelectionItem::new(format!("{label}: {}", if enabled { "on" } else { "off" }))
                .description("Enter toggles · persisted in [tui]"),
        );
        actions.push(PickAction::SetConfig(
            key.into(),
            if enabled { "off" } else { "on" }.into(),
        ));
    }
    items.push(
        SelectionItem::new(format!("theme: {theme}"))
            .description("auto / dark / light — Enter cycles"),
    );
    actions.push(PickAction::SetConfig(
        "theme".into(),
        next_in(&["auto", "dark", "light"], theme).to_string(),
    ));
    items.push(
        SelectionItem::new(format!("notify: {notify}"))
            .description("auto / bel / osc9 / off — Enter cycles"),
    );
    actions.push(PickAction::SetConfig(
        "notify".into(),
        next_in(&["auto", "bel", "osc9", "off"], notify).to_string(),
    ));
    items.push(
        SelectionItem::new(format!("vim: {vim}"))
            .description("Vim editing in the composer — Enter toggles"),
    );
    actions.push(PickAction::SetConfig(
        "vim".into(),
        if settings.vim { "off" } else { "on" }.to_string(),
    ));
    items.push(
        SelectionItem::new(format!("statusline: {status_now}"))
            .description("footer items — Enter shows all / hides optional ones"),
    );
    let status_next =
        if settings.statusline.is_empty() || settings.statusline.iter().any(|v| v == "all") {
            "model,ctx,goal".to_string()
        } else {
            "all".to_string()
        };
    actions.push(PickAction::SetConfig("statusline".into(), status_next));
    for action in ["sidebar", "projects", "config"] {
        let current = settings.key_for(action);
        let next = next_in(&["ctrl+b", "ctrl+g", "alt+b", "f2"], &current);
        items.push(
            SelectionItem::new(format!("key {action}: {current}")).description(
                "Enter cycles key binding; set [tui.keymap] in config.toml for custom keys",
            ),
        );
        actions.push(PickAction::SetConfig(
            format!("keymap.{action}"),
            next.to_string(),
        ));
    }
    let mut params = PickerParams::picker(42, "Settings · [tui]", items);
    params.is_searchable = false;
    params.subtitle = notice.map(str::to_string);
    params.footer_hint = Some("↑/↓ select · Enter change · Esc close".to_string());
    ActionPicker {
        kind: PickKind::Config,
        picker: ListPicker::new(params),
        actions,
    }
}

/// Applies one validated TUI setting to `config`; the error names an unknown key or bad value.
pub fn apply_tui_setting(
    config: &mut rightkit_agent_config::HostConfig,
    key: &str,
    value: &str,
) -> Result<(), String> {
    match key {
        "theme"
            if ["", "auto", "dark", "light"]
                .contains(&value.trim().to_ascii_lowercase().as_str()) =>
        {
            config.tui.theme = value.trim().to_ascii_lowercase()
        }
        "notify"
            if ["", "auto", "bel", "osc9", "off"]
                .contains(&value.trim().to_ascii_lowercase().as_str()) =>
        {
            config.tui.notify = value.trim().to_ascii_lowercase()
        }
        "vim" => {
            config.tui.vim = Some(matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "on" | "true" | "yes" | "1"
            ))
        }
        "fullscreen" => {
            config.tui.fullscreen = Some(matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "on" | "true" | "yes" | "1"
            ))
        }
        "copy-on-select" => {
            config.tui.copy_on_select = Some(matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "on" | "true" | "yes" | "1"
            ))
        }
        "always-copy-full-response" => {
            config.tui.always_copy_full_response = Some(matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "on" | "true" | "yes" | "1"
            ))
        }
        "agents-view-default" => {
            config.tui.agents_view_default = Some(matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "on" | "true" | "yes" | "1"
            ))
        }
        "statusline" => {
            config.tui.statusline = if value.trim().eq_ignore_ascii_case("all") {
                vec!["all".to_string()]
            } else {
                value
                    .split(',')
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                    .map(str::to_ascii_lowercase)
                    .collect()
            };
        }
        "keymap.sidebar" | "keymap.projects" | "keymap.config" => {
            let action = key.strip_prefix("keymap.").unwrap_or_default();
            config
                .tui
                .keymap
                .insert(action.to_string(), value.trim().to_ascii_lowercase());
        }
        _ => return Err(format!("invalid TUI setting `{key}` or value `{value}`")),
    }
    Ok(())
}

/// Validate and normalise one `/config` / `/theme` / `/vim` value into the
/// TOML value stored under `[tui]`.
pub fn config_value(key: &str, value: &str) -> Result<toml::Value, String> {
    let v = value.trim().to_ascii_lowercase();
    match key {
        "theme" => match v.as_str() {
            "auto" | "" => Ok(toml::Value::String(String::new())),
            "dark" | "light" => Ok(toml::Value::String(v)),
            _ => Err("theme must be auto, dark or light".into()),
        },
        "notify" => match v.as_str() {
            "auto" | "" => Ok(toml::Value::String(String::new())),
            "bel" | "osc9" | "off" => Ok(toml::Value::String(v)),
            _ => Err("notify must be auto, bel, osc9 or off".into()),
        },
        "vim" => match v.as_str() {
            "on" | "true" | "1" | "yes" => Ok(toml::Value::Boolean(true)),
            "off" | "false" | "0" | "no" => Ok(toml::Value::Boolean(false)),
            _ => Err("vim must be on or off".into()),
        },
        "fullscreen" | "copy-on-select" | "always-copy-full-response" | "agents-view-default" => match v.as_str() {
            "on" | "true" | "1" | "yes" => Ok(toml::Value::Boolean(true)),
            "off" | "false" | "0" | "no" => Ok(toml::Value::Boolean(false)),
            _ => Err(format!("{key} must be on or off")),
        },
        "statusline" => {
            if v == "all" || v.is_empty() {
                return Ok(toml::Value::Array(vec![toml::Value::String(
                    "all".to_string(),
                )]));
            }
            let known = ["cwd", "branch", "model", "ctx", "cache", "goal"];
            let mut out = Vec::new();
            for item in v.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                if !known.contains(&item) {
                    return Err(format!(
                        "unknown statusline item `{item}` (use {})",
                        known.join(", ")
                    ));
                }
                out.push(toml::Value::String(item.to_string()));
            }
            Ok(toml::Value::Array(out))
        }
        "keymap.sidebar" | "keymap.projects" | "keymap.config" => {
            if valid_key_spec(&v) {
                Ok(toml::Value::String(v))
            } else {
                Err("keymap value must be a key or chord such as ctrl+b".into())
            }
        }
        other => Err(format!(
            "`{other}` is not an editable setting (theme, notify, vim, fullscreen, copy-on-select, always-copy-full-response, agents-view-default, statusline)"
        )),
    }
}

/// `<state root>/config.toml` (`rightkit_agent_config::state_root()`).
pub fn user_config_path() -> Option<std::path::PathBuf> {
    Some(rightkit_agent_config::state_root()?.join("config.toml"))
}

// ---------------------------------------------------------------------------
// Goal
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalCmd {
    Show,
    Set(String),
    Edit(String),
    Pause,
    Resume,
    Clear,
}

/// Parse `/goal` arguments: bare shows; `set|edit <text>`; `pause`,
/// `resume`, `clear`; anything else is the goal text itself.
pub fn parse_goal_args(rest: &str) -> Result<GoalCmd, String> {
    let rest = rest.trim();
    if rest.is_empty() {
        return Ok(GoalCmd::Show);
    }
    let (head, tail) = match rest.split_once(char::is_whitespace) {
        Some((h, t)) => (h, t.trim()),
        None => (rest, ""),
    };
    match head.to_ascii_lowercase().as_str() {
        "set" | "new" => {
            if tail.is_empty() {
                Err("usage: /goal set <what you want achieved>".into())
            } else {
                Ok(GoalCmd::Set(tail.to_string()))
            }
        }
        "edit" => {
            if tail.is_empty() {
                Err("usage: /goal edit <new goal text>".into())
            } else {
                Ok(GoalCmd::Edit(tail.to_string()))
            }
        }
        "pause" if tail.is_empty() => Ok(GoalCmd::Pause),
        "resume" if tail.is_empty() => Ok(GoalCmd::Resume),
        "clear" | "cancel" | "stop" if tail.is_empty() => Ok(GoalCmd::Clear),
        _ => Ok(GoalCmd::Set(rest.to_string())),
    }
}

fn clip(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    if line.chars().count() <= max {
        line.to_string()
    } else {
        let mut cut: String = line.chars().take(max.saturating_sub(1)).collect();
        cut.push('…');
        cut
    }
}

/// Footer chip from a goal route body (`{"goal": {...}}`) or a bare
/// snapshot: `goal (active): <text> · 40%`. `None` when no goal is open.
pub fn goal_chip(body: &serde_json::Value) -> Option<String> {
    let goal = body.get("goal").unwrap_or(body);
    if goal.is_null() || !goal.is_object() {
        return None;
    }
    let status = goal
        .get("status")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("active");
    let objective = goal
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    if objective.is_empty() || goal.get("dismissedAt").is_some_and(|v| !v.is_null()) {
        return None;
    }
    let progress = match goal
        .get("completionPercent")
        .and_then(serde_json::Value::as_i64)
    {
        Some(percent) => format!(" · {percent}%"),
        None => match goal
            .get("continuationCount")
            .and_then(serde_json::Value::as_i64)
        {
            Some(n) if n > 0 => format!(" · step {n}"),
            _ => String::new(),
        },
    };
    Some(format!(
        "goal ({status}): {}{progress}",
        clip(objective, 40)
    ))
}

/// Multi-line status for a bare `/goal`.
pub fn goal_status_text(body: &serde_json::Value) -> String {
    let Some(chip) = goal_chip(body) else {
        return "No goal is set for this chat. Use /goal set <text>.".to_string();
    };
    let goal = body.get("goal").unwrap_or(body);
    let full = goal
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("");
    let mut out = format!("{chip}\nObjective: {full}");
    if let Some(now) = goal
        .get("now")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push_str(&format!("\nNow: {now}"));
    }
    if let Some(next) = goal
        .get("next")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
    {
        out.push_str(&format!("\nNext: {next}"));
    }
    out
}

// ---------------------------------------------------------------------------
// Rect helper
// ---------------------------------------------------------------------------

/// Left overlay width for the sidebar inside `total` columns.
pub fn sidebar_width(total: u16) -> u16 {
    (total / 2).clamp(24, 52).min(total)
}
