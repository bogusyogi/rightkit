//! Launch at login. Base: CodeRight `os_shell/autostart` (Linux XDG `.desktop`, Windows HKCU Run
//! through `reg.exe` with no console), plus the macOS LaunchAgent the donor lacked (HeardRight's
//! `login_launch` covered it only for its own bundle). Autostart launches the normal app with
//! `--background`; it never starts a second engine.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const BACKGROUND_ARG: &str = "--background";

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
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Program arguments for a macOS login item. Inside a `.app` bundle the bundle is launched
/// through LaunchServices (`open -a ... --args --background`) so it behaves like a Dock launch.
pub fn macos_program_arguments(exe: &Path) -> Vec<String> {
    let s = exe.to_string_lossy();
    if let Some(idx) = s.find(".app/Contents/MacOS/") {
        let bundle = &s[..idx + 4];
        return vec!["/usr/bin/open".into(), "-g".into(), "-a".into(), bundle.into(), "--args".into(), BACKGROUND_ARG.into()];
    }
    vec![s.into_owned(), BACKGROUND_ARG.into()]
}

pub fn macos_plist(label: &str, args: &[String]) -> String {
    let items: String = args.iter().map(|a| format!("    <string>{}</string>\n", xml(a))).collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n  <key>Label</key>\n  <string>{}</string>\n  <key>ProgramArguments</key>\n  <array>\n{items}  </array>\n  <key>RunAtLoad</key>\n  <true/>\n  <key>ProcessType</key>\n  <string>Interactive</string>\n</dict>\n</plist>\n",
        xml(label)
    )
}

fn desktop_exec_quote(v: &str) -> String {
    if !v.chars().any(|c| matches!(c, ' ' | '\t' | '\n' | '"' | '$' | '`' | '\\')) {
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
    format!("[Desktop Entry]\nType=Application\nName={name}\nExec={} {BACKGROUND_ARG}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n", desktop_exec_quote(&exe.to_string_lossy()))
}

/// Home directory for login entries. `RIGHTKIT_SHELL_HOME` redirects them (isolated QA runs and
/// sandboxes must never write the real user's LaunchAgents / autostart / config).
fn home() -> Option<PathBuf> {
    std::env::var_os("RIGHTKIT_SHELL_HOME").filter(|v| !v.is_empty()).or_else(|| std::env::var_os("HOME")).or_else(|| std::env::var_os("USERPROFILE")).map(PathBuf::from)
}

/// Where the login entry lives on this platform (None on Windows).
pub fn entry_path(spec: &AutostartSpec) -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        home().map(|h| h.join("Library/LaunchAgents").join(format!("{}.autostart.plist", spec.app_id)))
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let cfg = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| home().map(|h| h.join(".config")))?;
        Some(cfg.join("autostart").join(format!("{}.desktop", spec.app_id)))
    }
    #[cfg(windows)]
    {
        let _ = spec;
        None
    }
}

#[cfg(windows)]
fn reg(args: &[&str]) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new("reg.exe").creation_flags(0x0800_0000).args(args).output().map_err(|e| format!("run reg.exe: {e}"))
}

pub fn status(spec: &AutostartSpec) -> AutostartStatus {
    #[cfg(windows)]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let enabled = reg(&["QUERY", key, "/v", &spec.app_id]).map(|o| o.status.success()).unwrap_or(false);
        return AutostartStatus { supported: true, enabled, entry: None };
    }
    #[cfg(not(windows))]
    {
        match entry_path(spec) {
            Some(p) => AutostartStatus { supported: true, enabled: p.is_file(), entry: Some(p.to_string_lossy().into_owned()) },
            None => AutostartStatus { supported: false, enabled: false, entry: None },
        }
    }
}

pub fn set_enabled(spec: &AutostartSpec, enabled: bool) -> Result<AutostartStatus, String> {
    #[cfg(windows)]
    {
        let key = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
        let command = format!("\"{}\" {BACKGROUND_ARG}", spec.executable.display());
        let out = if enabled {
            reg(&["ADD", key, "/v", &spec.app_id, "/t", "REG_SZ", "/d", &command, "/f"])?
        } else {
            reg(&["DELETE", key, "/v", &spec.app_id, "/f"])?
        };
        if !out.status.success() && enabled {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
        return Ok(status(spec));
    }
    #[cfg(not(windows))]
    {
        let Some(path) = entry_path(spec) else { return Ok(AutostartStatus { supported: false, enabled: false, entry: None }) };
        if !enabled {
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(format!("remove {}: {e}", path.display())),
            }
            return Ok(status(spec));
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
        }
        #[cfg(target_os = "macos")]
        let body = macos_plist(&format!("{}.autostart", spec.app_id), &macos_program_arguments(&spec.executable));
        #[cfg(not(target_os = "macos"))]
        let body = linux_desktop_entry(&spec.app_name, &spec.executable);
        fs::write(&path, body).map_err(|e| format!("write {}: {e}", path.display()))?;
        Ok(status(spec))
    }
}
