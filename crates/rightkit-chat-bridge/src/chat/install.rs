//! `<cli> chat install`: put Chat skill where Claude and Codex
//! load skills, so a chat knows how to message chats on other computers.
//!
//! * Claude: `<CLAUDE_CONFIG_DIR or ~/.claude>/skills/pulse-chat/SKILL.md`
//! * Codex: `<CODEX_HOME or ~/.codex>/skills/pulse-chat/SKILL.md`
//!
//! The same file ships as the plugin `plugins/pulse-chat` in the repo. A
//! target whose config folder does not exist is skipped. Writes are atomic
//! (uniquely named temporary file renamed into place) and idempotent. Each target keeps
//! `.pulse-chat.json` (version, content hash, install time). A file that differs from both
//! that record and the new content is first copied to `SKILL.md.bak-<timestamp>`; uninstall
//! removes only a file whose hash matches the record and otherwise leaves it in place.
//! Install also retires the skill's pre-rename copy, `skills/pulse-bridge/`, the same way: its `SKILL.md` and
//! `.pulse-bridge.json` go only when the file's hash matches that record; the folder goes once empty.
//!
//! Windows only: the Claude Code plugin's hooks spawn `hooks/the launcher` without a shell. The
//! repository ships that name as a `#!/bin/sh` script (macOS and Linux ignore the extension);
//! on Windows the host app replaces it with the native launcher (`plugin-launcher/`). The repository's plugin folder
//! carries no binary, so this step copies the `the launcher` that sits beside `the app command`
//! (`Helpers\`) into every install folder `<claude config>/plugins/installed_plugins.json` lists
//! for a `<plugin>@…` plugin. `--claude-plugin` does only this step. The Windows installer runs it,
//! and owner app runs it at startup and whenever `installed_plugins.json` changes, because a
//! plugin update makes a new install folder without the file. It is idempotent: a file of the
//! same size and bytes is left alone, and a copy goes through a temporary file and a rename, so a
//! hook spawning mid-copy never sees half a file. A pass that copied something leaves a record
//! (`chat/claude-launcher-repair.json`) that `<cli> chat status` reads. `uninstall` removes it.
//!
//! Muse (`--muse`, and part of a plain install when Muse is installed) gets the plugin that ships
//! in the app (`plugins/muse/pulse`, Claude-plugin format): `muse plugins install <folder> --scope
//! user --json` (`muse plugins update pulse` when it is already installed), then `muse plugins
//! approve pulse --json`, which trusts the plugin's hooks (on Windows through `muse.cmd`, not yet
//! verified there). `<cli> chat plugins` (discovery, below) runs these steps by itself. Muse's hooks run `<cli> chat hook
//! --harness muse` to register each Muse chat with the host app and hand it waiting messages. Muse's hooks.json has
//! no `args` and refuses two hooks with one command, so each event is its own file,
//! `hooks/muse-<event>.exe`: a `#!/bin/sh` script in the repository and the Mac app (macOS and
//! Linux ignore the extension), and on Windows the payload ships `Helpers\the launcher`
//! (`plugin-launcher/`) under those names in `plugins\muse\pulse`, which acts on the `muse-` prefix.
//! Written, not yet run on the Dell (whether Muse spawns that `.exe` there is unverified). Uninstall
//! is `muse plugins remove pulse --json`. Each Muse command is bounded by 30 seconds.
//!
//! Codex also gets three hook entries (PostToolUse, Stop, UserPromptSubmit) merged into
//! `<codex home>/hooks.json`, each running `"<this exe>" chat codex-hook`. Other entries and
//! keys are kept; the host app's entries are recognised by a command ending ` chat codex-hook`; an
//! invalid file is left alone; the previous file is copied once to `hooks.json.pulse-bak`.
//! Codex runs hooks only with `[features] hooks = true` in `config.toml`, so install adds that one
//! line when it is missing (an explicit `hooks = false` is the owner's choice and is left, with a
//! note, and so is any layout it does not fully recognise); no other line changes. Codex runs a new hook only after its owner trusts
//! it (`/hooks`); the host app never writes trust state.

use super::brand::brand;
use super::deliver_claude::{sessions_dir, sessions_dirs};
use super::deliver_codex::codex_home;
use rightkit_child_process::RunOptions;
use serde::Serialize;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The chat skill's text, supplied by the host app (`Brand::skill_markdown`).
fn skill() -> &'static str {
    &brand().skill_markdown
}

fn manifest() -> &'static str {
    &brand().manifest_name
}

fn skill_dir_name() -> &'static str {
    &brand().skill_name
}

#[derive(Debug, Clone, Default)]
pub struct Options {
    pub claude: bool,
    pub codex: bool,
    pub muse: bool,
    /// Only the Windows launcher step (`--claude-plugin`).
    pub claude_plugin: bool,
    pub dry_run: bool,
    /// A Claude config folder other than this process's own (another profile's `.claude`): the
    /// Claude skill and the Windows launcher go there. Discovery sets it; the command line does not.
    pub claude_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Change {
    pub target: String,
    pub path: String,
    /// changed, unchanged, left (modified, kept), absent, skipped or failed.
    pub action: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    pub dry_run: bool,
    pub changes: Vec<Change>,
}

struct Target {
    name: &'static str,
    /// The config folder that must exist (`~/.claude`, `~/.codex`).
    root: PathBuf,
    skill: PathBuf,
}

fn targets(options: &Options) -> Vec<Target> {
    let all = no_target_named(options);
    let mut list = Vec::new();
    if all || options.claude {
        let root = claude_root(options);
        list.push(Target {
            name: "claude",
            skill: skills_dir(&root).join(skill_dir_name()).join("SKILL.md"),
            root,
        });
    }
    if all || options.codex {
        let root = codex_home();
        list.push(Target {
            name: "codex",
            skill: skills_dir(&root).join(skill_dir_name()).join("SKILL.md"),
            root,
        });
    }
    list
}

/// The Claude config folder this run installs into: the named one, else this process's own
/// (`sessions_dir()` is `<claude config>/sessions`).
fn claude_root(options: &Options) -> PathBuf {
    options.claude_root.clone().unwrap_or_else(|| {
        sessions_dir()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_default()
    })
}

/// Whether no target is named: everything is installed or removed.
fn no_target_named(options: &Options) -> bool {
    !options.claude && !options.codex && !options.muse && !options.claude_plugin
}

/// `<root>/skills`, resolved through a junction or symlink when it is one: Windows refuses
/// to create files across an "untrusted mount point" (os error 448; a `.codex\skills` junction to
/// `.claude\skills` is common), so the real folder is written instead. The link is read, not
/// followed (`read_link`), because following it is what fails: `canonicalize` through such a
/// junction fails with the same 448, which left the old code writing through the junction
/// (seen on the Dell, 2026-10-22). Codex and Claude then share one skill file, written once.
fn skills_dir(root: &Path) -> PathBuf {
    let dir = root.join("skills");
    let Ok(meta) = std::fs::symlink_metadata(&dir) else {
        return dir;
    };
    if meta.file_type().is_symlink() {
        if let Ok(target) = std::fs::read_link(&dir) {
            let target = target.to_string_lossy().into_owned();
            // A junction's target can come back as `\??\C:\...`.
            let target = PathBuf::from(target.strip_prefix(r"\??\").unwrap_or(&target));
            let target = if target.is_absolute() {
                target
            } else {
                root.join(target)
            };
            return std::fs::canonicalize(&target).unwrap_or(target);
        }
        return dir;
    }
    if meta.is_dir() {
        std::fs::canonicalize(&dir).unwrap_or(dir)
    } else {
        dir
    }
}

/// Whether two skill paths are the same file: equal once the Windows `\\?\` prefix and case are
/// ignored (one side may have been canonicalised and the other not).
fn same_skill_path(a: &Path, b: &Path) -> bool {
    let plain = |p: &Path| {
        let text = p.to_string_lossy().into_owned();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text).to_string();
        if cfg!(windows) {
            text.to_lowercase()
        } else {
            text
        }
    };
    plain(a) == plain(b)
}

/// FNV-1a 64 of the text, as 16 hex digits (an ownership check, not a security boundary).
fn hash(text: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.bytes() {
        h = (h ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

fn manifest_path(t: &Target) -> PathBuf {
    t.skill.with_file_name(manifest())
}

/// The content hash recorded at the last install, if any.
fn recorded_hash(t: &Target) -> Option<String> {
    let text = std::fs::read_to_string(manifest_path(t)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["hash"].as_str().map(String::from)
}

fn write_manifest(t: &Target) -> Result<(), String> {
    let record = serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "hash": hash(skill()),
        "installedAt": now_ms(),
    });
    rightkit_fs::atomic_write_with(
        &manifest_path(t),
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..Default::default()
        },
        |file| file.write_all(record.to_string().as_bytes()),
    )
    .map_err(|e| e.to_string())
}

/// A backup name that does not exist yet.
fn backup_path(t: &Target) -> PathBuf {
    let stamp = now_ms();
    let first = t.skill.with_file_name(format!("SKILL.md.bak-{stamp}"));
    if !first.exists() {
        return first;
    }
    (1..)
        .map(|n| t.skill.with_file_name(format!("SKILL.md.bak-{stamp}-{n}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Removes the skill's pre-rename install (`skills/pulse-bridge/`) when the host app wrote it and nobody changed it since,
/// so chats stop loading the old name. A modified or unrecorded file is left where it is.
fn retire_legacy(t: &Target) {
    let Some(dir) = t
        .skill
        .parent()
        .and_then(Path::parent)
        .map(|skills| skills.join(&brand().legacy_skill_name))
    else {
        return;
    };
    let (skill, record) = (
        dir.join("SKILL.md"),
        dir.join(&brand().legacy_manifest_name),
    );
    let Ok(text) = std::fs::read_to_string(&skill) else {
        return;
    };
    let recorded = std::fs::read_to_string(&record)
        .ok()
        .and_then(|r| serde_json::from_str::<serde_json::Value>(&r).ok())
        .and_then(|v| v["hash"].as_str().map(String::from));
    if recorded.as_deref() != Some(&hash(&text)) {
        return;
    }
    if std::fs::remove_file(&skill).is_ok() {
        let _ = std::fs::remove_file(&record);
        let _ = std::fs::remove_dir(&dir);
    }
}

fn install_one(t: &Target, dry: bool) -> Result<(&'static str, String), String> {
    if skill().is_empty() {
        return Err(
            "the host app supplied no chat skill (Brand::skill_markdown), so there is nothing to install"
                .to_string(),
        );
    }
    if !t.root.is_dir() {
        return Ok(("skipped", format!("{} isn't set up here", t.root.display())));
    }
    if !dry {
        retire_legacy(t);
    }
    let existing = std::fs::read_to_string(&t.skill).ok();
    if existing.as_deref() == Some(skill()) {
        if !dry && recorded_hash(t).as_deref() != Some(&hash(skill())) {
            write_manifest(t)?;
        }
        return Ok(("unchanged", "already installed".into()));
    }
    if dry {
        return Ok(("changed", "dry run: nothing written".into()));
    }
    let mut note = String::from("restart open chats to see it");
    if let Some(old) = &existing {
        let h = hash(old);
        if recorded_hash(t).as_deref() != Some(&h) && h != hash(skill()) {
            let backup = backup_path(t);
            rightkit_fs::copy_tree(
                &t.skill,
                &backup,
                rightkit_fs::CopyOptions::new(true, false),
            )
            .map_err(|e| e.to_string())?;
            let name = backup
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            note = format!("kept the previous file as {name}; {note}");
        }
    }
    rightkit_fs::atomic_write_with(
        &t.skill,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..Default::default()
        },
        |file| file.write_all(skill().as_bytes()),
    )
    .map_err(|e| e.to_string())?;
    write_manifest(t)?;
    Ok(("changed", note))
}

fn uninstall_one(t: &Target, dry: bool) -> Result<(&'static str, String), String> {
    let Some(existing) = std::fs::read_to_string(&t.skill).ok() else {
        return Ok(("absent", "the app skill isn't installed".into()));
    };
    let h = hash(&existing);
    let owned = match recorded_hash(t) {
        Some(recorded) => recorded == h,
        None => existing == skill(),
    };
    if !owned {
        return Ok(("left", "left in place: modified".into()));
    }
    if dry {
        return Ok(("changed", "dry run: nothing changed".into()));
    }
    std::fs::remove_file(&t.skill).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(manifest_path(t));
    if let Some(dir) = t.skill.parent() {
        let _ = std::fs::remove_dir(dir);
    }
    Ok(("changed", "removed".into()))
}

const HOOK_SUFFIX: &str = " chat codex-hook";
const HOOK_EVENTS: [&str; 3] = ["PostToolUse", "Stop", "UserPromptSubmit"];
/// Said after every Codex hook install.
pub const TRUST_NOTE: &str = "Codex runs a new hook only after you trust it: open Codex, run /hooks, and trust the app's three hooks.";

fn hooks_path() -> PathBuf {
    codex_home().join("hooks.json")
}

/// The hook command line. Codex runs hook commands through the platform shell: `sh` on the Mac,
/// PowerShell on Windows. PowerShell reads `"C:\\…\\the app command" chat codex-hook` as a string followed by
/// stray tokens and fails every hook (seen on the Dell, 2026-10-19, #409), so on Windows the path is
/// written bare when it has no space, and with PowerShell's call operator otherwise.
fn hook_command(exe: &std::path::Path) -> String {
    let path = exe.display().to_string();
    if cfg!(windows) {
        if path.contains(' ') {
            format!("& \"{path}\"{HOOK_SUFFIX}")
        } else {
            format!("{path}{HOOK_SUFFIX}")
        }
    } else {
        format!("\"{path}\"{HOOK_SUFFIX}")
    }
}

fn is_own_hook(hook: &serde_json::Value) -> bool {
    hook["command"]
        .as_str()
        .is_some_and(|c| c.ends_with(HOOK_SUFFIX))
}

/// Whether hooks.json holds the host app's entry for every event.
pub fn hooks_present() -> bool {
    let Ok(text) = std::fs::read_to_string(hooks_path()) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    HOOK_EVENTS.iter().all(|event| {
        root["hooks"][*event].as_array().is_some_and(|groups| {
            groups.iter().any(|g| {
                g["hooks"]
                    .as_array()
                    .is_some_and(|h| h.iter().any(is_own_hook))
            })
        })
    })
}

/// Merge (or remove) the host app's hook entries; `Ok(false)` means nothing needed to change.
fn merge_hooks(root: &mut serde_json::Value, command: Option<&str>) -> Result<bool, String> {
    let Some(top) = root.as_object_mut() else {
        return Err("hooks.json is not a JSON object".into());
    };
    let before = serde_json::Value::Object(top.clone());
    match command {
        Some(command) => {
            let hooks = top
                .entry("hooks")
                .or_insert_with(|| serde_json::json!({}))
                .as_object_mut()
                .ok_or("\"hooks\" in hooks.json is not an object")?;
            let entry = serde_json::json!({"type": "command", "command": command, "timeout": 10});
            for event in HOOK_EVENTS {
                let groups = hooks
                    .entry(event)
                    .or_insert_with(|| serde_json::json!([]))
                    .as_array_mut()
                    .ok_or_else(|| format!("\"{event}\" in hooks.json is not a list"))?;
                let mut placed = false;
                for group in groups.iter_mut() {
                    if let Some(list) = group.get_mut("hooks").and_then(|h| h.as_array_mut()) {
                        for hook in list.iter_mut().filter(|h| is_own_hook(h)) {
                            *hook = entry.clone();
                            placed = true;
                        }
                    }
                }
                if !placed {
                    groups.push(serde_json::json!({"hooks": [entry.clone()]}));
                }
            }
        }
        None => {
            if let Some(hooks) = top.get_mut("hooks").and_then(|h| h.as_object_mut()) {
                for event in HOOK_EVENTS {
                    let Some(groups) = hooks.get_mut(event).and_then(|g| g.as_array_mut()) else {
                        continue;
                    };
                    // Drop a group only when removing the host app's entry emptied it.
                    groups.retain_mut(|group| {
                        let Some(list) = group.get_mut("hooks").and_then(|h| h.as_array_mut())
                        else {
                            return true;
                        };
                        let had = list.len();
                        list.retain(|h| !is_own_hook(h));
                        !(list.is_empty() && had > 0)
                    });
                    if groups.is_empty() {
                        hooks.remove(event);
                    }
                }
            }
        }
    }
    Ok(serde_json::Value::Object(top.clone()) != before)
}

fn hooks_one(uninstall: bool, dry: bool) -> Result<(&'static str, String), String> {
    let root_dir = codex_home();
    if !root_dir.is_dir() {
        return Ok((
            "skipped",
            format!("{} isn't set up here", root_dir.display()),
        ));
    }
    let path = hooks_path();
    let existing = std::fs::read_to_string(&path).ok();
    let mut root: serde_json::Value = match &existing {
        Some(text) => match serde_json::from_str(text) {
            Ok(v) => v,
            Err(e) => {
                return Ok((
                    "left",
                    format!("hooks.json isn't valid JSON ({e}); not touched"),
                ));
            }
        },
        None if uninstall => return Ok(("absent", "no hooks.json".into())),
        None => serde_json::json!({}),
    };
    let command = if uninstall {
        None
    } else {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        Some(hook_command(&exe))
    };
    let changed = match merge_hooks(&mut root, command.as_deref()) {
        Ok(changed) => changed,
        Err(e) => return Ok(("left", format!("{e}; not touched"))),
    };
    if !changed && existing.is_some() {
        return Ok((
            "unchanged",
            if uninstall {
                "no the app hooks"
            } else {
                "already installed"
            }
            .into(),
        ));
    }
    if dry {
        return Ok(("changed", "dry run: nothing written".into()));
    }
    if existing.is_some() {
        // The first backup is kept: a later install never writes over it.
        let backup = path.with_file_name(format!("hooks.json.{}", brand().hooks_backup_suffix));
        match rightkit_fs::copy_tree(&path, &backup, rightkit_fs::CopyOptions::new(true, false)) {
            Ok(()) | Err(rightkit_fs::FsError::AlreadyExists(_)) => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    let text = serde_json::to_string_pretty(&root).map_err(|e| e.to_string())?;
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..Default::default()
        },
        |file| file.write_all(format!("{text}\n").as_bytes()),
    )
    .map_err(|e| e.to_string())?;
    Ok((
        "changed",
        if uninstall {
            "removed the app's hooks".into()
        } else {
            TRUST_NOTE.into()
        },
    ))
}

/// A TOML table header (`[name]`, optional spaces and trailing comment) and its name, else `None`.
fn table_header(line: &str) -> Option<String> {
    let t = line.trim();
    let t = t.split('#').next().unwrap_or("").trim();
    let inner = t.strip_prefix('[')?.strip_suffix(']')?;
    let inner = inner.strip_prefix('[').unwrap_or(inner);
    let inner = inner.strip_suffix(']').unwrap_or(inner).trim();
    // Bare key parts are letters, digits, `_` and `-`; a quoted part may hold anything but its
    // own quote (Codex writes `[projects."/home/vendure/sites"]`, #483).
    let mut quote: Option<char> = None;
    let mut plain = !inner.is_empty();
    for c in inner.chars() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None => plain &= c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | ' '),
        }
    }
    (plain && quote.is_none()).then(|| inner.replace(['"', '\'', ' '], ""))
}

/// The bare key of a `key = value` line, quotes removed.
fn line_key(line: &str) -> String {
    line.split('=')
        .next()
        .unwrap_or("")
        .trim()
        .replace(['"', '\''], "")
}

/// Turns on Codex's `hooks` feature in `config.toml` by text, adding one line and keeping every
/// other line and the file's line endings. Only a layout it fully recognises is written: a file
/// with multi-line strings, or one that sets `features` as an inline table or dotted key, is left
/// with a note, and so is an explicit `hooks = false`.
fn enable_hooks_feature(text: &str) -> Result<Option<String>, String> {
    let manual = "add `hooks = true` under [features] in config.toml yourself";
    if text.contains("\"\"\"") || text.contains("'''") {
        return Err(format!(
            "config.toml has multi-line strings the app won't edit; {manual}"
        ));
    }
    let mut section: Option<String> = None;
    let mut features_at = None;
    for (i, line) in text.lines().enumerate() {
        let t = line.trim();
        if t.is_empty() || t.starts_with('#') {
            continue;
        }
        if let Some(name) = table_header(t) {
            if name == "features" {
                if features_at.is_some() {
                    return Err(format!("config.toml has [features] twice; {manual}"));
                }
                features_at = Some(i);
            }
            section = Some(name);
            continue;
        }
        if t.starts_with('[') {
            // An array value continuing over lines, or a header the host app doesn't read.
            if section.as_deref() == Some("features") || section.is_none() {
                return Err(format!(
                    "config.toml has a layout the app won't edit; {manual}"
                ));
            }
            continue;
        }
        let key = line_key(t);
        let in_features = section.as_deref() == Some("features");
        if (section.is_none() && (key == "features" || key.starts_with("features.")))
            || (!in_features && section.is_some() && key.starts_with("features."))
        {
            return Err(format!("config.toml sets features another way; {manual}"));
        }
        if in_features && key == "hooks" {
            let value = t
                .split_once('=')
                .map(|(_, v)| v)
                .unwrap_or("")
                .split('#')
                .next()
                .unwrap_or("")
                .trim();
            return if value == "true" {
                Ok(None)
            } else {
                Err(format!(
                    "config.toml sets hooks = {value} under [features]; left as you set it, so the app's hooks won't run"
                ))
            };
        }
    }
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<&str> = text.lines().collect();
    match features_at {
        Some(i) => lines.insert(i + 1, "hooks = true"),
        None => {
            if lines.last().is_some_and(|l| !l.trim().is_empty()) {
                lines.push("");
            }
            lines.push("[features]");
            lines.push("hooks = true");
        }
    }
    Ok(Some(format!("{}{eol}", lines.join(eol))))
}

/// Whether `config.toml` has `[features] hooks = true`; without it Codex runs no hooks at all
///, so `<cli> chat status` says so.
pub fn hooks_feature_on() -> bool {
    std::fs::read_to_string(codex_home().join("config.toml"))
        .is_ok_and(|text| matches!(enable_hooks_feature(&text), Ok(None)))
}

fn features_one(dry: bool) -> Result<(&'static str, String), String> {
    let path = codex_home().join("config.toml");
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("couldn't read config.toml ({e}); not touched")),
    };
    match enable_hooks_feature(&existing) {
        Ok(None) => Ok(("unchanged", "hooks feature already on".into())),
        Err(note) => Ok(("left", note)),
        Ok(Some(_)) if dry => Ok(("changed", "dry run: nothing written".into())),
        Ok(Some(text)) => {
            rightkit_fs::atomic_write_with(
                &path,
                rightkit_fs::AtomicOptions {
                    create_parents: true,
                    ..Default::default()
                },
                |file| file.write_all(text.as_bytes()),
            )
            .map_err(|e| e.to_string())?;
            Ok(("changed", "turned on [features] hooks".into()))
        }
    }
}

/// How long one `muse` command may run.
const MUSE_TIMEOUT: Duration = Duration::from_secs(30);
/// The id Muse gives the plugin: its name in `.claude-plugin/plugin.json`.
fn muse_plugin() -> &'static str {
    &brand().plugin_id
}

/// The Muse command line: where Muse installs itself, then the search path.
fn muse_binary() -> Option<PathBuf> {
    let names: &[&str] = if cfg!(windows) {
        &["muse.cmd", "muse.exe"]
    } else {
        &["muse"]
    };
    let mut dirs: Vec<PathBuf> = vec![rightkit_app_host::account_home().join(".local").join("bin")];
    if let Some(local) = std::env::var_os("LOCALAPPDATA").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(local).join("Programs").join("muse"));
    }
    if let Some(path) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path));
    }
    dirs.iter()
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
}

/// The Muse plugin folder that ships with this install: `Contents/Resources/plugins/muse/pulse` in
/// the Mac app, `plugins\muse\pulse` beside `Helpers` on Windows, `plugins/muse/pulse` in a
/// repository checkout, found from the folders above this executable.
fn muse_plugin_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .take(6)
        .flat_map(|dir| {
            [
                dir.join("plugins").join("muse").join(&brand().plugin_id),
                dir.join("Resources")
                    .join("plugins")
                    .join("muse")
                    .join(&brand().plugin_id),
            ]
        })
        .find(|folder| folder.join(".claude-plugin").join("plugin.json").is_file())
}

/// One `muse plugins …` run: whether it succeeded and what it printed (the start of it).
fn muse_run(muse: &Path, args: &[&str]) -> (bool, String) {
    let options = RunOptions::new(MUSE_TIMEOUT)
        .stdout_cap(64 * 1024)
        .stderr_cap(64 * 1024);
    match rightkit_child_process::run(muse, args, &options) {
        Ok(done) if done.timed_out => (
            false,
            format!(
                "Muse did not answer within {} seconds",
                MUSE_TIMEOUT.as_secs()
            ),
        ),
        Ok(done) => {
            let mut text = String::from_utf8_lossy(&done.stdout.bytes).into_owned();
            text.push(' ');
            text.push_str(&String::from_utf8_lossy(&done.stderr.bytes));
            let text: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
            (done.status.success(), text.chars().take(300).collect())
        }
        Err(e) => (false, format!("couldn't run Muse: {}", e.kind())),
    }
}

/// Install (and approve) or remove the host app plugin for Muse.
fn muse_one(uninstall: bool, dry: bool) -> (&'static str, String, PathBuf) {
    let Some(muse) = muse_binary() else {
        return (
            "skipped",
            "Muse isn't installed here".into(),
            PathBuf::from("muse"),
        );
    };
    let done = |action: &'static str, note: String| (action, note, muse.clone());
    if uninstall {
        if dry {
            return done("changed", "dry run: nothing changed".into());
        }
        let (ok, text) = muse_run(&muse, &["plugins", "remove", muse_plugin(), "--json"]);
        let gone = text.to_lowercase();
        return if ok {
            done("changed", "removed the app plugin from Muse".into())
        } else if gone.contains("not found")
            || gone.contains("not installed")
            || gone.contains("no such")
        {
            done("absent", "the app plugin isn't installed in Muse".into())
        } else {
            done("failed", format!("muse plugins remove failed: {text}"))
        };
    }
    // On Windows `muse` is `muse.cmd`; the same `muse plugins …` commands run through it. Not
    // verified on Windows: that Muse runs the plugin's `hooks/muse-<event>.exe` there (the Windows
    // payload ships plugin-launcher's the launcher under those names).
    let Some(folder) = muse_plugin_dir() else {
        return done(
            "failed",
            "the app plugin for Muse is missing from this install (plugins/muse/<plugin>); reinstall the app"
                .into(),
        );
    };
    if dry {
        return done("changed", "dry run: nothing written".into());
    }
    let folder_text = folder.display().to_string();
    let (installed, text) = muse_run(
        &muse,
        &[
            "plugins",
            "install",
            folder_text.as_str(),
            "--scope",
            "user",
            "--json",
        ],
    );
    // Already installed: refresh it from its source, which is this folder.
    let (installed, text) = if installed {
        (true, text)
    } else {
        let (updated, update_text) =
            muse_run(&muse, &["plugins", "update", muse_plugin(), "--json"]);
        (updated, if updated { update_text } else { text })
    };
    if !installed {
        return done("failed", format!("muse plugins install failed: {text}"));
    }
    let (approved, text) = muse_run(&muse, &["plugins", "approve", muse_plugin(), "--json"]);
    if !approved && !text.to_lowercase().contains("already") {
        return done(
            "failed",
            format!("installed, but muse plugins approve failed: {text}"),
        );
    }
    done(
        "changed",
        "installed and approved; restart open Muse chats to load its hooks".into(),
    )
}

/// The launcher the Windows installer lays down beside `the app command`.
fn launcher() -> &'static str {
    &brand().launcher
}

fn installed_plugins_path(config: &Path) -> PathBuf {
    config.join("plugins").join("installed_plugins.json")
}

/// Install folders Claude Code lists for a plugin named `<cli>` (any marketplace), existing ones
/// only, in this process's own Claude config folder.
pub fn claude_plugin_dirs() -> Vec<PathBuf> {
    claude_plugin_dirs_in(&claude_root(&Options::default()))
}

/// The same, in the Claude config folder `config`. The file is
/// `{"version": 2, "plugins": {"<name>@<marketplace>": [{"installPath": …}]}}`.
pub fn claude_plugin_dirs_in(config: &Path) -> Vec<PathBuf> {
    let Ok(text) = std::fs::read_to_string(installed_plugins_path(config)) else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(plugins) = root["plugins"].as_object() {
        for (key, installs) in plugins {
            if key.split('@').next() != Some(brand().plugin_id.as_str()) {
                continue;
            }
            for install in installs.as_array().into_iter().flatten() {
                if let Some(path) = install["installPath"].as_str().map(PathBuf::from)
                    && path.is_dir()
                    && !dirs.contains(&path)
                {
                    dirs.push(path);
                }
            }
        }
    }
    dirs
}

/// Whether Claude Code in one config folder can run the host app plugin's hooks, and if not, why.
#[derive(Debug, Clone, Default)]
pub struct PluginHealth {
    /// All three hold: `installed_plugins.json` lists `<plugin>@<marketplace>` with an existing
    /// install folder, `settings.json` `enabledPlugins` has it `true`, and that marketplace's
    /// source (`known_marketplaces.json`) exists.
    pub ok: bool,
    /// `installed_plugins.json` lists a `<plugin>@…` entry with an existing install folder at all.
    pub entry: bool,
    /// Why not, in a sentence that names the file; empty when `ok`.
    pub reason: String,
    /// The `<plugin>@<marketplace>` key looked at, when there was an entry.
    pub key: Option<String>,
    /// The only fault is `enabledPlugins` (entry and marketplace are fine): `repair_plugin` fixes it.
    pub only_disabled: bool,
}

/// Where `file` really lives when that is outside the config folder `config` (a link or junction
/// to another profile's file), else `None`. A profile's plugin state counts only from its own
/// files.
fn resolves_outside(config: &Path, file: &Path) -> Option<PathBuf> {
    let real_file = std::fs::canonicalize(file).ok()?;
    let real_config = std::fs::canonicalize(config).ok()?;
    (!real_file.starts_with(&real_config)).then_some(real_file)
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches('\u{feff}')).ok()
}

/// The marketplace source's path that must exist: a directory or file source's `path`, else the
/// `installLocation` Claude Code cloned a git or GitHub source into; `None` when it names neither.
fn marketplace_source(config: &Path, marketplace: &str) -> Option<PathBuf> {
    let known = read_json(&config.join("plugins").join("known_marketplaces.json"))?;
    let entry = &known[marketplace];
    if entry.is_null() {
        return None;
    }
    let source = &entry["source"];
    let direct = matches!(source["source"].as_str(), Some("directory" | "file"));
    let path = if direct {
        source["path"].as_str()
    } else {
        entry["installLocation"].as_str()
    };
    path.map(PathBuf::from)
}

/// Judge the host app plugin in the Claude config folder `config` (`PluginHealth`).
pub fn claude_plugin_health(config: &Path) -> PluginHealth {
    let listed = installed_plugins_path(config);
    if let Some(real) = resolves_outside(config, &listed) {
        return PluginHealth {
            reason: format!(
                "{} is a link to {}, not this profile's own plugin list",
                listed.display(),
                real.display()
            ),
            ..PluginHealth::default()
        };
    }
    let Some(installed) = read_json(&listed) else {
        return PluginHealth {
            reason: format!("{} is missing or unreadable", listed.display()),
            ..PluginHealth::default()
        };
    };
    let mut best = PluginHealth {
        reason: format!(
            "{} has no {}@<marketplace> entry with an existing install folder",
            listed.display(),
            brand().plugin_id
        ),
        ..PluginHealth::default()
    };
    let settings_path = config.join("settings.json");
    // enabledPlugins counts only from this profile's own settings.json, not a linked one.
    let settings = if resolves_outside(config, &settings_path).is_some() {
        None
    } else {
        read_json(&settings_path)
    };
    for (key, installs) in installed["plugins"].as_object().into_iter().flatten() {
        let Some(marketplace) = key.strip_prefix(&format!("{}@", brand().plugin_id)) else {
            continue;
        };
        let folder_ok = installs.as_array().into_iter().flatten().any(|i| {
            i["installPath"]
                .as_str()
                .is_some_and(|p| Path::new(p).is_dir())
        });
        if !folder_ok {
            best.key = Some(key.clone());
            best.reason = format!(
                "{key}: its installPath in {} does not exist",
                listed.display()
            );
            continue;
        }
        let enabled = settings
            .as_ref()
            .is_some_and(|s| s["enabledPlugins"][key.as_str()].as_bool() == Some(true));
        let source = marketplace_source(config, marketplace);
        let source_ok = source.as_ref().is_some_and(|p| p.exists());
        let fault = if !source_ok {
            Some(match source {
                Some(path) => format!(
                    "marketplace \"{marketplace}\" source {} does not exist (known_marketplaces.json)",
                    path.display()
                ),
                None => format!(
                    "marketplace \"{marketplace}\" is not in {}",
                    config
                        .join("plugins")
                        .join("known_marketplaces.json")
                        .display()
                ),
            })
        } else if !enabled {
            Some(format!(
                "{} does not have enabledPlugins \"{key}\" set to true",
                settings_path.display()
            ))
        } else {
            None
        };
        match fault {
            None => {
                return PluginHealth {
                    ok: true,
                    entry: true,
                    key: Some(key.clone()),
                    ..PluginHealth::default()
                };
            }
            Some(reason) => {
                best = PluginHealth {
                    ok: false,
                    entry: true,
                    only_disabled: source_ok && !enabled,
                    key: Some(key.clone()),
                    reason,
                };
            }
        }
    }
    best
}

/// Repair what a file edit can: set `enabledPlugins["<plugin>@…"]` to true in `settings.json` when the
/// plugin is listed and its marketplace exists but it is not enabled. A missing entry or a missing
/// marketplace folder is reported with the command that fixes it; the host app does not write Claude
/// Code's plugin registry or recreate a marketplace.
fn repair_plugin(config: &Path, dry: bool) -> Vec<Change> {
    let health = claude_plugin_health(config);
    let settings_path = config.join("settings.json");
    let change = |action: &str, note: String| Change {
        target: "claude-plugin-registration".to_string(),
        path: settings_path.display().to_string(),
        action: action.to_string(),
        note,
    };
    if health.ok {
        return vec![change(
            "unchanged",
            "plugin listed, enabled, marketplace present".into(),
        )];
    }
    if !health.entry {
        return vec![change(
            "skipped",
            format!(
                "{}; install it in that profile: CLAUDE_CONFIG_DIR={} claude plugin install {}@<marketplace>",
                health.reason,
                config.display(),
                brand().plugin_id
            ),
        )];
    }
    let Some(key) = health.key.clone().filter(|_| health.only_disabled) else {
        return vec![change(
            "failed",
            format!(
                "{}; fix the marketplace (CLAUDE_CONFIG_DIR={} claude plugin marketplace add <source>) or reinstall the plugin",
                health.reason,
                config.display()
            ),
        )];
    };
    // Never write through a link: a settings.json or installed_plugins.json that resolves outside
    // this profile belongs to another profile.
    if resolves_outside(config, &settings_path).is_some()
        || resolves_outside(config, &installed_plugins_path(config)).is_some()
    {
        super::sendlog::event(
            rightkit_logs::Level::Warn,
            "chat.plugin.repair_skipped",
            serde_json::json!({"reason": "linked_config"}),
        );
        return vec![change(
            "skipped",
            "settings.json or installed_plugins.json is a link to another profile; left alone"
                .into(),
        )];
    }
    let mut settings = match std::fs::read_to_string(&settings_path) {
        Ok(text) => {
            match serde_json::from_str::<serde_json::Value>(text.trim_start_matches('\u{feff}')) {
                Ok(value) if value.is_object() => value,
                _ => {
                    return vec![change(
                        "failed",
                        "settings.json is not a JSON object; left alone".into(),
                    )];
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => serde_json::json!({}),
        Err(e) => return vec![change("failed", format!("settings.json: {e}"))],
    };
    if dry {
        return vec![change("changed", format!("dry run: would enable {key}"))];
    }
    if !settings["enabledPlugins"].is_object() {
        settings["enabledPlugins"] = serde_json::json!({});
    }
    settings["enabledPlugins"][key.as_str()] = serde_json::json!(true);
    let text = serde_json::to_string_pretty(&settings).unwrap_or_default();
    match rightkit_fs::atomic_write(&settings_path, text.as_bytes()) {
        Ok(()) => vec![change(
            "changed",
            format!("enabled {key}; restart open Claude chats to load its hooks"),
        )],
        Err(e) => vec![change("failed", e.to_string())],
    }
}

/// One sentence per Claude profile whose the host app plugin cannot run its hooks, naming the profile and
/// the reason (`<cli> chat status`: "Claude plugin hook: never run" says which profile and why).
/// Empty when every profile found is healthy.
pub fn claude_plugin_problems() -> Vec<String> {
    claude_homes()
        .into_iter()
        .filter(|(home, _)| home.is_dir())
        .filter_map(|(home, profile)| {
            let health = claude_plugin_health(&home);
            (!health.ok).then(|| {
                format!(
                    "profile {} ({}): {}",
                    profile.as_deref().unwrap_or("main"),
                    home.display(),
                    health.reason
                )
            })
        })
        .collect()
}

/// Whether `dir/hooks/the launcher` is a Windows program. The marketplace copy of the plugin
/// carries the Mac's shell script under that same name, so a file that is there but starts with
/// anything but `MZ` cannot be run by Windows and the plugin's hooks never fire: the Dell's Claude
/// hook "never run". Presence alone is not health.
pub fn launcher_is_program(dir: &Path) -> bool {
    use std::io::Read;
    let mut first = [0u8; 2];
    std::fs::File::open(dir.join("hooks").join(launcher()))
        .and_then(|mut file| file.read_exact(&mut first))
        .is_ok()
        && &first == b"MZ"
}

/// The Windows launcher beside this `the app command`, if it is there.
fn launcher_source() -> Option<PathBuf> {
    let beside = std::env::current_exe().ok()?.parent()?.join(launcher());
    beside.is_file().then_some(beside)
}

fn repair_record_path() -> Option<PathBuf> {
    Some(
        rightkit_app_host::state_directory()
            .ok()?
            .join("chat")
            .join("claude-launcher-repair.json"),
    )
}

/// Remember a pass that copied the launcher into `n` folders (nothing is written when it copied none).
fn record_launcher_repair(changes: &[Change]) {
    let folders = changes
        .iter()
        .filter(|c| c.target == "claude-plugin" && c.action == "changed")
        .count();
    if folders == 0 {
        return;
    }
    if let Some(path) = repair_record_path() {
        let record = serde_json::json!({"atMs": now_ms() as u64, "folders": folders});
        let _ = rightkit_fs::atomic_write_with(
            &path,
            rightkit_fs::AtomicOptions {
                create_parents: true,
                ..Default::default()
            },
            |file| file.write_all(record.to_string().as_bytes()),
        );
    }
}

/// The last pass that copied the launcher: (unix milliseconds, folders it was copied into).
pub fn last_launcher_repair() -> Option<(u64, usize)> {
    let text = std::fs::read_to_string(repair_record_path()?).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some((
        value["atMs"].as_u64()?,
        usize::try_from(value["folders"].as_u64()?).ok()?,
    ))
}

/// Copy the launcher into (or remove it from) each installed the host app plugin; one change per folder.
fn launcher_changes(config: &Path, uninstall: bool, dry: bool) -> Vec<Change> {
    let dirs = claude_plugin_dirs_in(config);
    let change = |path: &Path, action: &str, note: String| Change {
        target: "claude-plugin".to_string(),
        path: path.display().to_string(),
        action: action.to_string(),
        note,
    };
    if dirs.is_empty() {
        return vec![change(
            &installed_plugins_path(config),
            "skipped",
            "the app plugin isn't installed in Claude Code".into(),
        )];
    }
    let source = launcher_source();
    dirs.iter()
        .map(|dir| {
            let dest = dir.join("hooks").join(launcher());
            if uninstall {
                return match (dest.is_file(), dry) {
                    (false, _) => change(&dest, "absent", "no launcher there".into()),
                    (true, true) => change(&dest, "changed", "dry run: nothing changed".into()),
                    (true, false) => match std::fs::remove_file(&dest) {
                        Ok(()) => change(&dest, "changed", "removed".into()),
                        Err(e) => change(&dest, "failed", e.to_string()),
                    },
                };
            }
            let Some(source) = &source else {
                return change(
                    &dest,
                    "failed",
                    format!(
                        "{} is missing beside {}.exe; reinstall the app",
                        launcher(),
                        brand().cli
                    ),
                );
            };
            let bytes = match std::fs::read(source) {
                Ok(bytes) => bytes,
                Err(e) => return change(&dest, "failed", format!("{}: {e}", source.display())),
            };
            // Same size first (a metadata read), then the bytes: copy only when missing or different.
            let same_size = std::fs::metadata(&dest).is_ok_and(|m| m.len() == bytes.len() as u64);
            if same_size && std::fs::read(&dest).ok().as_deref() == Some(bytes.as_slice()) {
                return change(&dest, "unchanged", "already installed".into());
            }
            if dry {
                return change(&dest, "changed", "dry run: nothing written".into());
            }
            match rightkit_fs::atomic_write(&dest, &bytes) {
                Ok(()) => change(&dest, "changed", "restart open chats to use it".into()),
                Err(e) => change(&dest, "failed", e.to_string()),
            }
        })
        .collect()
}

/// Install or remove the skill for the chosen targets (both when none chosen). A failing
/// target is reported as `failed` and the others still run.
pub fn apply(uninstall: bool, options: &Options) -> Result<Report, String> {
    let mut changes = Vec::new();
    for t in targets(options) {
        // `.codex\skills` is often a junction to `.claude\skills`: one file. Removing Codex's
        // skill by name must not delete Claude's; an install writes it once (the second target
        // finds it in place and reports `unchanged`).
        let shared = uninstall
            && t.name == "codex"
            && options.codex
            && !options.claude
            && same_skill_path(
                &t.skill,
                &skills_dir(&claude_root(options))
                    .join(skill_dir_name())
                    .join("SKILL.md"),
            );
        let result = if shared {
            Ok((
                "left",
                "left in place: Codex's skills folder is Claude's (a junction or link), so this is the Claude skill; remove it with --claude".into(),
            ))
        } else if uninstall {
            uninstall_one(&t, options.dry_run)
        } else {
            install_one(&t, options.dry_run)
        };
        let (action, note) = match result {
            Ok((action, note)) => (action, note),
            Err(e) => ("failed", e),
        };
        changes.push(Change {
            target: t.name.to_string(),
            path: t.skill.display().to_string(),
            action: action.to_string(),
            note,
        });
        if t.name == "codex" {
            let (action, note) = match hooks_one(uninstall, options.dry_run) {
                Ok(done) => done,
                Err(e) => ("failed", e),
            };
            changes.push(Change {
                target: "codex-hooks".to_string(),
                path: hooks_path().display().to_string(),
                action: action.to_string(),
                note,
            });
            if !uninstall && codex_home().is_dir() {
                let (action, note) = match features_one(options.dry_run) {
                    Ok(done) => done,
                    Err(e) => ("failed", e),
                };
                changes.push(Change {
                    target: "codex-features".to_string(),
                    path: codex_home().join("config.toml").display().to_string(),
                    action: action.to_string(),
                    note,
                });
            }
        }
    }
    if options.muse || no_target_named(options) {
        let (action, note, path) = muse_one(uninstall, options.dry_run);
        changes.push(Change {
            target: "muse".to_string(),
            path: path.display().to_string(),
            action: action.to_string(),
            note,
        });
    }
    // The plugin launcher is removed only when asked by name: removing the loose Claude skill must
    // not take the plugin's hooks with it.
    if cfg!(windows)
        && (options.claude_plugin
            || (!uninstall && (options.claude || (!options.codex && !options.muse))))
    {
        let launcher = launcher_changes(&claude_root(options), uninstall, options.dry_run);
        if !uninstall && !options.dry_run {
            record_launcher_repair(&launcher);
        }
        changes.extend(launcher);
    }
    // The plugin itself (not only the loose skill): listed, enabled and its marketplace present.
    if !uninstall && (options.claude || options.claude_plugin || no_target_named(options)) {
        changes.extend(repair_plugin(&claude_root(options), options.dry_run));
    }
    Ok(Report {
        dry_run: options.dry_run,
        changes,
    })
}

// ---- Discovery: `<cli> chat plugins`. The owner app runs it at start and now and then; it finds each
// harness on this computer and puts the host app's integration there with the same steps as `install`.
// Idempotent: a harness that already has it is left alone and nothing is asked. Muse is the one
// harness that asks first (its plugin's hooks run commands, so `muse plugins approve` trusts them):
// the answer is kept in `chat/plugin-discovery.json` and a "declined" is never asked again until
// `--reset-declined` (owner app runs it when the setting is turned on).

const DISCOVERY: &str = "plugin-discovery.json";
/// Harnesses that need the person's yes before anything is installed.
const ASKS: [&str; 1] = ["muse"];

/// One harness as discovery sees it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Harness {
    /// claude, codex or muse.
    pub harness: String,
    /// The profile a Claude config folder belongs to (Claude B), if it is another profile's.
    pub profile: Option<String>,
    /// The config folder (Claude, Codex) or command (Muse).
    pub home: String,
    pub found: bool,
    /// notFound, found (there, the host app's plugin not in it), installed, approvalPending, declined, failed.
    pub state: String,
    /// What this pass did: changed, unchanged, failed, skipped, or empty for a read-only view.
    pub action: String,
    pub note: String,
}

fn discovery_path() -> Option<PathBuf> {
    Some(
        rightkit_app_host::state_directory()
            .ok()?
            .join("chat")
            .join(DISCOVERY),
    )
}

fn read_discovery() -> serde_json::Value {
    discovery_path()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}))
}

fn write_discovery(state: &serde_json::Value) -> Result<(), String> {
    let path = discovery_path().ok_or("no the app state folder")?;
    rightkit_fs::atomic_write_with(
        &path,
        rightkit_fs::AtomicOptions {
            create_parents: true,
            ..Default::default()
        },
        |file| file.write_all(state.to_string().as_bytes()),
    )
    .map_err(|e| e.to_string())
}

/// "allowed" or "declined", once the person has answered for `harness`.
fn answer_of(state: &serde_json::Value, harness: &str) -> Option<String> {
    state["answers"][harness].as_str().map(String::from)
}

/// The plugin folder's version (`.claude-plugin/plugin.json`).
fn plugin_version(folder: &Path) -> Option<String> {
    let text = std::fs::read_to_string(folder.join(".claude-plugin").join("plugin.json")).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value["version"].as_str().map(String::from)
}

/// Every Claude config folder to look at: this process's own first (listed even when absent, so
/// "not found" has a path), then the other profiles' while `chatClaudeProfiles` is on
/// (`sessions_dirs` applies that switch).
fn claude_homes() -> Vec<(PathBuf, Option<String>)> {
    let mut homes: Vec<(PathBuf, Option<String>)> = Vec::new();
    for (sessions, label) in sessions_dirs() {
        if let Some(config) = sessions.parent()
            && !homes.iter().any(|(home, _)| home == config)
        {
            homes.push((config.to_path_buf(), label));
        }
    }
    if homes.is_empty() {
        homes.push((claude_root(&Options::default()), None));
    }
    let own = homes.remove(0);
    std::iter::once(own)
        .chain(homes.into_iter().filter(|(home, _)| home.is_dir()))
        .collect()
}

fn row(harness: &str, profile: Option<String>, home: &Path, found: bool, state: &str) -> Harness {
    Harness {
        harness: harness.to_string(),
        profile,
        home: home.display().to_string(),
        found,
        state: state.to_string(),
        action: String::new(),
        note: String::new(),
    }
}

/// Whether the host app skill file is in place in a Claude or Codex config folder.
fn skill_in_place(root: &Path) -> bool {
    std::fs::read_to_string(skills_dir(root).join(skill_dir_name()).join("SKILL.md")).ok()
        == Some(skill().to_string())
}

/// What discovery sees now, changing nothing: found, plugin installed, approval pending, declined.
pub fn view() -> Vec<Harness> {
    let state = read_discovery();
    let mut rows = Vec::new();
    for (home, profile) in claude_homes() {
        let found = home.is_dir();
        // Installed means the plugin's hooks can run: the loose skill file alone is not that (the
        // Dell's Claude B profile had the skill and no plugin, so its chats got no hook delivery).
        let health = if found {
            claude_plugin_health(&home)
        } else {
            PluginHealth::default()
        };
        let label = match (found, health.ok) {
            (false, _) => "notFound",
            (true, true) => "installed",
            (true, false) if health.entry => "failed",
            (true, false) => "found",
        };
        let mut out = row("claude", profile, &home, found, label);
        if found && !health.ok {
            out.note = health.reason;
        }
        rows.push(out);
    }
    let codex = codex_home();
    let found = codex.is_dir();
    let label = match (found, hooks_present() && skill_in_place(&codex)) {
        (false, _) => "notFound",
        (true, true) => "installed",
        (true, false) => "found",
    };
    rows.push(row("codex", None, &codex, found, label));
    let muse = muse_binary();
    let label = match (&muse, answer_of(&state, "muse").as_deref()) {
        (None, _) => "notFound",
        (Some(_), Some("declined")) => "declined",
        (Some(_), Some("allowed")) if state["muse"]["version"].is_string() => "installed",
        (Some(_), Some("allowed")) => "found",
        (Some(_), _) => "approvalPending",
    };
    rows.push(row(
        "muse",
        None,
        &muse.clone().unwrap_or_else(|| PathBuf::from("muse")),
        muse.is_some(),
        label,
    ));
    rows
}

/// The row for a target installed through `apply`: failed when any step failed, else installed.
fn applied(harness: &str, profile: Option<String>, home: &Path, changes: &[Change]) -> Harness {
    let failed = changes.iter().find(|c| c.action == "failed");
    let changed = changes
        .iter()
        .find(|c| matches!(c.action.as_str(), "changed" | "left"));
    let mut out = row(
        harness,
        profile,
        home,
        true,
        if failed.is_some() {
            "failed"
        } else {
            "installed"
        },
    );
    (out.action, out.note) = match (failed, changed) {
        (Some(c), _) => ("failed".into(), c.note.clone()),
        (None, Some(c)) => (c.action.clone(), c.note.clone()),
        (None, None) => ("unchanged".into(), String::new()),
    };
    out
}

/// Muse: nothing is installed until the person has said yes; a yes is installed once per plugin
/// version and folder, without running Muse again while nothing changed.
fn muse_discover(state: &mut serde_json::Value, dry: bool) -> Harness {
    let Some(binary) = muse_binary() else {
        return row("muse", None, Path::new("muse"), false, "notFound");
    };
    let mut out = row("muse", None, &binary, true, "approvalPending");
    match answer_of(state, "muse").as_deref() {
        Some("declined") => out.state = "declined".into(),
        Some("allowed") => {
            let folder = muse_plugin_dir();
            let current = folder
                .as_deref()
                .and_then(|dir| Some((plugin_version(dir)?, dir.display().to_string())));
            let recorded = &state["muse"];
            let same = current.as_ref().is_some_and(|(version, dir)| {
                recorded["version"].as_str() == Some(version.as_str())
                    && recorded["folder"].as_str() == Some(dir.as_str())
            });
            if same {
                out.state = "installed".into();
                out.action = "unchanged".into();
                out.note = "already installed".into();
            } else {
                let (action, note, _) = muse_one(false, dry);
                out.action = action.into();
                out.note = note;
                out.state = match action {
                    "changed" => "installed",
                    "failed" => "failed",
                    _ => "found",
                }
                .into();
                if action == "changed"
                    && !dry
                    && let Some((version, dir)) = current
                {
                    state["muse"] = serde_json::json!({"version": version, "folder": dir});
                }
            }
        }
        _ => {}
    }
    out
}

/// Finds each harness and installs the host app's integration where it is missing (Muse only after a yes).
/// A harness that is not there is reported `notFound`. The result is kept for `<cli> chat status`.
pub fn discover(dry: bool) -> Vec<Harness> {
    let mut state = read_discovery();
    let mut rows = Vec::new();
    for (home, profile) in claude_homes() {
        if !home.is_dir() {
            rows.push(row("claude", profile, &home, false, "notFound"));
            continue;
        }
        let options = Options {
            claude: true,
            dry_run: dry,
            claude_root: Some(home.clone()),
            ..Default::default()
        };
        rows.push(match apply(false, &options) {
            Ok(report) => {
                let mut out = applied("claude", profile, &home, &report.changes);
                // What was fixed, and what still stops the hooks, in the row's note.
                let fixed: Vec<String> = report
                    .changes
                    .iter()
                    .filter(|c| c.target == "claude-plugin-registration" && c.action == "changed")
                    .map(|c| c.note.clone())
                    .collect();
                let health = if dry {
                    PluginHealth::default()
                } else {
                    claude_plugin_health(&home)
                };
                if !dry && !health.ok && out.state != "failed" {
                    out.state = if health.entry { "failed" } else { "found" }.to_string();
                    out.note = health.reason;
                } else if !fixed.is_empty() {
                    out.note = fixed.join("; ");
                }
                out
            }
            Err(e) => {
                let mut out = row("claude", profile, &home, true, "failed");
                out.action = "failed".into();
                out.note = e;
                out
            }
        });
    }
    let codex = codex_home();
    if codex.is_dir() {
        let options = Options {
            codex: true,
            dry_run: dry,
            ..Default::default()
        };
        rows.push(match apply(false, &options) {
            Ok(report) => applied("codex", None, &codex, &report.changes),
            Err(e) => {
                let mut out = row("codex", None, &codex, true, "failed");
                out.action = "failed".into();
                out.note = e;
                out
            }
        });
    } else {
        rows.push(row("codex", None, &codex, false, "notFound"));
    }
    rows.push(muse_discover(&mut state, dry));
    if !dry {
        state["results"] = serde_json::json!(rows);
        state["atMs"] = serde_json::json!(now_ms() as u64);
        let _ = write_discovery(&state);
    }
    rows
}

/// Keeps the person's answer for a harness that asks (`allow` or decline).
pub fn answer(harness: &str, allow: bool) -> Result<(), String> {
    if !ASKS.contains(&harness) {
        return Err(format!(
            "{harness} is installed without asking; the ones that ask are: {}",
            ASKS.join(", ")
        ));
    }
    let mut state = read_discovery();
    state["answers"][harness] = serde_json::json!(if allow { "allowed" } else { "declined" });
    write_discovery(&state)
}

/// Forgets every "declined", so the next pass asks again (the setting was turned on).
pub fn reset_declined() -> Result<(), String> {
    let mut state = read_discovery();
    if let Some(answers) = state["answers"].as_object_mut() {
        answers.retain(|_, v| v.as_str() != Some("declined"));
    }
    write_discovery(&state)
}

/// `<cli> chat plugins [--status] [--dry-run] [--allow <harness>] [--decline <harness>]
/// [--reset-declined] [--json]`.
pub fn run_plugins(args: Vec<String>, machine: bool) -> Result<(), String> {
    let (mut status, mut dry, mut reset) = (false, false, false);
    let mut allow: Option<String> = None;
    let mut decline: Option<String> = None;
    let mut it = args.into_iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--status" => status = true,
            "--dry-run" => dry = true,
            "--reset-declined" => reset = true,
            "--allow" => allow = Some(it.next().ok_or("--allow needs a harness")?),
            "--decline" => decline = Some(it.next().ok_or("--decline needs a harness")?),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    if reset {
        reset_declined()?;
    }
    if let Some(harness) = &decline {
        answer(harness, false)?;
    }
    if let Some(harness) = &allow {
        answer(harness, true)?;
    }
    // A decline or a status question changes nothing in any harness; everything else looks and installs.
    let rows = if status || (decline.is_some() && allow.is_none()) {
        view()
    } else {
        discover(dry)
    };
    if machine {
        let pending: Vec<&str> = rows
            .iter()
            .filter(|r| r.state == "approvalPending")
            .map(|r| r.harness.as_str())
            .collect();
        println!(
            "{}",
            serde_json::json!({"harnesses": rows, "approvalPending": pending})
        );
    } else {
        for r in &rows {
            let profile = r
                .profile
                .as_deref()
                .map(|p| format!(" ({p})"))
                .unwrap_or_default();
            println!("{}{profile}: {} {} {}", r.harness, r.state, r.home, r.note);
        }
    }
    // The JSON carries each failure in its row; only the printed form turns one into an exit status.
    let failed = rows.iter().filter(|r| r.state == "failed").count();
    if failed > 0 && !machine {
        return Err(format!("{failed} harness(es) failed; see above"));
    }
    Ok(())
}

/// What `install --claude` and `status` say about messages to a busy Claude chat (`chatClaudeHookDelivery`).
pub fn claude_hook_delivery_line(store: &super::store::Store) -> String {
    match store.claude_hook_delivery_mode() {
        "on" => "Claude hook delivery: on (chatClaudeHookDelivery on); messages to a busy Claude chat go through the app plugin's hook.".to_string(),
        "off" => "Claude hook delivery: off (chatClaudeHookDelivery off); messages to a busy Claude chat use the session socket.".to_string(),
        _ if store.claude_plugin_hook_recent() => "Claude hook delivery: automatic, in use (the app plugin's hook ran here within 7 days).".to_string(),
        _ => "Claude hook delivery: automatic; it will be used once the app plugin's hook runs in a Claude chat on this computer.".to_string(),
    }
}

/// `<cli> chat install|uninstall [--claude] [--codex] [--muse] [--dry-run]`.
pub fn run(subcommand: &str, args: Vec<String>, machine: bool) -> Result<(), String> {
    let mut options = Options::default();
    for arg in &args {
        match arg.as_str() {
            "--claude" => options.claude = true,
            "--codex" => options.codex = true,
            "--muse" => options.muse = true,
            "--claude-plugin" => options.claude_plugin = true,
            "--dry-run" => options.dry_run = true,
            other => return Err(format!("unknown option: {other}")),
        }
    }
    let report = apply(subcommand == "uninstall", &options)?;
    if machine {
        println!("{}", serde_json::json!(report));
    } else {
        for c in &report.changes {
            println!("{}: {} {} {}", c.target, c.action, c.path, c.note);
        }
        let hooks_changed = report
            .changes
            .iter()
            .any(|c| c.target == "codex-hooks" && c.action == "changed");
        if subcommand != "uninstall" && hooks_changed && !report.dry_run {
            println!("{TRUST_NOTE}");
        }
        if subcommand != "uninstall"
            && options.claude
            && !report.dry_run
            && let Ok(store) = super::store::Store::open_default()
        {
            println!("{}", claude_hook_delivery_line(&store));
        }
    }
    let failed = report
        .changes
        .iter()
        .filter(|c| c.action == "failed")
        .count();
    if failed > 0 {
        return Err(format!("{failed} target(s) failed; see above"));
    }
    Ok(())
}
