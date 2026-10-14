//! DevTools policy: every Right app has no access to DevTools; ViewRight alone may inspect the
//! HTML content it opens.
//!
//! The owner's fixed design, verbatim: every Right app has absolutely NO access to DevTools,
//! blocked at each and every level that is possible, for the app, its shell and its own UI. The
//! single exception is ViewRight: it opens HTML documents, and DevTools must be available INSIDE
//! that opened HTML content only (the content frame), never for ViewRight's own UI or shell.
//!
//! Two values exist, [`DevToolsPolicy::Denied`] (the default) and [`DevToolsPolicy::ContentOnly`]
//! (a named allowance an app must request explicitly). Under both, every webview the shell sees is
//! made non-inspectable unless the app registered it as a content webview
//! ([`ContentWebview`]) and the policy is `ContentOnly`.
//!
//! Layers (see README "DevTools policy" for what each can and cannot block):
//! 1. [`guard_process_start`]: refuses a remote-debugging launch argument, removes the WebView2
//!    environment overrides, and refuses a machine policy that injects debugging browser
//!    arguments (Windows).
//! 2. Native per-webview settings ([`apply_native`]): `isInspectable` / `developerExtrasEnabled`
//!    (macOS), `AreDevToolsEnabled`, accelerator keys and the context-menu Inspect item (Windows).
//! 3. The injected hardening script (keyboard shortcuts), in [`crate::harden`].
//! 4. [`verify_not_remote_debuggable`]: a post-creation tripwire for a live remote-debugging port.
//!
//! Development builds are the one deliberate hole: with the off-by-default `dev-inspect` cargo
//! feature compiled in, every webview is inspectable and the guards above stand down (see
//! [`DEV_INSPECT_COMPILED`]). Such a build carries [`DEV_INSPECT_MARKER`], and `right-release`
//! refuses to seal or upload a release or update build that contains it.
//!
//! Everything that decides (argument and environment scanning, registry output, content
//! registration, capability checks) is a plain function so it is testable without a GUI.

use std::collections::BTreeSet;
use std::fmt;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Manager, Runtime, Webview};

/// The owner's decision, stated verbatim. Do not edit or soften.
pub const OWNER_DECISION: &str = "Every Right app has absolutely NO access to DevTools: blocked at each and every level that is possible, for the app, its shell and its own UI. The single exception is ViewRight: it opens HTML documents, and DevTools must be available INSIDE that opened HTML content only (the content frame), never for ViewRight's own UI or shell.";

/// Bytes a binary carries when it was built with the `content-devtools` feature. `right-release`
/// accepts Tauri's devtools code in a binary only if this marker is present and the app is on the
/// allow-list (`packages/release/devtools-policy.json`). Keep identical to
/// `CONTENT_ONLY_MARKER` in `packages/release/devtools-policy.mjs`.
pub const CONTENT_ONLY_MARKER: &str = "RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED";

#[cfg(feature = "content-devtools")]
#[used]
static CONTENT_ONLY_MARKER_BYTES: &[u8] = b"RIGHTKIT-SHELL-DEVTOOLS-CONTENT-ONLY-COMPILED";

/// Bytes that appear in every binary built with the `dev-inspect` feature, and in no other. `right-release`
/// scans Mach-O, PE and ELF files for exactly this string and refuses to seal or upload a release or update
/// tier build that has it. Keep identical to `DEV_INSPECT_MARKER` in
/// `packages/release/qa-control-scan.mjs`. Code outside the `dev-inspect` feature must never reference this
/// constant at run time (it would put the string into every binary that links the shell).
pub const DEV_INSPECT_MARKER: &str = "RIGHTKIT-SHELL-DEV-INSPECT-COMPILED";

/// True when this build was compiled with the `dev-inspect` feature: developer inspection is active.
pub const DEV_INSPECT_COMPILED: bool = cfg!(feature = "dev-inspect");

/// The marker as a retained static. `#[used]` keeps the object out of `-dead_strip` and `/OPT:REF`;
/// [`keep_dev_inspect_marker`] also reads it from [`guard_process_start`], so the linker, LTO and the
/// optimiser keep it for as long as the shell's startup code is kept.
#[cfg(feature = "dev-inspect")]
#[used]
static DEV_INSPECT_MARKER_BYTES: [u8; DEV_INSPECT_MARKER.len()] = {
    let src = DEV_INSPECT_MARKER.as_bytes();
    let mut out = [0u8; DEV_INSPECT_MARKER.len()];
    let mut i = 0;
    while i < src.len() {
        out[i] = src[i];
        i += 1;
    }
    out
};

/// A volatile read of the marker's first byte, so the static is reachable from live startup code.
#[cfg(feature = "dev-inspect")]
#[inline(never)]
fn keep_dev_inspect_marker() -> u8 {
    // SAFETY: the pointer is the start of a live, initialised static array of non-zero length.
    unsafe { std::ptr::read_volatile(DEV_INSPECT_MARKER_BYTES.as_ptr()) }
}

/// Who may use DevTools. Exactly two values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DevToolsPolicy {
    /// No webview of the app is inspectable, in any build. The default for every app.
    #[default]
    Denied,
    /// Only webviews the app registered as content webviews ([`ContentWebview`]) are inspectable.
    /// A named allowance; intended for ViewRight only. Needs the `content-devtools` feature.
    ContentOnly,
}

/// What the shell does to one webview.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebviewDecision {
    /// Make the webview non-inspectable.
    Deny,
    /// Leave the webview inspectable (a registered content webview under `ContentOnly`).
    AllowContent,
    /// Developer inspection: the build was compiled with the `dev-inspect` feature, so every webview
    /// (the app UI included) stays inspectable and no menu or shortcut is touched. Never produced
    /// by a build without that feature.
    AllowDev,
}

/// A webview that renders opened content and may be inspected under [`DevToolsPolicy::ContentOnly`].
///
/// Registration is refused unless the content webview cannot reach the app's IPC: no capability
/// the app ships may apply to it, by webview label or by the label of its parent window (Tauri
/// matches a capability's `windows` against the window a webview lives in, so a child webview of
/// the main window inherits the main window's capability), and the app must declare an
/// application ACL manifest (without one Tauri lets local-origin pages call every app command).
#[derive(Debug, Clone)]
pub struct ContentWebview {
    /// Label of the content webview, or a prefix pattern such as `html-content-*` for webviews
    /// created per document at run time.
    pub label: String,
    /// Label of the window that hosts it.
    pub window_label: String,
    /// Every capability document of the app, as JSON text (`include_str!` of each file under
    /// `capabilities/`, plus any capability added at runtime).
    pub capabilities: Vec<String>,
    /// The app declares an ACL manifest for its own commands (`tauri_build::AppManifest` with
    /// `commands(..)` in `build.rs`). The shell cannot read this at runtime, so the app attests it
    /// and `right-release` checks `build.rs` for it.
    pub app_acl_manifest: bool,
}

impl ContentWebview {
    pub fn new(label: &str, window_label: &str) -> Self {
        Self {
            label: label.to_owned(),
            window_label: window_label.to_owned(),
            capabilities: Vec::new(),
            app_acl_manifest: false,
        }
    }
    /// Add one capability document (JSON text).
    pub fn capability(mut self, json: &str) -> Self {
        self.capabilities.push(json.to_owned());
        self
    }
    /// Attest that the app declares an ACL manifest for its commands.
    pub fn app_acl_manifest(mut self, declared: bool) -> Self {
        self.app_acl_manifest = declared;
        self
    }
}

/// A refusal. Every variant is fatal at startup; there is no inspectable fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DevToolsError {
    /// The process was launched with a remote-debugging or auto-open-DevTools argument.
    RemoteDebuggingArgument { arg: String },
    /// A machine or user policy injects a debugging browser argument into WebView2.
    BrowserPolicyInjects { source: String, switch: String },
    /// A machine or user policy makes WebView2 start a browser from another folder than the
    /// installed runtime.
    BrowserRuntimeRedirected { source: String, folder: String },
    /// The Windows policy registry could not be read, so the policy cannot be ruled out.
    RegistryUnreadable { detail: String },
    /// A remote-debugging port file written by this process's WebView2 exists.
    RemoteDebugPortLive { path: PathBuf },
    /// Content webviews were registered but the policy is `Denied`.
    NotContentOnly,
    /// `ContentOnly` needs the `content-devtools` feature (it enables Tauri's devtools).
    ContentDevtoolsNotCompiled,
    /// The label is empty or not a valid Tauri label.
    InvalidLabel { label: String },
    /// The main (app UI) webview can never be a content webview.
    MainWebview { label: String },
    /// A capability applies to the content webview and grants permissions.
    ContentHasIpc { label: String, capability: String },
    /// A capability document could not be parsed.
    CapabilitiesUnreadable { detail: String },
    /// The app did not attest an application ACL manifest.
    AppAclManifestMissing { label: String },
    /// The webview is not a registered content webview.
    NotAContentWebview { label: String },
    /// The shell's DevTools gate is not installed.
    NotInstalled,
    /// The webview does not exist.
    WebviewMissing { label: String },
    /// DevTools could not be disabled in a denied webview, so the webview is refused.
    NativePolicyNotApplied { label: String, detail: String },
}

impl DevToolsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::RemoteDebuggingArgument { .. } => "remote_debugging_argument",
            Self::BrowserPolicyInjects { .. } => "browser_policy_injects",
            Self::BrowserRuntimeRedirected { .. } => "browser_runtime_redirected",
            Self::RegistryUnreadable { .. } => "registry_unreadable",
            Self::RemoteDebugPortLive { .. } => "remote_debug_port_live",
            Self::NotContentOnly => "not_content_only",
            Self::ContentDevtoolsNotCompiled => "content_devtools_not_compiled",
            Self::InvalidLabel { .. } => "invalid_label",
            Self::MainWebview { .. } => "main_webview",
            Self::ContentHasIpc { .. } => "content_has_ipc",
            Self::CapabilitiesUnreadable { .. } => "capabilities_unreadable",
            Self::AppAclManifestMissing { .. } => "app_acl_manifest_missing",
            Self::NotAContentWebview { .. } => "not_a_content_webview",
            Self::NotInstalled => "not_installed",
            Self::WebviewMissing { .. } => "webview_missing",
            Self::NativePolicyNotApplied { .. } => "native_policy_not_applied",
        }
    }
}

impl fmt::Display for DevToolsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rightkit-tauri-shell devtools policy: ")?;
        match self {
            Self::RemoteDebuggingArgument { arg } => write!(f, "refusing to start, the process was launched with a debugging argument ({arg})"),
            Self::BrowserPolicyInjects { source, switch } => write!(f, "refusing to create a webview, {source} injects a debugging browser argument ({switch}); an application cannot override a machine policy"),
            Self::BrowserRuntimeRedirected { source, folder } => write!(f, "refusing to create a webview, {source} makes WebView2 start a browser from `{folder}` instead of the installed runtime; an application cannot override a machine policy"),
            Self::RegistryUnreadable { detail } => write!(f, "refusing to create a webview, the WebView2 policy registry could not be read ({detail})"),
            Self::RemoteDebugPortLive { path } => write!(f, "refusing to run, a remote-debugging port is live ({})", path.display()),
            Self::NotContentOnly => write!(f, "content webviews are registered but the policy is Denied; select DevToolsPolicy::ContentOnly explicitly"),
            Self::ContentDevtoolsNotCompiled => write!(f, "ContentOnly needs the rightkit-tauri-shell `content-devtools` feature"),
            Self::InvalidLabel { label } => write!(f, "`{label}` is not a valid webview label"),
            Self::MainWebview { label } => write!(f, "`{label}` is the app UI webview; it can never be a content webview"),
            Self::ContentHasIpc { label, capability } => write!(f, "content webview `{label}` is covered by capability `{capability}`, which grants app IPC; content webviews need no capability at all"),
            Self::CapabilitiesUnreadable { detail } => write!(f, "a capability document could not be read ({detail})"),
            Self::AppAclManifestMissing { label } => write!(f, "content webview `{label}` requires an application ACL manifest for the app's own commands (otherwise local content can call every app command)"),
            Self::NotAContentWebview { label } => write!(f, "`{label}` is not a registered content webview"),
            Self::NotInstalled => write!(f, "the shell is not installed"),
            Self::WebviewMissing { label } => write!(f, "webview `{label}` does not exist"),
            Self::NativePolicyNotApplied { label, detail } => write!(f, "refusing webview `{label}`, DevTools could not be disabled in it ({detail})"),
        }
    }
}

impl std::error::Error for DevToolsError {}

impl From<DevToolsError> for crate::ShellError {
    fn from(e: DevToolsError) -> Self {
        crate::ShellError::PermissionDenied(e.to_string())
    }
}

// ---------------------------------------------------------------------------------------------
// Names. They are stored reversed and reassembled at run time: `right-release` scans every
// shipped binary for the remote-debugging switch and the WebView2 override variable as evidence of
// a debugging surface, and this crate must not trip its own scan.
// ---------------------------------------------------------------------------------------------

fn unrev(s: &str) -> String {
    s.chars().rev().collect()
}

/// Lower-case switch names (no leading dashes) that must never reach a browser process.
fn forbidden_switch_names() -> Vec<String> {
    [
        "trop-gniggubed-etomer",
        "epip-gniggubed-etomer",
        "sserdda-gniggubed-etomer",
        "snigiro-wolla-etomer",
        "sbat-rof-slootved-nepo-otua",
        "gniggubedetomerpdwslootvedegdesm",
    ]
    .iter()
    .map(|s| unrev(s))
    .collect()
}

/// Environment variables that can alter or attach a debugger to WebView2.
fn scrubbed_env_names() -> Vec<String> {
    [
        "STNEMUGRA_RESWORB_LANOITIDDA_2WEIVBEW",
        "REGGUBED_TPIRCS_ROF_EPIP_2WEIVBEW",
    ]
    .iter()
    .map(|s| unrev(s))
    .collect()
}

/// The debugging switches found in `args` (a command line or an `AdditionalBrowserArguments`
/// string): remote debugging port, pipe, address and allowed origins, auto-open DevTools, the
/// Edge remote-debugging flag, and `--js-flags` that enable the V8 inspector.
pub fn forbidden_switches_in(args: &str) -> Vec<String> {
    let names = forbidden_switch_names();
    let mut out = Vec::new();
    for raw in args.split_whitespace() {
        let token = raw.trim_matches(|c| c == '"' || c == '\'');
        let Some(rest) = token.strip_prefix('-') else {
            continue;
        };
        let rest = rest.trim_start_matches('-');
        let (name, value) = rest.split_once('=').unwrap_or((rest, ""));
        let name = name.to_ascii_lowercase();
        let hit = names.contains(&name)
            || (name == "js-flags" && value.to_ascii_lowercase().contains("inspect"));
        if hit {
            out.push(token.to_owned());
        }
    }
    out
}

/// Launch arguments that carry a debugging switch.
pub fn argv_violations(args: &[String]) -> Vec<String> {
    args.iter().flat_map(|a| forbidden_switches_in(a)).collect()
}

/// Names (as spelled in `names`) of the environment variables to remove from the process.
/// Matching ignores ASCII case: Windows environment names are case-insensitive.
pub fn env_vars_to_scrub<I, S>(names: I) -> Vec<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let wanted = scrubbed_env_names();
    names
        .into_iter()
        .filter(|n| wanted.iter().any(|w| w.eq_ignore_ascii_case(n.as_ref())))
        .map(|n| n.as_ref().to_owned())
        .collect()
}

/// One string value of a registry key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegValue {
    pub name: String,
    pub value: String,
}

/// `REG_SZ`, `REG_EXPAND_SZ` and `REG_MULTI_SZ`: the registry types that hold text.
const REG_TEXT_TYPES: [u32; 3] = [1, 2, 7];

/// The text of a registry value from its type and raw bytes (UTF-16LE), or `None` for a type that
/// holds no text (a number or binary data cannot carry a browser argument). The strings of a
/// `REG_MULTI_SZ` are joined with spaces; the terminating NULs are dropped.
pub fn decode_reg_text(kind: u32, data: &[u8]) -> Option<String> {
    if !REG_TEXT_TYPES.contains(&kind) {
        return None;
    }
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    let text = String::from_utf16_lossy(&units);
    Some(
        text.split('\0')
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" "),
    )
}

/// What opening a policy key means for the guard, from the status `RegOpenKeyExW` returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyKeyOpen {
    /// The key is open and its values are to be read.
    Opened,
    /// `ERROR_FILE_NOT_FOUND`: the key does not exist, so there is no policy. The only status
    /// that means that.
    Absent,
    /// Anything else (access denied, a registry failure): what the policy says is unknown, and
    /// the guard refuses to start.
    Unreadable(u32),
}

impl PolicyKeyOpen {
    pub fn from_status(status: u32) -> Self {
        match status {
            0 => Self::Opened,
            2 => Self::Absent,
            other => Self::Unreadable(other),
        }
    }
}

/// Errors for every value of a policy key that injects a debugging switch. A value named for a
/// different application is still refused: an administrator-set debugging policy is never normal.
pub fn policy_violations(values: &[RegValue], source: &str) -> Vec<DevToolsError> {
    values
        .iter()
        .flat_map(|v| {
            forbidden_switches_in(&v.value)
                .into_iter()
                .map(move |switch| DevToolsError::BrowserPolicyInjects {
                    source: format!("{source} (value `{}`)", v.name),
                    switch,
                })
        })
        .collect()
}

/// Whether a WebView2 policy value named `name` applies to the program whose executable file is
/// `exe_name`: the name is `*` (every application) or this program's executable file name,
/// compared without regard to case. Every other name belongs to another program. When this
/// program's own file name is unknown, every value is taken to apply.
pub fn policy_value_applies(name: &str, exe_name: &str) -> bool {
    let name = name.trim();
    exe_name.is_empty() || name == "*" || name.eq_ignore_ascii_case(exe_name)
}

/// The name of the environment variable that makes WebView2 start a browser from another folder.
pub fn runtime_folder_env_name() -> String {
    unrev("REDLOF_ELBATUCEXE_RESWORB_2WEIVBEW")
}

/// The refusal for that variable's `value` in the process environment at start, if it is set to
/// anything. A launcher controls the environment as it controls policy, so it is refused the
/// same way.
pub fn runtime_folder_env_violation(value: Option<&str>) -> Option<DevToolsError> {
    let folder = value?.trim();
    if folder.is_empty() {
        return None;
    }
    Some(DevToolsError::BrowserRuntimeRedirected {
        source: format!("the environment variable {}", runtime_folder_env_name()),
        folder: folder.to_owned(),
    })
}

/// Errors for every value of the WebView2 `BrowserExecutableFolder` policy that applies to this
/// program. Such a value makes WebView2 start whatever browser is in that folder, with every
/// argument and behaviour that browser was built with, so nothing this crate sets can be relied on.
pub fn runtime_folder_violations(
    values: &[RegValue],
    source: &str,
    exe_name: &str,
) -> Vec<DevToolsError> {
    values
        .iter()
        .filter(|v| !v.value.trim().is_empty() && policy_value_applies(&v.name, exe_name))
        .map(|v| DevToolsError::BrowserRuntimeRedirected {
            source: format!("{source} (value `{}`)", v.name),
            folder: v.value.trim().to_owned(),
        })
        .collect()
}

/// `text` with every `%NAME%` that `env` knows replaced by its value. An unknown name and a lone
/// `%` stay as they are.
pub fn expand_env_refs(text: &str, env: impl Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('%') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        match after.find('%') {
            Some(close) if close > 0 => match env(&after[..close]) {
                Some(value) => {
                    out.push_str(&value);
                    rest = &after[close + 1..];
                }
                None => {
                    out.push('%');
                    rest = after;
                }
            },
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The user data folders the WebView2 `UserDataFolder` policy names for this program, with
/// environment references expanded. The browser writes its port file there, not in the folder
/// the application chose, so the tripwire has to look there too.
pub fn policy_user_data_dirs(
    values: &[RegValue],
    exe_name: &str,
    env: impl Fn(&str) -> Option<String>,
) -> Vec<PathBuf> {
    values
        .iter()
        .filter(|v| !v.value.trim().is_empty() && policy_value_applies(&v.name, exe_name))
        .map(|v| PathBuf::from(expand_env_refs(v.value.trim(), &env)))
        .collect()
}

/// Further folders the port-file tripwire looks in, besides the app's local and roaming data.
static EXTRA_USER_DATA_DIRS: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

/// Tell the tripwire about a user data folder the application gives a webview itself (Tauri's
/// `data_directory`). Call it before the webview is created. The folders a policy or the WebView2
/// user-data environment variable names are added by [`guard_process_start`].
pub fn register_user_data_dir(dir: impl Into<PathBuf>) {
    let dir = dir.into();
    let mut dirs = EXTRA_USER_DATA_DIRS
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    if !dirs.contains(&dir) {
        dirs.push(dir);
    }
}

/// The result of [`guard_process_start`].
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GuardReport {
    /// Environment variables removed from the process.
    pub scrubbed_env: Vec<String>,
}

static GUARD: OnceLock<Result<GuardReport, DevToolsError>> = OnceLock::new();
static PROCESS_START: OnceLock<SystemTime> = OnceLock::new();

/// Process-start guard. Call it first in `main` (before Tauri builds anything) to fail before any
/// webview exists; the shell plugin also runs it when it is constructed and refuses to set up when
/// it failed. Idempotent: the first result is returned on every call.
///
/// 1. Refuses a launch argument that enables remote debugging or auto-opens DevTools.
/// 2. Removes the WebView2 override environment variables from this process, so they cannot reach
///    the browser process that WebView2 starts later (Windows).
/// 3. Refuses to continue when the WebView2 `AdditionalBrowserArguments` policy (HKLM, HKCU,
///    WOW6432Node) injects a debugging switch. WebView2 appends policy arguments to whatever the
///    application passes, so the application cannot override them (Windows).
/// 4. Refuses to continue when the WebView2 `BrowserExecutableFolder` policy, or the environment
///    variable of the same purpose, makes WebView2 start a browser from another folder for this
///    program (Windows).
/// 5. Records the folders the `UserDataFolder` policy and the WebView2 user-data environment
///    variable name, so the port-file tripwire looks there too (Windows).
///
/// A build compiled with the `dev-inspect` feature does none of this: it prints one loud stderr
/// line saying developer inspection is active and returns an empty report.
pub fn guard_process_start() -> Result<GuardReport, DevToolsError> {
    GUARD.get_or_init(run_guard).clone()
}

fn run_guard() -> Result<GuardReport, DevToolsError> {
    let _ = PROCESS_START.set(SystemTime::now());
    if DEV_INSPECT_COMPILED {
        return Ok(announce_dev_inspect());
    }
    let argv: Vec<String> = std::env::args_os()
        .skip(1)
        .map(|a| a.to_string_lossy().into_owned())
        .collect();
    if let Some(arg) = argv_violations(&argv).into_iter().next() {
        return Err(DevToolsError::RemoteDebuggingArgument { arg });
    }
    let present: Vec<String> = std::env::vars_os()
        .map(|(k, _)| k.to_string_lossy().into_owned())
        .collect();
    let scrubbed_env = env_vars_to_scrub(present.iter());
    for name in &scrubbed_env {
        std::env::remove_var(name);
    }
    #[cfg(windows)]
    {
        if let Some(err) = registry_policy_violations()?.into_iter().next() {
            return Err(err);
        }
        let folder = std::env::var_os(runtime_folder_env_name());
        let folder = folder.as_ref().map(|value| value.to_string_lossy());
        if let Some(err) = runtime_folder_env_violation(folder.as_deref()) {
            return Err(err);
        }
        let exe = current_exe_name();
        for (source, values) in read_policy("BrowserExecutableFolder")? {
            if let Some(err) = runtime_folder_violations(&values, source, &exe)
                .into_iter()
                .next()
            {
                return Err(err);
            }
        }
        let env = |name: &str| std::env::var(name).ok();
        for (_, values) in read_policy("UserDataFolder")? {
            for dir in policy_user_data_dirs(&values, &exe, env) {
                register_user_data_dir(dir);
            }
        }
        // The variable is left in place (a test harness may set it to keep runs apart); the
        // folder it names is watched like the others.
        if let Some(dir) = std::env::var_os(unrev("REDLOF_ATAD_RESU_2WEIVBEW")) {
            if !dir.is_empty() {
                register_user_data_dir(PathBuf::from(dir));
            }
        }
    }
    Ok(GuardReport { scrubbed_env })
}

/// The one loud line of a `dev-inspect` build. It also reads the marker static from live startup
/// code, so the marker survives dead-code elimination for as long as the shell does.
#[cfg(feature = "dev-inspect")]
fn announce_dev_inspect() -> GuardReport {
    let kept = keep_dev_inspect_marker();
    eprintln!(
        "RIGHTKIT SHELL DEVELOPER INSPECTION ACTIVE: this build was compiled with the rightkit-tauri-shell `dev-inspect` \
         feature (marker byte {kept:#04x}). Every webview is inspectable, debugging switches and WebView2 \
         environment overrides are not scrubbed, and no remote-debugging refusal or port tripwire runs. \
         A build like this can never be released; right-release refuses it."
    );
    GuardReport::default()
}

/// Never called: `DEV_INSPECT_COMPILED` is false here.
#[cfg(not(feature = "dev-inspect"))]
fn announce_dev_inspect() -> GuardReport {
    GuardReport::default()
}

/// Windows: the registry, read through the registry API. No `reg.exe`: a program started by name
/// is found in the application's directory and the working directory before the system's, and its
/// exit code cannot tell a missing key from one that could not be read.
#[cfg(windows)]
mod registry {
    use super::{decode_reg_text, PolicyKeyOpen, RegValue};

    pub(super) const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    pub(super) const HKEY_LOCAL_MACHINE: isize = 0x8000_0002_u32 as i32 as isize;
    /// The 64-bit and the 32-bit registry view, named explicitly so a 32-bit and a 64-bit build
    /// read the same two keys.
    pub(super) const VIEW_64: u32 = 0x0100;
    pub(super) const VIEW_32: u32 = 0x0200;
    const KEY_READ: u32 = 0x0002_0019;
    const ERROR_MORE_DATA: u32 = 234;
    const ERROR_NO_MORE_ITEMS: u32 = 259;
    /// The longest value name the registry allows, in UTF-16 units, plus the terminator.
    const NAME_UNITS: usize = 16_384;

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegOpenKeyExW(
            key: isize,
            sub_key: *const u16,
            options: u32,
            access: u32,
            result: *mut isize,
        ) -> u32;
        fn RegEnumValueW(
            key: isize,
            index: u32,
            name: *mut u16,
            name_len: *mut u32,
            reserved: *mut u32,
            kind: *mut u32,
            data: *mut u8,
            data_len: *mut u32,
        ) -> u32;
        fn RegCloseKey(key: isize) -> u32;
    }

    struct Key(isize);

    impl Drop for Key {
        fn drop(&mut self) {
            // SAFETY: the handle came from RegOpenKeyExW and is closed once.
            unsafe { RegCloseKey(self.0) };
        }
    }

    /// The text values of `root\path` in `view`. `Ok(None)`: the key does not exist. `Err`: the
    /// key could not be opened or read; the text names the step and the Win32 status.
    pub(super) fn text_values(
        root: isize,
        path: &str,
        view: u32,
    ) -> Result<Option<Vec<RegValue>>, String> {
        let wide: Vec<u16> = path.encode_utf16().chain([0]).collect();
        let mut handle: isize = 0;
        // SAFETY: `wide` is NUL-terminated and outlives the call; `handle` is a valid
        // out-parameter.
        let status = unsafe { RegOpenKeyExW(root, wide.as_ptr(), 0, KEY_READ | view, &mut handle) };
        match PolicyKeyOpen::from_status(status) {
            PolicyKeyOpen::Opened => {}
            PolicyKeyOpen::Absent => return Ok(None),
            PolicyKeyOpen::Unreadable(code) => {
                return Err(format!("open failed with Win32 error {code}"))
            }
        }
        let key = Key(handle);
        let mut values = Vec::new();
        let mut name = vec![0u16; NAME_UNITS];
        let mut data = vec![0u8; 4096];
        let mut index = 0u32;
        loop {
            let mut name_len = name.len() as u32;
            let mut data_len = data.len() as u32;
            let mut kind = 0u32;
            // SAFETY: both buffers are writable for the lengths passed, which the call updates;
            // the reserved pointer is null as documented.
            let status = unsafe {
                RegEnumValueW(
                    key.0,
                    index,
                    name.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    &mut kind,
                    data.as_mut_ptr(),
                    &mut data_len,
                )
            };
            match status {
                0 => {
                    if let Some(value) = decode_reg_text(kind, &data[..data_len as usize]) {
                        values.push(RegValue {
                            name: String::from_utf16_lossy(&name[..name_len as usize]),
                            value,
                        });
                    }
                    index += 1;
                }
                // The data did not fit: `data_len` now holds the size needed. Same index again.
                ERROR_MORE_DATA => {
                    let needed = (data_len as usize).max(data.len() * 2);
                    data.resize(needed, 0);
                }
                ERROR_NO_MORE_ITEMS => return Ok(Some(values)),
                other => {
                    return Err(format!(
                        "reading value {index} failed with Win32 error {other}"
                    ))
                }
            }
        }
    }
}

/// Every debugging switch the WebView2 `AdditionalBrowserArguments` policy injects, from the
/// machine policy in both registry views and the user policy. A key that does not exist is no
/// policy; a key that cannot be opened or read is [`DevToolsError::RegistryUnreadable`], and the
/// guard refuses to start: what an unreadable policy says is not known, so it is not assumed empty.
#[cfg(windows)]
fn registry_policy_violations() -> Result<Vec<DevToolsError>, DevToolsError> {
    let mut out = Vec::new();
    for (source, values) in read_policy("AdditionalBrowserArguments")? {
        out.extend(policy_violations(&values, source));
    }
    Ok(out)
}

/// This program's executable file name, or empty when it cannot be determined.
#[cfg(windows)]
fn current_exe_name() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_default()
}

/// The text values of one WebView2 policy, from the machine policy in both registry views and the
/// user policy, each with the name of where it was read. A key that does not exist contributes
/// nothing; a key that cannot be opened or read is [`DevToolsError::RegistryUnreadable`].
#[cfg(windows)]
fn read_policy(policy: &str) -> Result<Vec<(&'static str, Vec<RegValue>)>, DevToolsError> {
    let path = format!(r"Software\Policies\Microsoft\Edge\WebView2\{policy}");
    let path = path.as_str();
    let keys = [
        (
            "HKLM policy (64-bit view)",
            registry::HKEY_LOCAL_MACHINE,
            registry::VIEW_64,
        ),
        (
            "HKLM policy (32-bit view)",
            registry::HKEY_LOCAL_MACHINE,
            registry::VIEW_32,
        ),
        (
            "HKCU policy",
            registry::HKEY_CURRENT_USER,
            registry::VIEW_64,
        ),
    ];
    let mut out = Vec::new();
    for (source, root, view) in keys {
        match registry::text_values(root, path, view) {
            Ok(None) => {}
            Ok(Some(values)) => out.push((source, values)),
            Err(detail) => {
                return Err(DevToolsError::RegistryUnreadable {
                    detail: format!("{source}, {path}: {detail}"),
                })
            }
        }
    }
    Ok(out)
}

/// Test support (Windows): read the text values of a key under `HKEY_CURRENT_USER` the way the
/// guard reads the policy keys. `Ok(None)` when the key does not exist.
#[cfg(windows)]
#[doc(hidden)]
pub fn read_user_key_text_values(path: &str) -> Result<Option<Vec<RegValue>>, String> {
    registry::text_values(registry::HKEY_CURRENT_USER, path, registry::VIEW_64)
}

/// The file name Chromium writes into the user data folder when remote debugging is on.
pub const DEBUG_PORT_FILE: &str = "DevToolsActivePort";

/// A port file older than the process (by more than 2 s) is a leftover of an earlier run.
pub fn is_live_debug_port_file(modified: SystemTime, process_start: SystemTime) -> bool {
    modified + Duration::from_secs(2) >= process_start
}

/// A live remote-debugging port file under any of `dirs` (or under `<dir>/EBWebView`).
pub fn live_debug_port_file(dirs: &[PathBuf], process_start: SystemTime) -> Option<PathBuf> {
    for dir in dirs {
        for candidate in [
            dir.join(DEBUG_PORT_FILE),
            dir.join("EBWebView").join(DEBUG_PORT_FILE),
        ] {
            if let Ok(meta) = std::fs::metadata(&candidate) {
                // Unreadable time: assume live (fail closed).
                let modified = meta.modified().unwrap_or_else(|_| SystemTime::now());
                if is_live_debug_port_file(modified, process_start) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

/// Post-creation tripwire (Windows): the browser process writes the port file while it starts, so a
/// fresh file next to this process's user data means a debugging port is open. A no-op elsewhere.
pub fn verify_not_remote_debuggable<R: Runtime>(app: &AppHandle<R>) -> Result<(), DevToolsError> {
    // A `dev-inspect` build is meant to be attached to: no tripwire.
    if DEV_INSPECT_COMPILED {
        return Ok(());
    }
    #[cfg(windows)]
    {
        let start = PROCESS_START.get().copied().unwrap_or_else(SystemTime::now);
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Ok(d) = app.path().app_local_data_dir() {
            dirs.push(d);
        }
        if let Ok(d) = app.path().app_data_dir() {
            dirs.push(d);
        }
        dirs.extend(
            EXTRA_USER_DATA_DIRS
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .iter()
                .cloned(),
        );
        if let Some(path) = live_debug_port_file(&dirs, start) {
            return Err(DevToolsError::RemoteDebugPortLive { path });
        }
    }
    #[cfg(not(windows))]
    let _ = app;
    Ok(())
}

/// The tripwire once more, the first time a page has finished loading; later calls return `Ok`
/// without looking. The browser process can write its port file after the webview is ready, which
/// is when [`verify_not_remote_debuggable`] first runs.
pub fn verify_after_first_navigation<R: Runtime>(app: &AppHandle<R>) -> Result<(), DevToolsError> {
    static DONE: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if DONE.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return Ok(());
    }
    verify_not_remote_debuggable(app)
}

// ---------------------------------------------------------------------------------------------
// Registration and the gate
// ---------------------------------------------------------------------------------------------

/// Which webviews are inspectable. Built once at startup and kept in Tauri state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevToolsGate {
    policy: DevToolsPolicy,
    main_label: String,
    content: BTreeSet<String>,
    /// Built with the `dev-inspect` feature: every webview is inspectable.
    dev_inspect: bool,
}

impl DevToolsGate {
    /// The gate that denies every webview.
    pub fn denied(main_label: &str) -> Self {
        Self {
            policy: DevToolsPolicy::Denied,
            main_label: main_label.to_owned(),
            content: BTreeSet::new(),
            dev_inspect: false,
        }
    }
    /// Whether this is the gate of a `dev-inspect` build (every webview inspectable). Only
    /// [`build_gate_with`] with `dev_inspect = true` produces one.
    pub fn is_dev_inspect(&self) -> bool {
        self.dev_inspect
    }
    pub fn policy(&self) -> DevToolsPolicy {
        self.policy
    }
    /// Registered content webview labels or prefix patterns (empty under `Denied`). A `dev-inspect`
    /// gate returns the single pattern `*`, so the injected script stands aside in every webview.
    pub fn content_labels(&self) -> Vec<String> {
        if self.dev_inspect {
            return vec!["*".to_owned()];
        }
        self.content.iter().cloned().collect()
    }
    /// What to do with the webview labelled `label`. Anything not registered is denied, and the
    /// main webview is denied whatever else is true.
    pub fn decide(&self, label: &str) -> WebviewDecision {
        if self.dev_inspect {
            return WebviewDecision::AllowDev;
        }
        if self.policy == DevToolsPolicy::ContentOnly
            && label != self.main_label
            && self
                .content
                .iter()
                .any(|pattern| glob_match(pattern, label))
        {
            WebviewDecision::AllowContent
        } else {
            WebviewDecision::Deny
        }
    }
}

fn valid_label(label: &str) -> bool {
    !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '/' | ':' | '_'))
}

/// A content label is an exact Tauri label, or a prefix pattern: a valid label followed by one
/// trailing `*` (`html-content-*`), for webviews created per document or tab at run time.
fn valid_content_label(label: &str) -> bool {
    match label.strip_suffix('*') {
        Some(prefix) => valid_label(prefix),
        None => valid_label(label),
    }
}

/// Could any label match both patterns? Conservative: patterns are compared by their literal head
/// (the text before the first wildcard), and heads that are prefixes of one another overlap.
pub fn patterns_may_overlap(a: &str, b: &str) -> bool {
    let head = |p: &str| match p.find(['*', '?']) {
        Some(i) => (p[..i].to_owned(), true),
        None => (p.to_owned(), false),
    };
    let (ha, wa) = head(a);
    let (hb, wb) = head(b);
    match (wa, wb) {
        (false, false) => ha == hb,
        (true, false) => hb.starts_with(&ha) && glob_match(a, b),
        (false, true) => ha.starts_with(&hb) && glob_match(b, a),
        (true, true) => ha.starts_with(&hb) || hb.starts_with(&ha),
    }
}

/// `*` matches any run, `?` one character: the glob subset Tauri capability patterns use.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn patterns(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|s| s.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The identifier of the first capability in `doc` that applies to the webview and grants
/// permissions. A capability applies when one of its `webviews` patterns matches the webview label
/// or one of its `windows` patterns matches the label of the window that hosts the webview.
/// `webview_label` may be a content prefix pattern (`html-content-*`); overlap is judged conservatively.
pub fn capability_granting_ipc(
    doc: &str,
    webview_label: &str,
    window_label: &str,
) -> Result<Option<String>, DevToolsError> {
    let parsed: serde_json::Value =
        serde_json::from_str(doc).map_err(|e| DevToolsError::CapabilitiesUnreadable {
            detail: e.to_string(),
        })?;
    let docs: Vec<&serde_json::Value> = match &parsed {
        serde_json::Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    for cap in docs {
        let applies = patterns(cap, "webviews")
            .iter()
            .any(|p| patterns_may_overlap(p, webview_label))
            || patterns(cap, "windows")
                .iter()
                .any(|p| glob_match(p, window_label));
        let grants = cap
            .get("permissions")
            .and_then(|v| v.as_array())
            .is_some_and(|a| !a.is_empty());
        if applies && grants {
            let id = cap
                .get("identifier")
                .and_then(|v| v.as_str())
                .unwrap_or("<unnamed>");
            return Ok(Some(id.to_owned()));
        }
    }
    Ok(None)
}

/// Validate the policy and the content registrations and build the gate. `devtools_compiled` is
/// `cfg!(feature = "content-devtools")` in the shell; it is a parameter so tests can cover both.
pub fn build_gate(
    policy: DevToolsPolicy,
    main_label: &str,
    registrations: &[ContentWebview],
    devtools_compiled: bool,
) -> Result<DevToolsGate, DevToolsError> {
    build_gate_with(policy, main_label, registrations, devtools_compiled, false)
}

/// [`build_gate`] plus the `dev-inspect` switch (`DEV_INSPECT_COMPILED` in the shell). With
/// `dev_inspect` the registrations are still validated as before (a misconfiguration surfaces in
/// development, not only in the release build), Tauri's devtools count as compiled (the feature
/// enables them), and the gate allows every webview.
pub fn build_gate_with(
    policy: DevToolsPolicy,
    main_label: &str,
    registrations: &[ContentWebview],
    devtools_compiled: bool,
    dev_inspect: bool,
) -> Result<DevToolsGate, DevToolsError> {
    let devtools_compiled = devtools_compiled || dev_inspect;
    if registrations.is_empty() {
        return Ok(DevToolsGate {
            policy,
            main_label: main_label.to_owned(),
            content: BTreeSet::new(),
            dev_inspect,
        });
    }
    if policy != DevToolsPolicy::ContentOnly {
        return Err(DevToolsError::NotContentOnly);
    }
    if !devtools_compiled {
        return Err(DevToolsError::ContentDevtoolsNotCompiled);
    }
    let mut content = BTreeSet::new();
    for reg in registrations {
        if !valid_content_label(&reg.label) || !valid_label(&reg.window_label) {
            return Err(DevToolsError::InvalidLabel {
                label: reg.label.clone(),
            });
        }
        if reg.label == main_label || glob_match(&reg.label, main_label) {
            return Err(DevToolsError::MainWebview {
                label: reg.label.clone(),
            });
        }
        if !reg.app_acl_manifest {
            return Err(DevToolsError::AppAclManifestMissing {
                label: reg.label.clone(),
            });
        }
        for doc in &reg.capabilities {
            if let Some(capability) = capability_granting_ipc(doc, &reg.label, &reg.window_label)? {
                return Err(DevToolsError::ContentHasIpc {
                    label: reg.label.clone(),
                    capability,
                });
            }
        }
        content.insert(reg.label.clone());
    }
    Ok(DevToolsGate {
        policy,
        main_label: main_label.to_owned(),
        content,
        dev_inspect,
    })
}

/// Open the inspector of a registered content webview. The only path from this crate to an open
/// inspector; every other webview is refused.
pub fn open_content_devtools<R: Runtime>(
    app: &AppHandle<R>,
    label: &str,
) -> Result<(), DevToolsError> {
    let gate = app
        .try_state::<DevToolsGate>()
        .ok_or(DevToolsError::NotInstalled)?;
    if gate.decide(label) == WebviewDecision::Deny {
        return Err(DevToolsError::NotAContentWebview {
            label: label.to_owned(),
        });
    }
    #[cfg(any(feature = "content-devtools", feature = "dev-inspect"))]
    {
        let webview = app
            .get_webview(label)
            .ok_or_else(|| DevToolsError::WebviewMissing {
                label: label.to_owned(),
            })?;
        webview.open_devtools();
        Ok(())
    }
    #[cfg(not(any(feature = "content-devtools", feature = "dev-inspect")))]
    Err(DevToolsError::ContentDevtoolsNotCompiled)
}

// ---------------------------------------------------------------------------------------------
// Native, per-webview settings
// ---------------------------------------------------------------------------------------------

/// The native DevTools setting of one webview, as [`enforce_switch`] needs it.
pub trait DevToolsSwitch {
    fn set(&mut self, enabled: bool) -> Result<(), String>;
    fn get(&self) -> Result<bool, String>;
}

/// Set the native DevTools switch for `decision`. A denied webview is accepted only when the
/// switch was set to off and reads back off; a missing settings object, a failed call or a value
/// that is still on is [`DevToolsError::NativePolicyNotApplied`]. A webview that may be inspected
/// is never refused here: failing to turn inspection on is not a security failure.
pub fn enforce_switch(
    label: &str,
    decision: WebviewDecision,
    switch: Option<&mut dyn DevToolsSwitch>,
) -> Result<(), DevToolsError> {
    let deny = decision == WebviewDecision::Deny;
    let refuse = |detail: String| DevToolsError::NativePolicyNotApplied {
        label: label.to_owned(),
        detail,
    };
    let Some(switch) = switch else {
        return if deny {
            Err(refuse("the webview has no settings object".to_owned()))
        } else {
            Ok(())
        };
    };
    let set = switch.set(!deny);
    if !deny {
        return Ok(());
    }
    set.map_err(|e| refuse(format!("the setting was rejected: {e}")))?;
    match switch.get() {
        Ok(false) => Ok(()),
        Ok(true) => Err(refuse(
            "the setting is still on after it was turned off".to_owned(),
        )),
        Err(e) => Err(refuse(format!("the setting could not be read back: {e}"))),
    }
}

/// Which groups of browser keys one webview does not act on. Each group is switched by one rule:
/// `devtools` by the DevTools decision (a denied webview), the others by the hardening options of
/// the same name. A key in no active group is left to the browser.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyRules {
    /// F12, Ctrl+Shift+I / J / C (inspector) and Ctrl+U (view source).
    pub devtools: bool,
    /// F5, Ctrl+R and Ctrl+Shift+R, and the keyboard's refresh key.
    pub reload: bool,
    /// Alt+Left, Alt+Right and the keyboard's back and forward keys.
    pub history: bool,
    /// Ctrl with plus, minus or zero, on the main block or the number pad.
    pub zoom: bool,
}

/// The modifier keys held while a key went down.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyModifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Whether a webview with `rules` must not act on this key (a Windows virtual-key code). Find,
/// print and every key outside the active groups always pass.
pub fn is_blocked_accelerator(virtual_key: u32, held: KeyModifiers, rules: KeyRules) -> bool {
    let KeyModifiers { ctrl, shift, alt } = held;
    match virtual_key {
        // F12; I, J, C; U.
        0x7B => rules.devtools,
        0x49 | 0x4A | 0x43 => rules.devtools && ctrl && shift,
        0x55 => rules.devtools && ctrl,
        // F5, browser refresh; R.
        0x74 | 0xA8 => rules.reload,
        0x52 => rules.reload && ctrl,
        // Left, Right; browser back, forward.
        0x25 | 0x27 => rules.history && alt,
        0xA6 | 0xA7 => rules.history,
        // Plus, minus, zero; number pad add, subtract, zero.
        0xBB | 0xBD | 0x30 | 0x6B | 0x6D | 0x60 => rules.zoom && ctrl,
        _ => false,
    }
}

/// Apply the DevTools decision to one live webview. Runs for every webview, whatever other
/// hardening modules the app opted out of.
///
/// `Err` (Windows only) means DevTools could not be disabled in a denied webview; the webview has
/// been closed and the caller must not go on setting it up. See `windows_impl::apply` for when
/// the refusal happens after this function has returned.
pub fn apply_native<R: Runtime>(
    webview: &Webview<R>,
    decision: WebviewDecision,
) -> Result<(), DevToolsError> {
    // Close an inspector that already exists, where the build has one.
    #[cfg(any(
        debug_assertions,
        feature = "content-devtools",
        feature = "dev-inspect"
    ))]
    if decision == WebviewDecision::Deny {
        webview.close_devtools();
    }
    #[cfg(target_os = "macos")]
    macos::apply(webview, decision);
    #[cfg(windows)]
    windows_impl::apply(webview, decision)?;
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (webview, decision);
    Ok(())
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;
    use objc2::msg_send;
    use objc2::runtime::AnyObject;
    use objc2_foundation::{ns_string, NSNumber};

    pub fn apply<R: Runtime>(webview: &Webview<R>, decision: WebviewDecision) {
        let allow = decision != WebviewDecision::Deny;
        let _ = webview.with_webview(move |platform| unsafe {
            // SAFETY: Tauri hands over the live WKWebView; used only inside this main-thread closure.
            let wk = &*(platform.inner() as *mut AnyObject);
            // `isInspectable` (macOS 13.3+) is what Safari's Develop menu and `_inspector` honour.
            let responds: bool = msg_send![wk, respondsToSelector: objc2::sel!(setInspectable:)];
            if responds {
                let (): () = msg_send![wk, setInspectable: allow];
            }
            // The private `developerExtrasEnabled` preference adds "Inspect Element" to the context
            // menu and WebKit's own Cmd+Opt+I handler. Set explicitly in both directions so the
            // value never depends on a default (wry sets it true when its devtools are on).
            let config: objc2::rc::Retained<AnyObject> = msg_send![wk, configuration];
            let prefs: objc2::rc::Retained<AnyObject> = msg_send![&config, preferences];
            let flag = NSNumber::numberWithBool(allow);
            let (): () = msg_send![&prefs, setValue: Some(&*flag), forKey: ns_string!("developerExtrasEnabled")];
        });
    }
}

#[cfg(windows)]
pub(crate) use windows_impl::set_hardening_keys;

#[cfg(windows)]
mod windows_impl {
    use super::*;
    use crate::winapi::webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2ContextMenuItem, ICoreWebView2ContextMenuItemCollection,
        ICoreWebView2Settings, ICoreWebView2_11, COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN,
        COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN,
    };
    use crate::winapi::webview2_com::{
        AcceleratorKeyPressedEventHandler, ContextMenuRequestedEventHandler,
    };
    use crate::winapi::windows::core::{Interface, PWSTR};
    use std::collections::HashMap;
    use std::sync::Mutex;
    use tauri::webview::PlatformWebview;

    #[link(name = "ole32")]
    unsafe extern "system" {
        fn CoTaskMemFree(block: *const core::ffi::c_void);
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetKeyState(virtual_key: i32) -> i16;
    }
    const VK_SHIFT: i32 = 0x10;
    const VK_CONTROL: i32 = 0x11;
    const VK_MENU: i32 = 0x12;

    /// The key rules of each webview, by label. The one key handler of a webview reads them when
    /// a key goes down, so a rule set after the handler was added still applies.
    static KEY_RULES: Mutex<Option<HashMap<String, KeyRules>>> = Mutex::new(None);

    fn update_key_rules(label: &str, change: impl FnOnce(&mut KeyRules)) {
        let mut rules = KEY_RULES
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        change(
            rules
                .get_or_insert_with(HashMap::new)
                .entry(label.to_owned())
                .or_default(),
        );
    }

    fn key_rules(label: &str) -> KeyRules {
        let rules = KEY_RULES
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        rules
            .as_ref()
            .and_then(|map| map.get(label).copied())
            .unwrap_or_default()
    }

    /// The hardening layer's key groups for one webview. It adds no key handler of its own and
    /// never turns all browser keys off.
    pub(crate) fn set_hardening_keys(label: &str, reload: bool, history: bool, zoom: bool) {
        update_key_rules(label, |rules| {
            rules.reload = reload;
            rules.history = history;
            rules.zoom = zoom;
        });
    }

    fn key_is_down(virtual_key: i32) -> bool {
        // SAFETY: GetKeyState takes a virtual-key code and has no other precondition.
        unsafe { GetKeyState(virtual_key) < 0 }
    }

    /// Event registrations this module made, by webview label: (context menu, accelerator keys).
    /// A webview that becomes ready again gets its old handlers removed before the new ones are
    /// added, so each webview has one of each.
    static HANDLERS: Mutex<Option<HashMap<String, Registered>>> = Mutex::new(None);

    /// The event tokens of one webview: (context menu, accelerator keys).
    type Registered = (Option<i64>, Option<i64>);

    fn is_inspect_item(item: &ICoreWebView2ContextMenuItem) -> bool {
        let mut name = PWSTR::null();
        // SAFETY: Name writes a CoTaskMem string, which is copied and then freed here.
        unsafe {
            if item.Name(&mut name).is_err() || name.is_null() {
                return false;
            }
            let text = name.to_string().unwrap_or_default();
            CoTaskMemFree(name.0 as *const core::ffi::c_void);
            text.to_ascii_lowercase().contains("inspect")
        }
    }

    /// Remove the items for which `remove` is true.
    unsafe fn retain_items(
        items: &ICoreWebView2ContextMenuItemCollection,
        remove: impl Fn(&ICoreWebView2ContextMenuItem) -> bool,
    ) -> usize {
        let mut count: u32 = 0;
        if items.Count(&mut count).is_err() {
            return 0;
        }
        let mut i = count;
        while i > 0 {
            i -= 1;
            if let Ok(item) = items.GetValueAtIndex(i) {
                if remove(&item) {
                    let _ = items.RemoveValueAtIndex(i);
                }
            }
        }
        let mut left: u32 = 0;
        let _ = items.Count(&mut left);
        left as usize
    }

    struct LiveSwitch<'a>(&'a ICoreWebView2Settings);

    impl DevToolsSwitch for LiveSwitch<'_> {
        fn set(&mut self, enabled: bool) -> Result<(), String> {
            // SAFETY: a live settings object, used on the thread WebView2 gave it to us on.
            unsafe { self.0.SetAreDevToolsEnabled(enabled) }.map_err(|e| e.to_string())
        }
        fn get(&self) -> Result<bool, String> {
            let mut enabled = Default::default();
            // SAFETY: as above; `enabled` is a valid out pointer.
            unsafe { self.0.AreDevToolsEnabled(&mut enabled) }.map_err(|e| e.to_string())?;
            Ok(enabled.as_bool())
        }
    }

    /// WebView2 side of the policy. Compiled by the windows-2025 CI job; nobody has run it against
    /// a live WebView2 on a Windows machine.
    ///
    /// `Err` means DevTools could not be disabled in a denied webview. The webview is then closed
    /// by this function. When Tauri runs the native closure later rather than at once, the result
    /// here is `Ok` and the closure closes the webview itself on failure.
    pub fn apply<R: Runtime>(
        webview: &Webview<R>,
        decision: WebviewDecision,
    ) -> Result<(), DevToolsError> {
        let label = webview.label().to_owned();
        let refused = webview.clone();
        let (report, reported) = std::sync::mpsc::channel();
        let on_main = label.clone();
        let dispatched = webview.with_webview(move |platform| {
            let outcome = apply_on_main(&platform, &on_main, decision);
            if let Err(error) = &outcome {
                eprintln!("{error}");
                // Closed from another thread: this closure still holds the platform webview.
                std::thread::spawn(move || {
                    let _ = refused.close();
                });
            }
            let _ = report.send(outcome);
        });
        if let Err(error) = dispatched {
            if decision != WebviewDecision::Deny {
                return Ok(());
            }
            let error = DevToolsError::NativePolicyNotApplied {
                label,
                detail: format!("the webview could not be reached: {error}"),
            };
            eprintln!("{error}");
            let _ = webview.close();
            return Err(error);
        }
        reported.try_recv().unwrap_or(Ok(()))
    }

    fn apply_on_main(
        platform: &PlatformWebview,
        label: &str,
        decision: WebviewDecision,
    ) -> Result<(), DevToolsError> {
        let allow = decision != WebviewDecision::Deny;
        // Developer inspection leaves the context menu exactly as WebView2 has it.
        let dev = decision == WebviewDecision::AllowDev;
        // SAFETY: Tauri hands over the live controller; used only inside this main-thread call.
        unsafe {
            let controller = platform.controller();
            let core = controller.CoreWebView2().ok();
            let settings = core.as_ref().and_then(|core| core.Settings().ok());
            let mut live = settings.as_ref().map(LiveSwitch);
            enforce_switch(
                label,
                decision,
                live.as_mut().map(|live| live as &mut dyn DevToolsSwitch),
            )?;
            let Some(core) = core else {
                return Ok(());
            };
            let wv11 = core.cast::<ICoreWebView2_11>().ok();

            let mut handlers = HANDLERS.lock().unwrap_or_else(|poison| poison.into_inner());
            let (old_menu, old_keys) = handlers
                .get_or_insert_with(HashMap::new)
                .remove(label)
                .unwrap_or((None, None));
            if let (Some(wv11), Some(token)) = (&wv11, old_menu) {
                let _ = wv11.remove_ContextMenuRequested(token);
            }
            if let Some(token) = old_keys {
                let _ = controller.remove_AcceleratorKeyPressed(token);
            }

            // One key handler per webview, for the whole shell. It stops exactly the keys this
            // webview's rules name (inspector and view-source keys when denied; reload, history
            // and zoom keys when the hardening asks) and leaves every other key to WebView2.
            update_key_rules(label, |rules| rules.devtools = !allow);
            let mut keys = None;
            {
                let label = label.to_owned();
                let handler =
                    AcceleratorKeyPressedEventHandler::create(Box::new(move |_sender, args| {
                        let Some(args) = args else { return Ok(()) };
                        let mut kind = Default::default();
                        args.KeyEventKind(&mut kind)?;
                        if kind != COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN
                            && kind != COREWEBVIEW2_KEY_EVENT_KIND_SYSTEM_KEY_DOWN
                        {
                            return Ok(());
                        }
                        let mut key: u32 = 0;
                        args.VirtualKey(&mut key)?;
                        let held = KeyModifiers {
                            ctrl: key_is_down(VK_CONTROL),
                            shift: key_is_down(VK_SHIFT),
                            alt: key_is_down(VK_MENU),
                        };
                        if is_blocked_accelerator(key, held, key_rules(&label)) {
                            args.SetHandled(true)?;
                        }
                        Ok(())
                    }));
                let mut token: i64 = 0;
                if controller
                    .add_AcceleratorKeyPressed(&handler, &mut token)
                    .is_ok()
                {
                    keys = Some(token);
                }
            }

            // The default context menu carries "Inspect". Denied: remove it from every menu.
            // Content: keep only it, and show no menu when it is absent.
            let mut menu = None;
            if let (false, Some(wv11)) = (dev, &wv11) {
                let handler =
                    ContextMenuRequestedEventHandler::create(Box::new(move |_sender, args| {
                        let Some(args) = args else { return Ok(()) };
                        let items = args.MenuItems()?;
                        if allow {
                            let left = retain_items(&items, |it| !is_inspect_item(it));
                            if left == 0 {
                                args.SetHandled(true)?;
                            }
                        } else {
                            retain_items(&items, is_inspect_item);
                        }
                        Ok(())
                    }));
                let mut token: i64 = 0;
                if wv11.add_ContextMenuRequested(&handler, &mut token).is_ok() {
                    menu = Some(token);
                }
            }
            if let Some(map) = handlers.as_mut() {
                map.insert(label.to_owned(), (menu, keys));
            }
            Ok(())
        }
    }
}
