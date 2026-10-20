//! File-association status and default-handler control (Orthic-Labs/rightkit#670).
//!
//! Donor: ViewRight `commands/mac_associations/*`, generalised. The app passes its bundle id and
//! its type list ([`AssociationSpec`]); nothing here knows a format, an app name or a ProgID.
//!
//! - **macOS**: the default handler of a UTI is read through `NSWorkspace
//!   URLForApplicationToOpenContentType:` (falling back to `LSCopyDefaultRoleHandlerForContentType`)
//!   and set through `NSWorkspace setDefaultApplicationAtURL:toOpenContentType:completionHandler:`
//!   on the main thread, so macOS can show its consent sheet. `LSSetDefaultRoleHandlerForContentType`
//!   runs only when the NSWorkspace selector does not exist, never after a denial or a timeout. The
//!   app can only make itself the default: the running bundle id must equal the spec's. Extensions
//!   are resolved to their preferred UTI and every target is re-read after a set, so the result
//!   reports what took effect.
//! - **Windows**: read-only. Status reads `HKCU\...\FileExts\.ext\UserChoice\ProgId`, then the
//!   `HKCU\Software\Classes\.ext` default, then `HKCR\.ext`, and compares it with the group's
//!   [`AssociationGroup::windows_prog_id`] (case-insensitive). `UserChoice` is protected by a
//!   system hash and is never written: [`set_defaults`] opens Settings > Default apps
//!   ([`open_default_apps_for`]: the app's own page when the spec names it) and reports
//!   `user_action_required`.
//! - **Elsewhere**: `ShellError::Unsupported`.
//!
//! The planning, validation and roll-up logic is pure and compiles on every host. Only the OS calls
//! are platform code (the objc2 runtime already in this crate; the Windows registry through
//! `advapi32` `RegGetValueW`, read-only, with no new crates or `windows` features).
//!
//! # Renderer commands
//!
//! The spec is native state, never renderer input: the app registers it once with [`install`] and the
//! commands [`rk_shell_associations_status`], [`rk_shell_associations_set`] and
//! [`rk_shell_open_default_apps`] take only group ids and a timeout. Each starts with
//! [`crate::caller_gate::require_app_caller`]. Without [`install`] they answer `unsupported`.

use std::collections::HashSet;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager, Runtime, Webview, Wry};

use crate::os_settings::{self, SettingsPane};
use crate::ShellError;

/// How long [`set_defaults`] waits for macOS (consent sheet included) by default.
pub const DEFAULT_SET_TIMEOUT: Duration = Duration::from_secs(60);
/// Bounds applied to a caller-supplied set timeout.
pub const MIN_SET_TIMEOUT: Duration = Duration::from_secs(1);
pub const MAX_SET_TIMEOUT: Duration = Duration::from_secs(120);

/// One logical document type of the app: a UTI list and/or extensions that always change together.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssociationGroup {
    /// Stable id the app uses to select the group (for example `"md"`). Unique within the spec.
    pub id: String,
    /// File extensions with or without the leading dot. Resolved to UTIs on macOS, read per
    /// extension on Windows.
    #[serde(default)]
    pub extensions: Vec<String>,
    /// Extra macOS UTIs to check and set (for example the app's own exported type). Ignored on
    /// Windows.
    #[serde(default)]
    pub utis: Vec<String>,
    /// Windows: the ProgID / class name the app registers for this group. Required for a Windows
    /// status; ignored on macOS.
    #[serde(default)]
    pub windows_prog_id: Option<String>,
}

/// The app's identity and type list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssociationSpec {
    /// macOS bundle identifier of the app (must be the running app to set a default).
    pub bundle_id: String,
    pub groups: Vec<AssociationGroup>,
    /// Windows: the name the app registered under `HKCU\Software\RegisteredApplications`. When set,
    /// opening Default apps goes to the app's own page (`ms-settings:defaultapps?registeredAppUser=`)
    /// instead of the generic list.
    #[serde(default)]
    pub windows_app_name: Option<String>,
}

/// One UTI (macOS) or `extension:.ext` id (Windows) and who handles it now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandlerStatus {
    pub target: String,
    /// Bundle id (macOS) or ProgID (Windows) currently preferred, when one is known.
    pub preferred_handler: Option<String>,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupStatus {
    pub id: String,
    pub extensions: Vec<String>,
    pub handlers: Vec<HandlerStatus>,
    /// True when the group has targets and the app is the default for every one of them.
    pub all_set: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssociationStatus {
    pub groups: Vec<GroupStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SetResult {
    /// Status re-read after the attempt.
    pub groups: Vec<GroupStatus>,
    pub all_set: bool,
    /// Windows: the OS only lets the user choose; Default apps was opened and the user must pick
    /// the app there. Always false on macOS.
    pub user_action_required: bool,
}

// ---------------------------------------------------------------------------------------------
// Pure logic
// ---------------------------------------------------------------------------------------------

fn invalid(message: impl Into<String>) -> ShellError {
    ShellError::Invalid(message.into())
}

/// `.MD` / `md` -> `md`. ASCII letters, digits, `-`, `_`, `+`, at most 32 characters.
pub fn normalize_extension(extension: &str) -> Result<String, ShellError> {
    let ext = extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let ok = !ext.is_empty()
        && ext.len() <= 32
        && ext
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'+'));
    if ok {
        Ok(ext)
    } else {
        Err(invalid(format!(
            "{extension:?} is not a plain file extension"
        )))
    }
}

/// UTIs and bundle ids: ASCII letters, digits, `-` and `.`; no empty or leading/trailing dot parts.
fn validate_dotted_id(what: &str, value: &str) -> Result<String, ShellError> {
    let v = value.trim();
    let ok = !v.is_empty()
        && v.len() <= 255
        && !v.starts_with('.')
        && !v.ends_with('.')
        && !v.contains("..")
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'));
    if ok {
        Ok(v.to_owned())
    } else {
        Err(invalid(format!("{value:?} is not a valid {what}")))
    }
}

fn validate_prog_id(value: &str) -> Result<String, ShellError> {
    let v = value.trim();
    let ok = !v.is_empty() && v.len() <= 255 && !v.chars().any(|c| c.is_control() || c == '\\');
    if ok {
        Ok(v.to_owned())
    } else {
        Err(invalid(format!("{value:?} is not a valid ProgID")))
    }
}

fn push_unique(out: &mut Vec<String>, seen: &mut HashSet<String>, value: String) {
    if seen.insert(value.clone()) {
        out.push(value);
    }
}

/// Validate and normalise a spec: ids, extensions (lowercased, dot stripped, de-duplicated), UTIs,
/// bundle id, ProgIDs and the Windows app name. Every group needs an extension or a UTI.
pub fn validate_spec(spec: &AssociationSpec) -> Result<AssociationSpec, ShellError> {
    let bundle_id = validate_dotted_id("bundle id", &spec.bundle_id)?;
    if spec.groups.is_empty() {
        return Err(invalid("an association spec needs at least one group"));
    }
    let mut ids = HashSet::new();
    let mut groups = Vec::with_capacity(spec.groups.len());
    for g in &spec.groups {
        let id = g.id.trim();
        if id.is_empty() {
            return Err(invalid("a group id cannot be empty"));
        }
        if !ids.insert(id.to_owned()) {
            return Err(invalid(format!("group {id:?} is given twice")));
        }
        let (mut extensions, mut seen) = (Vec::new(), HashSet::new());
        for e in &g.extensions {
            push_unique(&mut extensions, &mut seen, normalize_extension(e)?);
        }
        let (mut utis, mut seen) = (Vec::new(), HashSet::new());
        for u in &g.utis {
            push_unique(&mut utis, &mut seen, validate_dotted_id("UTI", u)?);
        }
        if extensions.is_empty() && utis.is_empty() {
            return Err(invalid(format!("group {id:?} has no extensions or UTIs")));
        }
        let windows_prog_id = g
            .windows_prog_id
            .as_deref()
            .map(validate_prog_id)
            .transpose()?;
        groups.push(AssociationGroup {
            id: id.to_owned(),
            extensions,
            utis,
            windows_prog_id,
        });
    }
    let windows_app_name = match spec.windows_app_name.as_deref() {
        Some(name) => {
            os_settings::default_apps_uri_for(name)?;
            Some(name.trim().to_owned())
        }
        None => None,
    };
    Ok(AssociationSpec {
        bundle_id,
        groups,
        windows_app_name,
    })
}

/// The groups named by `ids` (spec order, de-duplicated), or all of them when `ids` is `None`.
/// An empty list or an unknown id is an error.
pub fn select_groups(
    spec: &AssociationSpec,
    ids: Option<&[String]>,
) -> Result<Vec<AssociationGroup>, ShellError> {
    let Some(ids) = ids else {
        return Ok(spec.groups.clone());
    };
    if ids.is_empty() {
        return Err(invalid("at least one group id is required"));
    }
    let mut wanted = HashSet::new();
    for id in ids {
        if !spec.groups.iter().any(|g| g.id == *id) {
            return Err(invalid(format!("unknown association group: {id}")));
        }
        wanted.insert(id.as_str());
    }
    Ok(spec
        .groups
        .iter()
        .filter(|g| wanted.contains(g.id.as_str()))
        .cloned()
        .collect())
}

/// macOS targets of a group: its UTIs in order, then the preferred UTI of each extension
/// (`resolve`), without duplicates. A UTI the system cannot resolve (`resolvable` is false: not
/// declared by any installed app, such as a third-party editor's type) is skipped, so it is neither
/// reported nor set and cannot hold `all_set` false.
pub fn mac_targets(
    group: &AssociationGroup,
    resolve: &dyn Fn(&str) -> Option<String>,
    resolvable: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    let (mut out, mut seen) = (Vec::new(), HashSet::new());
    for uti in &group.utis {
        if resolvable(uti) {
            push_unique(&mut out, &mut seen, uti.clone());
        }
    }
    for ext in &group.extensions {
        if let Some(uti) = resolve(ext).filter(|u| resolvable(u)) {
            push_unique(&mut out, &mut seen, uti);
        }
    }
    out
}

const WINDOWS_TARGET_PREFIX: &str = "extension:.";

/// Windows target id of an extension: `extension:.md`.
pub fn windows_target(extension: &str) -> String {
    format!("{WINDOWS_TARGET_PREFIX}{extension}")
}

/// Inverse of [`windows_target`].
pub fn windows_target_extension(target: &str) -> Option<&str> {
    target.strip_prefix(WINDOWS_TARGET_PREFIX)
}

/// Windows precedence: the user's explicit choice wins over the registered class default.
pub fn effective_windows_handler(
    user_choice: Option<String>,
    registered: Option<String>,
) -> Option<String> {
    user_choice.or(registered)
}

/// True when `preferred` names the app's handler. Bundle ids and ProgIDs compare ASCII
/// case-insensitively.
pub fn handler_matches(preferred: Option<&str>, expected: &str) -> bool {
    preferred.is_some_and(|p| p.eq_ignore_ascii_case(expected))
}

/// Roll one group up: read each target's handler with `read`, compare with `expected`.
pub fn group_status(
    group: &AssociationGroup,
    expected: &str,
    targets: Vec<String>,
    read: &dyn Fn(&str) -> Option<String>,
) -> GroupStatus {
    let handlers: Vec<HandlerStatus> = targets
        .into_iter()
        .map(|target| {
            let preferred_handler = read(&target);
            HandlerStatus {
                is_default: handler_matches(preferred_handler.as_deref(), expected),
                target,
                preferred_handler,
            }
        })
        .collect();
    let all_set = !handlers.is_empty() && handlers.iter().all(|h| h.is_default);
    GroupStatus {
        id: group.id.clone(),
        extensions: group.extensions.clone(),
        handlers,
        all_set,
    }
}

/// True when every group is set (and there is at least one).
pub fn all_groups_set(groups: &[GroupStatus]) -> bool {
    !groups.is_empty() && groups.iter().all(|g| g.all_set)
}

/// The app can only make itself the default: the running bundle id must equal the spec's.
pub fn ensure_own_bundle(running: Option<&str>, expected: &str) -> Result<(), ShellError> {
    match running {
        Some(r) if r.eq_ignore_ascii_case(expected) => Ok(()),
        Some(r) => Err(invalid(format!(
            "this app is {r}, not {expected}: an app can only make itself the default handler"
        ))),
        None => Err(invalid(
            "the running app has no bundle identifier (run from a .app bundle)",
        )),
    }
}

/// Clamp a caller-supplied timeout to [`MIN_SET_TIMEOUT`]..=[`MAX_SET_TIMEOUT`].
pub fn clamp_timeout(requested: Option<Duration>) -> Duration {
    requested
        .unwrap_or(DEFAULT_SET_TIMEOUT)
        .clamp(MIN_SET_TIMEOUT, MAX_SET_TIMEOUT)
}

/// HKCU subkey holding the user's explicit choice for an extension. Read only: Windows protects
/// this key with a hash, and writing it is not a supported way to set a default.
pub fn user_choice_subkey(extension: &str) -> String {
    format!("Software\\Microsoft\\Windows\\CurrentVersion\\Explorer\\FileExts\\.{extension}\\UserChoice")
}

/// HKCU\Software\Classes subkey (and the HKCR subkey, without the prefix) of an extension.
pub fn classes_subkey(extension: &str) -> (String, String) {
    (
        format!("Software\\Classes\\.{extension}"),
        format!(".{extension}"),
    )
}

/// Decode a `REG_SZ` buffer: up to the first NUL, `None` when empty or not valid UTF-16.
pub fn decode_reg_sz(units: &[u16]) -> Option<String> {
    let end = units.iter().position(|&u| u == 0).unwrap_or(units.len());
    let s = String::from_utf16(&units[..end]).ok()?;
    (!s.is_empty()).then_some(s)
}

// ---------------------------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------------------------

/// Whether the app is the default handler for the selected groups (`None` = all).
/// Blocking: OS service and registry reads. Call from a worker thread.
pub fn status(
    spec: &AssociationSpec,
    group_ids: Option<&[String]>,
) -> Result<AssociationStatus, ShellError> {
    let spec = validate_spec(spec)?;
    let groups = select_groups(&spec, group_ids)?;
    platform::status(&spec.bundle_id, &groups)
}

/// Make the app the default handler for the selected groups.
///
/// macOS: one NSWorkspace request per target that is not already the app's, on the main thread (the
/// consent sheet may appear), sharing one `timeout` deadline; it stops at the first refusal or
/// timeout, then returns the re-read status. Windows: opens Default apps when something is not set
/// and reports `user_action_required`. Blocking: call from a worker thread, never the main thread.
pub fn set_defaults<R: Runtime>(
    app: &AppHandle<R>,
    spec: &AssociationSpec,
    group_ids: Option<&[String]>,
    timeout: Duration,
) -> Result<SetResult, ShellError> {
    let spec = validate_spec(spec)?;
    let groups = select_groups(&spec, group_ids)?;
    platform::set_defaults(app, &spec, &groups, clamp_timeout(Some(timeout)))
}

/// Open the OS page where the person chooses default apps: Windows Settings > Default apps, or
/// System Settings > General on macOS (see [`SettingsPane::DefaultApps`]).
pub fn open_default_apps() -> Result<(), ShellError> {
    os_settings::open_pane(SettingsPane::DefaultApps)
}

/// [`open_default_apps`] for `spec`: on Windows, the app's own Default apps page when the spec
/// names the app ([`AssociationSpec::windows_app_name`]); otherwise the generic one.
pub fn open_default_apps_for(spec: &AssociationSpec) -> Result<(), ShellError> {
    match spec.windows_app_name.as_deref() {
        Some(name) if os_settings::Os::current() == os_settings::Os::Windows => {
            os_settings::open_settings_uri(&os_settings::default_apps_uri_for(name)?)
        }
        _ => open_default_apps(),
    }
}

// ---------------------------------------------------------------------------------------------
// Renderer commands
// ---------------------------------------------------------------------------------------------

/// The app's validated spec as managed state. Create it with [`install`].
pub struct AssociationsConfig {
    spec: AssociationSpec,
}

impl AssociationsConfig {
    /// Validate and keep `spec`.
    pub fn new(spec: AssociationSpec) -> Result<Self, ShellError> {
        Ok(Self {
            spec: validate_spec(&spec)?,
        })
    }

    pub fn spec(&self) -> &AssociationSpec {
        &self.spec
    }
}

/// Register the app's association spec for the renderer commands (call once, in the app's setup).
/// A second call keeps the first.
pub fn install(app: &AppHandle<Wry>, spec: AssociationSpec) -> Result<(), ShellError> {
    let _ = app.manage(AssociationsConfig::new(spec)?);
    Ok(())
}

fn spec_of(app: &AppHandle<Wry>) -> Result<AssociationSpec, ShellError> {
    app.try_state::<AssociationsConfig>()
        .map(|config| config.spec.clone())
        .ok_or_else(|| ShellError::Unsupported("file associations are not configured".into()))
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ShellError> + Send + 'static,
) -> Result<T, ShellError> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| ShellError::Other(e.to_string()))?
}

/// Is the app the default handler for `groups` (`None`: all of the app's groups)?
#[tauri::command]
pub async fn rk_shell_associations_status(
    webview: Webview<Wry>,
    app: AppHandle<Wry>,
    groups: Option<Vec<String>>,
) -> Result<AssociationStatus, ShellError> {
    crate::commands::guard_command(&webview, "rk_shell_associations_status")?;
    crate::caller_gate::require_app_caller(&webview)?;
    let spec = spec_of(&app)?;
    blocking(move || status(&spec, groups.as_deref())).await
}

/// Make the app the default for `groups` (`None`: all). macOS asks the system, which may show a
/// consent sheet; Windows opens Default apps and reports `userActionRequired`. `timeout_ms` is
/// clamped to 1 s .. 120 s.
#[tauri::command]
pub async fn rk_shell_associations_set(
    webview: Webview<Wry>,
    app: AppHandle<Wry>,
    groups: Option<Vec<String>>,
    timeout_ms: Option<u64>,
) -> Result<SetResult, ShellError> {
    crate::commands::guard_command(&webview, "rk_shell_associations_set")?;
    crate::caller_gate::require_app_caller(&webview)?;
    let spec = spec_of(&app)?;
    let timeout = clamp_timeout(timeout_ms.map(Duration::from_millis));
    blocking(move || set_defaults(&app, &spec, groups.as_deref(), timeout)).await
}

/// Open the OS Default apps page.
#[tauri::command]
pub fn rk_shell_open_default_apps(
    webview: Webview<Wry>,
    app: AppHandle<Wry>,
) -> Result<(), ShellError> {
    crate::commands::guard_command(&webview, "rk_shell_open_default_apps")?;
    crate::caller_gate::require_app_caller(&webview)?;
    match spec_of(&app) {
        Ok(spec) => open_default_apps_for(&spec),
        Err(_) => open_default_apps(),
    }
}

// ---------------------------------------------------------------------------------------------
// Platforms
// ---------------------------------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod platform {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::sync_channel;
    use std::time::Instant;

    /// One NSWorkspace request at a time: an unresolved consent sheet ends the action.
    static REQUEST_PENDING: AtomicBool = AtomicBool::new(false);

    fn group_targets(group: &AssociationGroup) -> Vec<String> {
        mac_targets(
            group,
            &mac::preferred_uti_for_extension,
            &mac::uti_resolvable,
        )
    }

    pub fn status(
        bundle_id: &str,
        groups: &[AssociationGroup],
    ) -> Result<AssociationStatus, ShellError> {
        Ok(AssociationStatus {
            groups: groups
                .iter()
                .map(|g| group_status(g, bundle_id, group_targets(g), &mac::preferred_handler))
                .collect(),
        })
    }

    pub fn set_defaults<R: Runtime>(
        app: &AppHandle<R>,
        spec: &AssociationSpec,
        groups: &[AssociationGroup],
        timeout: Duration,
    ) -> Result<SetResult, ShellError> {
        let bundle_id = spec.bundle_id.as_str();
        ensure_own_bundle(mac::running_bundle_id().as_deref(), bundle_id)?;
        let (mut targets, mut seen) = (Vec::new(), HashSet::new());
        for t in groups.iter().flat_map(group_targets) {
            push_unique(&mut targets, &mut seen, t);
        }
        let deadline = Instant::now() + timeout;
        for uti in &targets {
            if !set_one(app, uti, bundle_id, deadline) {
                break;
            }
        }
        let status = status(bundle_id, groups)?;
        Ok(SetResult {
            all_set: all_groups_set(&status.groups),
            groups: status.groups,
            user_action_required: false,
        })
    }

    fn finish(tx: &std::sync::mpsc::SyncSender<bool>, ok: bool) {
        REQUEST_PENDING.store(false, Ordering::Release);
        let _ = tx.send(ok);
    }

    fn set_one<R: Runtime>(
        app: &AppHandle<R>,
        uti: &str,
        bundle_id: &str,
        deadline: Instant,
    ) -> bool {
        if handler_matches(mac::preferred_handler(uti).as_deref(), bundle_id) {
            return true;
        }
        if REQUEST_PENDING
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return false;
        }
        let (tx, rx) = sync_channel::<bool>(1);
        let (uti_owned, bundle_owned) = (uti.to_owned(), bundle_id.to_owned());
        let queued = app.run_on_main_thread(move || {
            if mac::start_set(&uti_owned, tx.clone()).is_none() {
                // The modern selector does not exist (older macOS): legacy API.
                finish(&tx, mac::set_legacy(&uti_owned, &bundle_owned));
            }
        });
        if queued.is_err() {
            REQUEST_PENDING.store(false, Ordering::Release);
            return false;
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        !remaining.is_zero() && matches!(rx.recv_timeout(remaining), Ok(true))
    }

    mod mac {
        use std::cell::RefCell;
        use std::sync::mpsc::SyncSender;

        use block2::RcBlock;
        use objc2::msg_send;
        use objc2::rc::Retained;
        use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
        use objc2_foundation::{NSError, NSString};

        #[link(name = "AppKit", kind = "framework")]
        extern "C" {}
        #[link(name = "UniformTypeIdentifiers", kind = "framework")]
        extern "C" {}

        // CFStringRef and NSString are toll-free bridged; the Copy/Create functions return +1.
        #[link(name = "CoreServices", kind = "framework")]
        extern "C" {
            #[allow(deprecated)]
            fn LSCopyDefaultRoleHandlerForContentType(
                content_type: *const NSString,
                roles: u32,
            ) -> *mut NSString;
            #[allow(deprecated)]
            fn LSSetDefaultRoleHandlerForContentType(
                content_type: *const NSString,
                roles: u32,
                handler_bundle_id: *const NSString,
            ) -> i32;
            #[allow(deprecated)]
            fn UTTypeCreatePreferredIdentifierForTag(
                tag_class: *const NSString,
                tag: *const NSString,
                conforming_to: *const NSString,
            ) -> *mut NSString;
        }

        /// kLSRolesEditor.
        const LS_ROLES_EDITOR: u32 = 1 << 2;

        type Completion = RcBlock<dyn Fn(*mut NSError)>;

        thread_local! {
            /// AppKit may invoke the completion after the request call returns; keep it alive on
            /// the main thread until it runs.
            static PENDING_COMPLETION: RefCell<Option<Completion>> = const { RefCell::new(None) };
        }

        fn owned_string(raw: *mut NSString) -> Option<String> {
            // SAFETY: Copy/Create functions return a +1 retained CFString (an NSString) or NULL.
            let s = unsafe { Retained::from_raw(raw) }?;
            Some(s.to_string())
        }

        /// kUTTagClassFilenameExtension -> preferred UTI.
        pub fn preferred_uti_for_extension(extension: &str) -> Option<String> {
            let class = NSString::from_str("public.filename-extension");
            let tag = NSString::from_str(extension);
            // SAFETY: both arguments are live NSStrings; the third may be NULL.
            let raw =
                unsafe { UTTypeCreatePreferredIdentifierForTag(&*class, &*tag, std::ptr::null()) };
            owned_string(raw)
        }

        pub fn running_bundle_id() -> Option<String> {
            let cls = AnyClass::get(c"NSBundle")?;
            // SAFETY: plain Foundation accessors.
            unsafe {
                let main: Option<Retained<AnyObject>> = msg_send![cls, mainBundle];
                let id: Option<Retained<NSString>> = msg_send![&*main?, bundleIdentifier];
                Some(id?.to_string())
            }
        }

        fn workspace() -> Option<Retained<AnyObject>> {
            let cls = AnyClass::get(c"NSWorkspace")?;
            // SAFETY: `+[NSWorkspace sharedWorkspace]`.
            unsafe { msg_send![cls, sharedWorkspace] }
        }

        fn responds(object: &AnyObject, selector: &std::ffi::CStr) -> bool {
            let sel = Sel::register(selector);
            // SAFETY: `respondsToSelector:` takes a SEL and returns BOOL.
            let r: Bool = unsafe { msg_send![object, respondsToSelector: sel] };
            r.as_bool()
        }

        /// True when the system knows the UTI (`UTType typeWithIdentifier:` is not nil).
        pub fn uti_resolvable(uti: &str) -> bool {
            ut_type(uti).is_some()
        }

        fn ut_type(uti: &str) -> Option<Retained<AnyObject>> {
            let cls = AnyClass::get(c"UTType")?;
            let id = NSString::from_str(uti);
            // SAFETY: `+[UTType typeWithIdentifier:]` takes an NSString and returns UTType or nil.
            unsafe { msg_send![cls, typeWithIdentifier: &*id] }
        }

        fn ns_workspace_handler(uti: &str) -> Option<String> {
            let ut = ut_type(uti)?;
            let ws = workspace()?;
            if !responds(&ws, c"URLForApplicationToOpenContentType:") {
                return None;
            }
            let bundle_cls = AnyClass::get(c"NSBundle")?;
            // SAFETY: each selector is a one-object-argument accessor on the class named.
            unsafe {
                let url: Option<Retained<AnyObject>> =
                    msg_send![&*ws, URLForApplicationToOpenContentType: &*ut];
                let bundle: Option<Retained<AnyObject>> =
                    msg_send![bundle_cls, bundleWithURL: &*url?];
                let id: Option<Retained<NSString>> = msg_send![&*bundle?, bundleIdentifier];
                Some(id?.to_string())
            }
        }

        /// Bundle id of the default editor of `uti`: NSWorkspace first, LaunchServices second.
        pub fn preferred_handler(uti: &str) -> Option<String> {
            if let Some(handler) = ns_workspace_handler(uti) {
                return Some(handler);
            }
            let id = NSString::from_str(uti);
            // SAFETY: `id` is a live NSString.
            let raw = unsafe { LSCopyDefaultRoleHandlerForContentType(&*id, LS_ROLES_EDITOR) };
            owned_string(raw)
        }

        /// Legacy LaunchServices set. Only when the NSWorkspace selector does not exist.
        pub fn set_legacy(uti: &str, bundle_id: &str) -> bool {
            let (uti, bundle) = (NSString::from_str(uti), NSString::from_str(bundle_id));
            // SAFETY: both arguments are live NSStrings.
            unsafe { LSSetDefaultRoleHandlerForContentType(&*uti, LS_ROLES_EDITOR, &*bundle) == 0 }
        }

        /// Begin one NSWorkspace set for the running app, on AppKit's main thread. `None`: the
        /// selector is unavailable, the caller may use [`set_legacy`]. `Some(())`: the outcome
        /// (denial included) arrives through `finish`; never fall back to the legacy API then.
        pub fn start_set(uti: &str, tx: SyncSender<bool>) -> Option<()> {
            use super::finish;
            let ut = ut_type(uti);
            let ws = workspace()?;
            if !responds(
                &ws,
                c"setDefaultApplicationAtURL:toOpenContentType:completionHandler:",
            ) {
                return None;
            }
            let Some(ut) = ut else {
                finish(&tx, false);
                return Some(());
            };
            let bundle_cls = AnyClass::get(c"NSBundle")?;
            // SAFETY: `+[NSBundle mainBundle]` and `-[NSBundle bundleURL]` take no arguments.
            let url: Option<Retained<AnyObject>> = unsafe {
                let main: Option<Retained<AnyObject>> = msg_send![bundle_cls, mainBundle];
                match main {
                    Some(m) => msg_send![&*m, bundleURL],
                    None => None,
                }
            };
            let Some(url) = url else {
                finish(&tx, false);
                return Some(());
            };
            let tx_block = tx.clone();
            let block: Completion = RcBlock::new(move |error: *mut NSError| {
                finish(&tx_block, error.is_null());
                PENDING_COMPLETION.with(|p| {
                    p.borrow_mut().take();
                });
            });
            PENDING_COMPLETION.with(|p| *p.borrow_mut() = Some(block.clone()));
            // SAFETY: the selector takes (NSURL*, UTType*, void (^)(NSError*)) and returns void;
            // the block stays alive in PENDING_COMPLETION until it runs.
            unsafe {
                let _: () = msg_send![
                    &*ws,
                    setDefaultApplicationAtURL: &*url,
                    toOpenContentType: &*ut,
                    completionHandler: &*block
                ];
            }
            Some(())
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;

    // The predefined registry roots, as the sign-extended handle values the API defines.
    const HKEY_CLASSES_ROOT: isize = 0x8000_0000_u32 as i32 as isize;
    const HKEY_CURRENT_USER: isize = 0x8000_0001_u32 as i32 as isize;
    /// Restrict `RegGetValueW` to `REG_SZ` (it also NUL-terminates the data).
    const RRF_RT_REG_SZ: u32 = 0x0000_0002;

    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(
            key: isize,
            sub_key: *const u16,
            value: *const u16,
            flags: u32,
            kind: *mut u32,
            data: *mut std::ffi::c_void,
            data_len: *mut u32,
        ) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// One `REG_SZ` value (`None` name = the key's default value). Read-only.
    fn reg_string(root: isize, subkey: &str, value: Option<&str>) -> Option<String> {
        let sub = wide(subkey);
        let val = value.map(wide);
        let val_ptr = val.as_ref().map_or(std::ptr::null(), |v| v.as_ptr());
        let mut size: u32 = 0;
        // SAFETY: `sub`/`val` are NUL-terminated and outlive the call; `size` is a valid out-pointer
        // and a null data pointer asks only for the needed size.
        let status = unsafe {
            RegGetValueW(
                root,
                sub.as_ptr(),
                val_ptr,
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut size,
            )
        };
        if status != 0 || size < 2 {
            return None;
        }
        let mut buf = vec![0u16; (size as usize).div_ceil(2) + 1];
        let mut size2 = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `size2` bytes; RegGetValueW writes at most that many.
        let status = unsafe {
            RegGetValueW(
                root,
                sub.as_ptr(),
                val_ptr,
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut size2,
            )
        };
        if status != 0 {
            return None;
        }
        decode_reg_sz(&buf[..(size2 as usize / 2).min(buf.len())])
    }

    fn handler_for_extension(extension: &str) -> Option<String> {
        let (hkcu_classes, hkcr) = classes_subkey(extension);
        effective_windows_handler(
            reg_string(
                HKEY_CURRENT_USER,
                &user_choice_subkey(extension),
                Some("ProgId"),
            ),
            reg_string(HKEY_CURRENT_USER, &hkcu_classes, None)
                .or_else(|| reg_string(HKEY_CLASSES_ROOT, &hkcr, None)),
        )
    }

    pub fn status(
        _bundle_id: &str,
        groups: &[AssociationGroup],
    ) -> Result<AssociationStatus, ShellError> {
        let mut out = Vec::with_capacity(groups.len());
        for g in groups {
            let prog = g
                .windows_prog_id
                .as_deref()
                .ok_or_else(|| invalid(format!("group {:?} has no windowsProgId", g.id)))?;
            let targets = g.extensions.iter().map(|e| windows_target(e)).collect();
            out.push(group_status(g, prog, targets, &|target| {
                windows_target_extension(target).and_then(handler_for_extension)
            }));
        }
        Ok(AssociationStatus { groups: out })
    }

    pub fn set_defaults<R: Runtime>(
        _app: &AppHandle<R>,
        spec: &AssociationSpec,
        groups: &[AssociationGroup],
        _timeout: Duration,
    ) -> Result<SetResult, ShellError> {
        // UserChoice is protected by a system hash: never write it, never fake success.
        let status = status(&spec.bundle_id, groups)?;
        let all_set = all_groups_set(&status.groups);
        if !all_set {
            open_default_apps_for(spec)?;
        }
        Ok(SetResult {
            all_set,
            groups: status.groups,
            user_action_required: !all_set,
        })
    }
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
mod platform {
    use super::*;

    pub fn status(
        _bundle_id: &str,
        _groups: &[AssociationGroup],
    ) -> Result<AssociationStatus, ShellError> {
        Err(ShellError::Unsupported(
            "file associations are managed on macOS and Windows only".into(),
        ))
    }

    pub fn set_defaults<R: Runtime>(
        _app: &AppHandle<R>,
        _spec: &AssociationSpec,
        _groups: &[AssociationGroup],
        _timeout: Duration,
    ) -> Result<SetResult, ShellError> {
        Err(ShellError::Unsupported(
            "file associations are managed on macOS and Windows only".into(),
        ))
    }
}
