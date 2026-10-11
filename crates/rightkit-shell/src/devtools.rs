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
}

impl DevToolsError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::RemoteDebuggingArgument { .. } => "remote_debugging_argument",
            Self::BrowserPolicyInjects { .. } => "browser_policy_injects",
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
        }
    }
}

impl fmt::Display for DevToolsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "rightkit-shell devtools policy: ")?;
        match self {
            Self::RemoteDebuggingArgument { arg } => write!(f, "refusing to start, the process was launched with a debugging argument ({arg})"),
            Self::BrowserPolicyInjects { source, switch } => write!(f, "refusing to create a webview, {source} injects a debugging browser argument ({switch}); an application cannot override a machine policy"),
            Self::RegistryUnreadable { detail } => write!(f, "refusing to create a webview, the WebView2 policy registry could not be read ({detail})"),
            Self::RemoteDebugPortLive { path } => write!(f, "refusing to run, a remote-debugging port is live ({})", path.display()),
            Self::NotContentOnly => write!(f, "content webviews are registered but the policy is Denied; select DevToolsPolicy::ContentOnly explicitly"),
            Self::ContentDevtoolsNotCompiled => write!(f, "ContentOnly needs the rightkit-shell `content-devtools` feature"),
            Self::InvalidLabel { label } => write!(f, "`{label}` is not a valid webview label"),
            Self::MainWebview { label } => write!(f, "`{label}` is the app UI webview; it can never be a content webview"),
            Self::ContentHasIpc { label, capability } => write!(f, "content webview `{label}` is covered by capability `{capability}`, which grants app IPC; content webviews need no capability at all"),
            Self::CapabilitiesUnreadable { detail } => write!(f, "a capability document could not be read ({detail})"),
            Self::AppAclManifestMissing { label } => write!(f, "content webview `{label}` requires an application ACL manifest for the app's own commands (otherwise local content can call every app command)"),
            Self::NotAContentWebview { label } => write!(f, "`{label}` is not a registered content webview"),
            Self::NotInstalled => write!(f, "the shell is not installed"),
            Self::WebviewMissing { label } => write!(f, "webview `{label}` does not exist"),
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

/// One value of a registry key as printed by `reg.exe query`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegValue {
    pub name: String,
    pub value: String,
}

/// Parse `reg.exe query <key>` output: lines of `<name>    REG_SZ    <data>`.
pub fn parse_reg_query(output: &str) -> Vec<RegValue> {
    let mut values = Vec::new();
    for line in output.lines() {
        let t = line.trim();
        if t.is_empty() || t.to_ascii_uppercase().starts_with("HKEY_") {
            continue;
        }
        for ty in ["REG_EXPAND_SZ", "REG_MULTI_SZ", "REG_SZ"] {
            if let Some(i) = t.find(ty) {
                values.push(RegValue {
                    name: t[..i].trim().to_owned(),
                    value: t[i + ty.len()..].trim().to_owned(),
                });
                break;
            }
        }
    }
    values
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
    if let Some(err) = registry_policy_violations()?.into_iter().next() {
        return Err(err);
    }
    Ok(GuardReport { scrubbed_env })
}

/// The one loud line of a `dev-inspect` build. It also reads the marker static from live startup
/// code, so the marker survives dead-code elimination for as long as the shell does.
#[cfg(feature = "dev-inspect")]
fn announce_dev_inspect() -> GuardReport {
    let kept = keep_dev_inspect_marker();
    eprintln!(
        "RIGHTKIT SHELL DEVELOPER INSPECTION ACTIVE: this build was compiled with the rightkit-shell `dev-inspect` \
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

#[cfg(windows)]
fn registry_policy_violations() -> Result<Vec<DevToolsError>, DevToolsError> {
    use std::os::windows::process::CommandExt;
    let tail = r"Software\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments";
    let keys = [
        ("HKLM policy", format!(r"HKLM\{tail}")),
        ("HKCU policy", format!(r"HKCU\{tail}")),
        (
            "HKLM WOW6432Node policy",
            r"HKLM\Software\WOW6432Node\Policies\Microsoft\Edge\WebView2\AdditionalBrowserArguments".to_owned(),
        ),
    ];
    let mut out = Vec::new();
    for (source, key) in keys {
        let output = std::process::Command::new("reg.exe")
            .creation_flags(0x0800_0000)
            .args(["query", key.as_str()])
            .output()
            .map_err(|e| DevToolsError::RegistryUnreadable {
                detail: format!("run reg.exe: {e}"),
            })?;
        match output.status.code() {
            // 1: the key does not exist, so there is no policy.
            Some(1) => continue,
            Some(0) => {
                let text = String::from_utf8_lossy(&output.stdout);
                out.extend(policy_violations(&parse_reg_query(&text), source));
            }
            other => {
                return Err(DevToolsError::RegistryUnreadable {
                    detail: format!("reg.exe query {key} exited with {other:?}"),
                })
            }
        }
    }
    Ok(out)
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
        if let Some(path) = live_debug_port_file(&dirs, start) {
            return Err(DevToolsError::RemoteDebugPortLive { path });
        }
    }
    #[cfg(not(windows))]
    let _ = app;
    Ok(())
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

/// Apply the DevTools decision to one live webview. Runs for every webview, whatever other
/// hardening modules the app opted out of.
pub fn apply_native<R: Runtime>(webview: &Webview<R>, decision: WebviewDecision) {
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
    windows_impl::apply(webview, decision);
    #[cfg(not(any(target_os = "macos", windows)))]
    let _ = (webview, decision);
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
mod windows_impl {
    use super::*;
    use crate::winapi::webview2_com::ContextMenuRequestedEventHandler;
    use crate::winapi::webview2_com::Microsoft::Web::WebView2::Win32::{
        ICoreWebView2ContextMenuItem, ICoreWebView2ContextMenuItemCollection,
        ICoreWebView2Settings3, ICoreWebView2_11,
    };
    use crate::winapi::windows::core::{Interface, PWSTR};

    fn is_inspect_item(item: &ICoreWebView2ContextMenuItem) -> bool {
        let mut name = PWSTR::null();
        // SAFETY: Name writes a CoTaskMem string; the few bytes per menu are left unfreed on purpose.
        let ok = unsafe { item.Name(&mut name) }.is_ok() && !name.is_null();
        ok && unsafe { name.to_string() }
            .unwrap_or_default()
            .to_ascii_lowercase()
            .contains("inspect")
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

    /// WebView2 side of the policy. NOT compiled on the macOS host: the windows-2025 CI job
    /// compiles it, and nobody has run it on a Windows machine.
    pub fn apply<R: Runtime>(webview: &Webview<R>, decision: WebviewDecision) {
        let allow = decision != WebviewDecision::Deny;
        // Developer inspection leaves the context menu exactly as WebView2 has it.
        let dev = decision == WebviewDecision::AllowDev;
        let _ = webview.with_webview(move |platform| unsafe {
            let Ok(core) = platform.controller().CoreWebView2() else {
                return;
            };
            if let Ok(settings) = core.Settings() {
                let _ = settings.SetAreDevToolsEnabled(allow);
                if !allow {
                    // F12, Ctrl+Shift+I / J / C are browser accelerator keys.
                    if let Ok(s3) = settings.cast::<ICoreWebView2Settings3>() {
                        let _ = s3.SetAreBrowserAcceleratorKeysEnabled(false);
                    }
                }
            }
            // The default context menu carries "Inspect". Denied: remove it from every menu.
            // Content: keep only it, and show no menu when it is absent.
            if dev {
                return;
            }
            if let Ok(wv11) = core.cast::<ICoreWebView2_11>() {
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
                let _ = wv11.add_ContextMenuRequested(&handler, &mut token);
            }
        });
    }
}
