//! macOS desktop facts and input for journeys that deliver text into *other* applications and read back
//! what landed with the Accessibility API.
//!
//! The verbs are `osascript` (System Events), `open`, `pbcopy`/`pbpaste`, `pgrep`, `mdls` and
//! `defaults`. They run in the journey process as the real user, never inside the QA app's workspace.
//!
//! HARD RULE, kept from the script this replaces: nothing here quits, closes, opens a new window in,
//! or selects-all-and-deletes a target application. The targets are the operator's real apps. There is
//! deliberately no `quit` and no select-all helper in this module.
#![cfg(target_os = "macos")]

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Sleep for `secs` seconds.
pub fn sleep(secs: f64) {
    std::thread::sleep(Duration::from_secs_f64(secs));
}

/// An AppleScript string literal for `value`.
pub fn quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// Run an AppleScript; the trimmed standard output, or the error text.
pub fn osascript(script: &str) -> Result<String, String> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| format!("osascript: {e}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| format!("{program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} {args:?} exited with {status}"))
    }
}

fn output_of(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Whether an application named `app` is installed (`open -Ra`).
pub fn is_installed(app: &str) -> bool {
    run("open", &["-Ra", app]).is_ok()
}

/// Whether a process named `app` is running, as System Events lists processes.
pub fn is_running(app: &str) -> bool {
    osascript(&format!(
        "tell application \"System Events\" to (name of processes) contains {}",
        quote(app)
    ))
    .map(|value| value == "true")
    .unwrap_or(false)
}

/// Open `app` (at `url` when given) and bring it to the front.
pub fn launch(app: &str, url: Option<&str>) -> Result<(), String> {
    match url {
        Some(url) => run("open", &["-a", app, url])?,
        None => run("open", &["-a", app])?,
    }
    osascript(&format!("tell application {} to activate", quote(app)))?;
    sleep(1.0);
    Ok(())
}

/// Make the process with `bundle_id` frontmost.
pub fn focus(bundle_id: &str) -> Result<(), String> {
    osascript(&format!(
        "tell application \"System Events\" to set frontmost of first process whose bundle identifier is {} to true",
        quote(bundle_id)
    ))
    .map(|_| ())
}

/// Type `value` into the frontmost app, with `modifier` held when given (`command`, `control`).
pub fn key(value: &str, modifier: Option<&str>) -> Result<(), String> {
    let suffix = modifier
        .map(|m| format!(" using {m} down"))
        .unwrap_or_default();
    osascript(&format!(
        "tell application \"System Events\" to keystroke {}{suffix}",
        quote(value)
    ))
    .map(|_| ())
}

/// Press a key by key code (Tab is 48, Escape is 53).
pub fn key_code(code: u32) -> Result<(), String> {
    osascript(&format!(
        "tell application \"System Events\" to key code {code}"
    ))
    .map(|_| ())
}

/// The AX value of the focused element of the frontmost process; empty when it cannot be read.
pub fn read_focused_ax() -> String {
    let script = [
        "tell application \"System Events\"",
        "set frontProcess to first process whose frontmost is true",
        "try",
        "set focusedElement to value of attribute \"AXFocusedUIElement\" of frontProcess",
        "set focusedValue to value of attribute \"AXValue\" of focusedElement",
        "return focusedValue as text",
        "on error",
        "return \"\"",
        "end try",
        "end tell",
    ]
    .join("\n");
    osascript(&script).unwrap_or_default()
}

/// Role and subrole of the focused element, and whether it is a secure text field.
#[derive(Clone, Debug, Default)]
pub struct FocusedField {
    /// `AXRole`.
    pub role: String,
    /// `AXSubrole`.
    pub subrole: String,
    /// Role or subrole names a secure text field.
    pub secure: bool,
}

/// Role, subrole and secure flag of the focused element of the frontmost process; empty when unreadable.
pub fn read_focused_ax_metadata() -> FocusedField {
    let script = [
        "tell application \"System Events\"",
        "set frontProcess to first process whose frontmost is true",
        "set focusedElement to value of attribute \"AXFocusedUIElement\" of frontProcess",
        "try",
        "set roleName to value of attribute \"AXRole\" of focusedElement as text",
        "on error",
        "set roleName to \"\"",
        "end try",
        "try",
        "set subroleName to value of attribute \"AXSubrole\" of focusedElement as text",
        "on error",
        "set subroleName to \"\"",
        "end try",
        "return roleName & \"|\" & subroleName",
        "end tell",
    ]
    .join("\n");
    match osascript(&script) {
        Ok(text) => {
            let mut parts = text.split('|');
            let role = parts.next().unwrap_or("").to_string();
            let subrole = parts.next().unwrap_or("").to_string();
            let secure = format!("{role} {subrole}")
                .to_lowercase()
                .contains("secure");
            FocusedField {
                role,
                subrole,
                secure,
            }
        }
        Err(_) => FocusedField::default(),
    }
}

/// The frontmost process.
#[derive(Clone, Debug, Default)]
pub struct Frontmost {
    /// Process name.
    pub name: String,
    /// Bundle identifier.
    pub bundle_id: String,
    /// Unix process id, when readable and non-zero.
    pub pid: Option<u32>,
}

/// Name, bundle id and pid of the frontmost process; empty when unreadable.
pub fn read_frontmost_identity() -> Frontmost {
    let script = [
        "tell application \"System Events\"",
        "set frontProcess to first process whose frontmost is true",
        "return (name of frontProcess) & \"|\" & (bundle identifier of frontProcess) & \"|\" & (unix id of frontProcess)",
        "end tell",
    ]
    .join("\n");
    match osascript(&script) {
        Ok(text) => {
            let mut parts = text.split('|');
            Frontmost {
                name: parts.next().unwrap_or("").to_string(),
                bundle_id: parts.next().unwrap_or("").to_string(),
                pid: parts
                    .next()
                    .and_then(|p| p.trim().parse().ok())
                    .filter(|p| *p != 0),
            }
        }
        Err(_) => Frontmost::default(),
    }
}

/// Click a fraction of the target window, near its bottom: where a chat composer sits. Used to put
/// keyboard focus in a composer the script cannot reach with the AX API alone.
pub fn click_composer_region(
    bundle_id: &str,
    x_fraction: f64,
    bottom_inset: f64,
) -> Result<(), String> {
    let script = [
        "tell application \"System Events\"".to_string(),
        format!(
            "set targetProcess to first process whose bundle identifier is {}",
            quote(bundle_id)
        ),
        "set frontmost of targetProcess to true".to_string(),
        "set windowPosition to position of window 1 of targetProcess".to_string(),
        "set windowSize to size of window 1 of targetProcess".to_string(),
        format!(
            "set clickX to (item 1 of windowPosition) + ((item 1 of windowSize) * {x_fraction})"
        ),
        format!(
            "set clickY to (item 2 of windowPosition) + (item 2 of windowSize) - {bottom_inset}"
        ),
        "click at {clickX, clickY}".to_string(),
        "end tell".to_string(),
    ]
    .join("\n");
    osascript(&script)?;
    sleep(0.3);
    Ok(())
}

/// The URL of the front browser tab, or empty.
pub fn read_active_browser_url(app: &str) -> String {
    let expression = if app == "Safari" {
        "tell application \"Safari\" to get URL of current tab of front window"
    } else {
        "tell application \"Google Chrome\" to get URL of active tab of front window"
    };
    osascript(expression).unwrap_or_default()
}

/// The text on the pasteboard (`pbpaste`), or empty.
pub fn read_clipboard() -> String {
    Command::new("pbpaste")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

/// Put `value` on the pasteboard (`pbcopy`).
pub fn copy_clipboard(value: &str) -> Result<(), String> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("pbcopy: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "pbcopy: no stdin".to_string())?
        .write_all(value.as_bytes())
        .map_err(|e| format!("pbcopy: {e}"))?;
    let status = child.wait().map_err(|e| format!("pbcopy: {e}"))?;
    status
        .success()
        .then_some(())
        .ok_or_else(|| format!("pbcopy exited with {status}"))
}

/// The bundle identifier of the `.app` at `app_path`: Spotlight first, then the Info.plist.
pub fn read_bundle_id(app_path: &std::path::Path) -> String {
    let path = app_path.to_string_lossy();
    if let Some(value) = output_of(
        "mdls",
        &["-raw", "-name", "kMDItemCFBundleIdentifier", &path],
    ) {
        if !value.is_empty() && value != "(null)" {
            return value;
        }
    }
    // An unindexed temporary QA bundle needs a direct plist read.
    let plist = app_path.join("Contents").join("Info.plist");
    output_of(
        "defaults",
        &["read", &plist.to_string_lossy(), "CFBundleIdentifier"],
    )
    .unwrap_or_default()
}

/// Whether a process whose full command line matches `pattern` is running (`pgrep -f`). A second
/// instance with the same bundle id cannot start, so a journey checks the installed production app's
/// executable path before launching its own copy.
pub fn is_process_running(pattern: &str) -> bool {
    run("pgrep", &["-f", pattern]).is_ok()
}
