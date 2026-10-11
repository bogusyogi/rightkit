//! Launch at login. Base: CodeRight `os_shell/autostart` (Linux XDG `.desktop`, Windows HKCU Run
//! through `reg.exe` with no console), plus the macOS LaunchAgent the donor lacked (HeardRight's
//! `login_launch` covered it only for its own bundle). The login launch receives
//! [`AutostartOptions::args`] (default `--background`), so it never starts a second engine.
//!
//! Migration. Earlier builds used `tauri-plugin-autostart`, which `auto-launch` writes under the
//! package name: `~/Library/LaunchAgents/<name>.plist` on macOS, the HKCU Run value `<name>` on
//! Windows, and `~/.config/autostart/<name>.desktop` elsewhere. [`migrate_plugin_entry`] enables the
//! shell's entry and removes that one, so an upgraded user never has two login items.
//!
//! The exact legacy names are [`LegacyAutostartNames`]. Read from `tauri-plugin-autostart` 2.5.1
//! (`src/lib.rs`), `auto-launch` 0.5.0 (`src/{windows,macos,linux}.rs`) and `tauri-codegen` 2.6.3
//! (`src/context.rs`), the versions HeardRight's lockfile pins for the first two. The entry name is
//! the plugin's `app_name` if the app set one, otherwise `PackageInfo::name`, which Tauri fills with
//! `tauri.conf.json` `productName` when present and the Cargo package name otherwise. For HeardRight
//! (`productName` `HeardRight`) that is `HeardRight`: the Windows Run value `HeardRight`, the macOS
//! LaunchAgent label `HeardRight` at `~/Library/LaunchAgents/HeardRight.plist`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
#[cfg(not(windows))]
use std::fs;

pub const BACKGROUND_ARG: &str = "--background";

/// How the login entry launches the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutostartOptions {
    /// Arguments the login launch receives. Default: `["--background"]`.
    pub args: Vec<String>,
    /// macOS LaunchAgent `LowPriorityIO`: yields the disk during the login storm (Spotlight,
    /// Gatekeeper). Ignored elsewhere. Default: off.
    pub low_priority_io: bool,
    /// Remove the entry `tauri-plugin-autostart` wrote, whenever this shell enables or disables login
    /// launch, so an upgraded user never has two login items. Default: off.
    pub migrate_from_plugin_entry: bool,
    /// Linux: written as `StartupWMClass` in the desktop entry so the shell matches the app's window
    /// class. `None` (the default) writes no such key. A value with control characters is ignored.
    pub startup_wm_class: Option<String>,
    /// Where the per-user config lives, instead of the environment (`RIGHTKIT_SHELL_HOME`,
    /// `XDG_CONFIG_HOME`, `HOME`). Linux: the XDG config directory, so the entry is
    /// `<config_home>/autostart/<id>.desktop`. macOS: the home directory, so the entry is
    /// `<config_home>/Library/LaunchAgents/<id>.autostart.plist`. For tests and sandboxes; `None`
    /// (the default) keeps the environment lookup. Ignored on Windows.
    pub config_home: Option<PathBuf>,
    /// macOS: report login launch as unsupported and create no LaunchAgent, for an app that handles
    /// login items itself. Ignored elsewhere. Default: off.
    pub macos_unsupported: bool,
}

/// True when `options` make login launch unsupported on the platform (`target_macos`).
pub fn reports_unsupported(options: &AutostartOptions, target_macos: bool) -> bool {
    target_macos && options.macos_unsupported
}

impl Default for AutostartOptions {
    fn default() -> Self {
        Self {
            args: vec![BACKGROUND_ARG.to_owned()],
            low_priority_io: false,
            migrate_from_plugin_entry: false,
            startup_wm_class: None,
            config_home: None,
            macos_unsupported: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutostartStatus {
    pub supported: bool,
    pub enabled: bool,
    /// Plist / desktop-entry path (None on Windows where the entry is a registry value).
    pub entry: Option<String>,
}

pub struct AutostartSpec {
    /// Reverse-DNS or short id; the LaunchAgent label and the registry value name.
    pub app_id: String,
    pub app_name: String,
    pub executable: PathBuf,
    pub options: AutostartOptions,
    /// The name `tauri-plugin-autostart` keyed its entry by. Set (by the shell, from the package name)
    /// only when [`AutostartOptions::migrate_from_plugin_entry`] is on.
    pub legacy_plugin_name: Option<String>,
}

/// What [`migrate_plugin_entry`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AutostartMigration {
    /// No `tauri-plugin-autostart` entry exists.
    NoLegacy,
    /// The legacy entry was replaced by the shell's entry and removed.
    Migrated,
}

/// The entries `tauri-plugin-autostart` writes for one entry name (see the module docs for where the
/// name comes from). Pure derivation, identical on every host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyAutostartNames {
    /// Windows: the value name under `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, verbatim.
    /// The plugin also writes a same-named value under
    /// `HKCU\...\Explorer\StartupApproved\Run` (Task Manager's enabled flag), which the migration
    /// leaves in place: it is inert once the Run value is gone.
    pub windows_run_value: String,
    /// macOS (LaunchAgent launcher, the default): the plist `Label`, verbatim.
    pub macos_launch_agent_label: String,
    /// macOS: the plist path relative to the user's home, `Library/LaunchAgents/<name>.plist`.
    pub macos_plist_relative: PathBuf,
    /// Linux: the desktop entry relative to the user's home, `.config/autostart/<name>.desktop`
    /// (the plugin does not honour `XDG_CONFIG_HOME`).
    pub linux_desktop_relative: PathBuf,
}

impl LegacyAutostartNames {
    /// Derive the entries for the plugin's entry name.
    pub fn for_entry_name(name: &str) -> Self {
        Self {
            windows_run_value: name.to_owned(),
            macos_launch_agent_label: name.to_owned(),
            macos_plist_relative: PathBuf::from("Library/LaunchAgents")
                .join(format!("{name}.plist")),
            linux_desktop_relative: PathBuf::from(".config/autostart")
                .join(format!("{name}.desktop")),
        }
    }
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Program arguments for a macOS login item with the default `--background` argument.
pub fn macos_program_arguments(exe: &Path) -> Vec<String> {
    macos_program_arguments_with(exe, &[BACKGROUND_ARG.to_owned()])
}

/// Program arguments for a macOS login item. Inside a `.app` bundle the bundle is launched through
/// LaunchServices (`open -g -a <bundle> --args ...`) so it behaves like a Dock launch.
pub fn macos_program_arguments_with(exe: &Path, args: &[String]) -> Vec<String> {
    let s = exe.to_string_lossy();
    let mut out: Vec<String>;
    let in_bundle = s.find(".app/Contents/MacOS/");
    if let Some(idx) = in_bundle {
        let bundle = &s[..idx + 4];
        out = vec![
            "/usr/bin/open".into(),
            "-g".into(),
            "-a".into(),
            bundle.into(),
        ];
        if !args.is_empty() {
            out.push("--args".into());
        }
    } else {
        out = vec![s.into_owned()];
    }
    out.extend(args.iter().cloned());
    out
}

pub fn macos_plist(label: &str, args: &[String]) -> String {
    macos_plist_with(label, args, false)
}

/// A LaunchAgent plist. `low_priority_io` adds `LowPriorityIO`.
pub fn macos_plist_with(label: &str, args: &[String], low_priority_io: bool) -> String {
    let items: String = args
        .iter()
        .map(|a| format!("    <string>{}</string>\n", xml(a)))
        .collect();
    let low = if low_priority_io {
        "  <key>LowPriorityIO</key>\n  <true/>\n"
    } else {
        ""
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{}</string>\n  <key>ProgramArguments</key>\n  <array>\n{items}  </array>\n  <key>RunAtLoad</key>\n  <true/>\n  <key>ProcessType</key>\n  <string>Interactive</string>\n{low}</dict>\n</plist>\n",
        xml(label)
    )
}

fn desktop_exec_quote(v: &str) -> String {
    if !v
        .chars()
        .any(|c| matches!(c, ' ' | '\t' | '\n' | '"' | '$' | '`' | '\\'))
    {
        return v.to_owned();
    }
    let mut out = String::from("\"");
    for c in v.chars() {
        if matches!(c, '"' | '$' | '`' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

pub fn linux_desktop_entry(name: &str, exe: &Path) -> String {
    linux_desktop_entry_with(name, exe, &[BACKGROUND_ARG.to_owned()])
}

/// A desktop entry whose `Exec` line carries `args`.
pub fn linux_desktop_entry_with(name: &str, exe: &Path, args: &[String]) -> String {
    linux_desktop_entry_for(name, exe, args, None)
}

/// [`linux_desktop_entry_with`] plus `StartupWMClass=<wm_class>` when it is a usable single-line
/// value (blank values and values with control characters write no key).
pub fn linux_desktop_entry_for(
    name: &str,
    exe: &Path,
    args: &[String],
    wm_class: Option<&str>,
) -> String {
    // Control characters are refused on the raw value: trimming first would silently drop a
    // trailing CR or LF and write a key the caller did not validate.
    let class = wm_class
        .filter(|c| !c.chars().any(char::is_control))
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(|c| format!("StartupWMClass={c}\n"))
        .unwrap_or_default();
    let mut exec = desktop_exec_quote(&exe.to_string_lossy());
    for arg in args {
        exec.push(' ');
        exec.push_str(&desktop_exec_quote(arg));
    }
    format!("[Desktop Entry]\nType=Application\nName={name}\nExec={exec}\nTerminal=false\n{class}X-GNOME-Autostart-enabled=true\n")
}

/// The Windows Run command: the quoted executable, then each argument (quoted when it has spaces).
pub fn windows_run_command(exe: &Path, args: &[String]) -> String {
    let mut command = format!("\"{}\"", exe.display());
    for arg in args {
        command.push(' ');
        if arg.contains(' ') || arg.contains('"') {
            command.push('"');
            command.push_str(&arg.replace('"', "\\\""));
            command.push('"');
        } else {
            command.push_str(arg);
        }
    }
    command
}

/// Home directory for login entries. `RIGHTKIT_SHELL_HOME` redirects them (isolated QA runs and
/// sandboxes must never write the real user's LaunchAgents / autostart / config).
// Used by the macOS (LaunchAgents) and other-Unix (XDG autostart) paths; Windows uses the registry.
#[cfg_attr(windows, allow(dead_code))]
fn home() -> Option<PathBuf> {
    std::env::var_os("RIGHTKIT_SHELL_HOME")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("HOME"))
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The login-item file for `stem` on this platform (None on Windows, where entries are registry
/// values).
#[cfg(not(windows))]
fn login_item_path(stem: &str, config_home: Option<&Path>) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        config_home
            .map(Path::to_path_buf)
            .or_else(home)
            .map(|h| h.join("Library/LaunchAgents").join(format!("{stem}.plist")))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        if let Some(cfg) = config_home {
            return Some(cfg.join("autostart").join(format!("{stem}.desktop")));
        }
        let cfg = std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| home().map(|h| h.join(".config")))?;
        Some(cfg.join("autostart").join(format!("{stem}.desktop")))
    }
}

/// Where `tauri-plugin-autostart` put its login item for `stem` (None on Windows).
#[cfg(not(windows))]
fn legacy_login_item_path(stem: &str, config_home: Option<&Path>) -> Option<PathBuf> {
    let names = LegacyAutostartNames::for_entry_name(stem);
    #[cfg(target_os = "macos")]
    {
        config_home
            .map(Path::to_path_buf)
            .or_else(home)
            .map(|h| h.join(names.macos_plist_relative))
    }
    #[cfg(not(target_os = "macos"))]
    {
        match config_home {
            // The injected directory is the config directory itself: `autostart/<name>.desktop`.
            Some(cfg) => Some(cfg.join("autostart").join(format!("{stem}.desktop"))),
            None => home().map(|h| h.join(names.linux_desktop_relative)),
        }
    }
}

/// The stem of the shell's own entry: `<app_id>.autostart` on macOS (the LaunchAgent label), the
/// app id elsewhere.
fn our_stem(spec: &AutostartSpec) -> String {
    #[cfg(target_os = "macos")]
    {
        format!("{}.autostart", spec.app_id)
    }
    #[cfg(not(target_os = "macos"))]
    {
        spec.app_id.clone()
    }
}

/// Where the shell's login entry lives on this platform (None on Windows).
pub fn entry_path(spec: &AutostartSpec) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        let _ = spec;
        None
    }
    #[cfg(not(windows))]
    {
        login_item_path(&our_stem(spec), spec.options.config_home.as_deref())
    }
}

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const STARTUP_APPROVED_KEY: &str =
    r"HKCU\Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run";

/// The two registry keys Windows login items live under. Only [`migrate_plugin_entry_in`] takes
/// them, so tests can run the migration against a scratch key; everything else uses the real keys.
/// Ignored off Windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryKeys {
    /// The `Run` key holding the command (`HKCU\...\CurrentVersion\Run`).
    pub run: String,
    /// The `StartupApproved\Run` key holding Task Manager's enabled or disabled flag per value name
    /// (a 12-byte `REG_BINARY`; `tauri-plugin-autostart` writes it as well as the Run value).
    pub startup_approved: String,
}

#[allow(clippy::derivable_impls)] // the Windows default is the real keys
impl Default for RegistryKeys {
    fn default() -> Self {
        #[cfg(windows)]
        {
            Self {
                run: RUN_KEY.to_owned(),
                startup_approved: STARTUP_APPROVED_KEY.to_owned(),
            }
        }
        #[cfg(not(windows))]
        {
            Self {
                run: String::new(),
                startup_approved: String::new(),
            }
        }
    }
}

/// True when a `StartupApproved` value says the user disabled the item in Task Manager. The rule is
/// `auto-launch` 0.5.0's own (`windows.rs`, `last_eight_bytes_all_zeros`): enabled when the last
/// eight bytes are all zero (`02 00 00 00 ...`), disabled when they hold the disable timestamp
/// (`03 00 00 00` plus a FILETIME). A value shorter than eight bytes is not a Task Manager record
/// and counts as enabled.
pub fn startup_approved_is_disabled(bytes: &[u8]) -> bool {
    bytes.len() >= 8 && bytes.iter().rev().take(8).any(|b| *b != 0)
}

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg.exe")
        .creation_flags(0x0800_0000)
        .args(args)
        .output()
        .map_err(|e| format!("run reg.exe: {e}"))
}

/// The bytes of a `REG_BINARY` value, or None when the key or value is absent or not binary.
#[cfg(windows)]
fn reg_read_binary(key: &str, name: &str) -> Option<Vec<u8>> {
    let out = reg(&["QUERY", key, "/v", name]).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("REG_BINARY"))?;
    let hex = line.split("REG_BINARY").nth(1)?.trim();
    if hex.len() % 2 != 0 {
        return None;
    }
    (0..hex.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(hex.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(windows)]
fn reg_write_binary(key: &str, name: &str, bytes: &[u8]) -> Result<(), String> {
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    let out = reg(&["ADD", key, "/v", name, "/t", "REG_BINARY", "/d", &hex, "/f"])?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

pub fn status(spec: &AutostartSpec) -> AutostartStatus {
    if reports_unsupported(&spec.options, cfg!(target_os = "macos")) {
        return AutostartStatus {
            supported: false,
            enabled: false,
            entry: None,
        };
    }
    #[cfg(windows)]
    {
        let enabled = reg(&["QUERY", RUN_KEY, "/v", &spec.app_id])
            .map(|o| o.status.success())
            .unwrap_or(false);
        AutostartStatus {
            supported: true,
            enabled,
            entry: None,
        }
    }
    #[cfg(not(windows))]
    {
        match entry_path(spec) {
            Some(p) => AutostartStatus {
                supported: true,
                enabled: p.is_file(),
                entry: Some(p.to_string_lossy().into_owned()),
            },
            None => AutostartStatus {
                supported: false,
                enabled: false,
                entry: None,
            },
        }
    }
}

/// Enable or disable login launch. With [`AutostartOptions::migrate_from_plugin_entry`] and a
/// legacy name on the spec, the legacy entry is removed as well.
pub fn set_enabled(spec: &AutostartSpec, enabled: bool) -> Result<AutostartStatus, String> {
    if reports_unsupported(&spec.options, cfg!(target_os = "macos")) {
        // Nothing was ever written, so turning it off has nothing to undo.
        return if enabled {
            Err("login launch is not supported by this app on macOS".into())
        } else {
            Ok(status(spec))
        };
    }
    if enabled {
        write_entry(&RegistryKeys::default(), spec)?;
    } else {
        remove_entry(spec)?;
    }
    if spec.options.migrate_from_plugin_entry {
        if let Some(legacy) = spec.legacy_plugin_name.as_deref() {
            remove_legacy(&RegistryKeys::default(), spec, legacy)?;
        }
    }
    Ok(status(spec))
}

/// One-time migration from `tauri-plugin-autostart`: when the legacy entry `legacy_name` exists,
/// write the shell's entry with the configured options and remove the legacy one. The user's
/// login-at-start choice is kept: a legacy entry means login launch was on.
pub fn migrate_plugin_entry(
    spec: &AutostartSpec,
    legacy_name: &str,
) -> Result<AutostartMigration, String> {
    migrate_plugin_entry_in(&RegistryKeys::default(), spec, legacy_name)
}

/// [`migrate_plugin_entry`] against explicit registry keys.
///
/// On Windows the plugin wrote two values under `legacy_name`: the command under `Run`, and Task
/// Manager's flag under `StartupApproved\Run`. Migration carries both. A legacy flag that says
/// the user disabled the item is copied to the shell's value name, so a user who turned login launch
/// off in Task Manager stays off; an enabled or absent flag changes nothing. The legacy flag value
/// is then removed along with the legacy command (unless the shell's entry uses the same name, in
/// which case the flag already applies to it).
pub fn migrate_plugin_entry_in(
    keys: &RegistryKeys,
    spec: &AutostartSpec,
    legacy_name: &str,
) -> Result<AutostartMigration, String> {
    if reports_unsupported(&spec.options, cfg!(target_os = "macos")) {
        // This host declined macOS login items: write nothing, leave the legacy entry alone.
        return Ok(AutostartMigration::NoLegacy);
    }
    if !legacy_present(keys, legacy_name, spec.options.config_home.as_deref()) {
        return Ok(AutostartMigration::NoLegacy);
    }
    #[cfg(windows)]
    let carried = reg_read_binary(&keys.startup_approved, legacy_name)
        .filter(|bytes| startup_approved_is_disabled(bytes));
    write_entry(keys, spec)?;
    #[cfg(windows)]
    if let Some(bytes) = carried {
        reg_write_binary(&keys.startup_approved, &spec.app_id, &bytes)?;
    }
    remove_legacy(keys, spec, legacy_name)?;
    Ok(AutostartMigration::Migrated)
}

fn write_entry(keys: &RegistryKeys, spec: &AutostartSpec) -> Result<(), String> {
    #[cfg(not(windows))]
    let _ = keys;
    #[cfg(windows)]
    {
        let command = windows_run_command(&spec.executable, &spec.options.args);
        let out = reg(&[
            "ADD",
            &keys.run,
            "/v",
            &spec.app_id,
            "/t",
            "REG_SZ",
            "/d",
            &command,
            "/f",
        ])?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let Some(path) = entry_path(spec) else {
            return Ok(());
        };
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        #[cfg(target_os = "macos")]
        let body = macos_plist_with(
            &our_stem(spec),
            &macos_program_arguments_with(&spec.executable, &spec.options.args),
            spec.options.low_priority_io,
        );
        #[cfg(not(target_os = "macos"))]
        let body = linux_desktop_entry_for(
            &spec.app_name,
            &spec.executable,
            &spec.options.args,
            spec.options.startup_wm_class.as_deref(),
        );
        fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))
    }
}

fn remove_entry(spec: &AutostartSpec) -> Result<(), String> {
    #[cfg(windows)]
    {
        // A missing value is already the desired state.
        let _ = reg(&["DELETE", RUN_KEY, "/v", &spec.app_id, "/f"]);
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let Some(path) = entry_path(spec) else {
            return Ok(());
        };
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(format!("remove {}: {e}", path.display())),
        }
    }
}

fn legacy_present(keys: &RegistryKeys, legacy_name: &str, config_home: Option<&Path>) -> bool {
    #[cfg(not(windows))]
    let _ = keys;
    #[cfg(windows)]
    let _ = config_home;
    #[cfg(windows)]
    {
        reg(&["QUERY", &keys.run, "/v", legacy_name])
            .map(|o| o.status.success())
            .unwrap_or(false)
    }
    #[cfg(not(windows))]
    {
        legacy_login_item_path(legacy_name, config_home).is_some_and(|p| p.exists())
    }
}

/// Remove the legacy entry, unless it is the same entry as the shell's own (then it was overwritten).
fn remove_legacy(
    keys: &RegistryKeys,
    spec: &AutostartSpec,
    legacy_name: &str,
) -> Result<bool, String> {
    #[cfg(not(windows))]
    let _ = keys;
    #[cfg(windows)]
    {
        if legacy_name == spec.app_id
            || !legacy_present(keys, legacy_name, spec.options.config_home.as_deref())
        {
            return Ok(false);
        }
        let out = reg(&["DELETE", &keys.run, "/v", legacy_name, "/f"])?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
        // The flag is inert without the command; a missing flag is already the desired state.
        let _ = reg(&["DELETE", &keys.startup_approved, "/v", legacy_name, "/f"]);
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        let Some(legacy) = legacy_login_item_path(legacy_name, spec.options.config_home.as_deref())
        else {
            return Ok(false);
        };
        if entry_path(spec).as_ref() == Some(&legacy) || !legacy.exists() {
            return Ok(false);
        }
        fs::remove_file(&legacy).map_err(|e| format!("remove {}: {e}", legacy.display()))?;
        Ok(true)
    }
}
