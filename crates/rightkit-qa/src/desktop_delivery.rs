//! Delivery journeys into other applications: a target-app matrix, the receipt a case records, the
//! check of a delivery receipt's pasteboard evidence, and (macOS) the drivers that prepare a target
//! app, deliver text into it, read back what landed through the Accessibility API and restore the
//! clipboard.
//!
//! The journey delivers text into the operator's real apps (TextEdit, Safari, Chrome, Terminal, ...).
//! HARD RULE (2026-09-02 incident): nothing here quits, closes, opens a new window in, or
//! select-all-deletes a target application, and never presses Cmd+A, Cmd+N or Cmd+V in one. The
//! delivery itself is the host's: [`run_case`] takes it as a closure that calls whatever command the
//! app registers on its control server and returns the delivery receipt.
//!
//! The matrix, the sentinel and fixture names, the placeholder strings and the options come from the
//! host ([`DeliveryConfig`], [`TargetCase`], [`DeliveryOptions`]).

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Map, Value};

use crate::harness::Scenario;

/// One target app and how it is prepared.
#[derive(Clone, Copy, Debug)]
pub struct TargetCase {
    /// Case id (for example `textedit`).
    pub id: &'static str,
    /// Application name as `open -a` knows it.
    pub app: &'static str,
    /// Bundle identifier the frontmost process must report.
    pub bundle_id: &'static str,
    /// `native`, `browser`, `electron` or `web`.
    pub kind: &'static str,
    /// The focused field is a secure text field: delivery must fail closed to the clipboard.
    pub secure_field: bool,
    /// A logged-in web surface (`chatgpt`, `claude`, `gemini`) the case needs, if any.
    pub surface: Option<&'static str>,
    /// Host the front browser tab must show, if any.
    pub expected_host: Option<&'static str>,
    /// The case is a chat composer the AX API alone cannot focus: click where it sits, and refuse to
    /// seed over a non-empty operator draft. A case with a `surface` behaves the same. Default false;
    /// [`target_case`] sets it for the reserved ids `chatgpt` and `claude`.
    pub composer_click: bool,
    /// After a paste-only run, undo the observed paste (single Cmd+Z) so the sentinel is not left in
    /// the operator's real composer. A case with a `surface` behaves the same. Default false;
    /// [`target_case`] sets it for the reserved ids `chatgpt` and `claude`.
    pub undo_paste_on_cleanup: bool,
    /// After a paste-only run, interrupt (Ctrl+C) the line left in a terminal. Default false;
    /// [`target_case`] sets it for the reserved id `terminal`.
    pub interrupt_on_cleanup: bool,
    /// After a sent run, press Escape in the same app to halt generation and prove it stopped.
    /// Default false; [`target_case`] sets it for the reserved ids `chatgpt` and `claude`.
    pub escape_after_send: bool,
}

const fn same_id(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// A [`TargetCase`] in one expression (usable in a `const` matrix). The cleanup flags default to
/// false, except for the reserved ids `chatgpt`, `claude` and `terminal`, which keep their documented
/// cleanup; the `with_*` setters change any flag for a differently named case.
pub const fn target_case(
    id: &'static str,
    app: &'static str,
    bundle_id: &'static str,
    kind: &'static str,
    secure_field: bool,
    surface: Option<&'static str>,
    expected_host: Option<&'static str>,
) -> TargetCase {
    let chat = same_id(id, "chatgpt") || same_id(id, "claude");
    TargetCase {
        id,
        app,
        bundle_id,
        kind,
        secure_field,
        surface,
        expected_host,
        composer_click: chat,
        undo_paste_on_cleanup: chat,
        interrupt_on_cleanup: same_id(id, "terminal"),
        escape_after_send: chat,
    }
}

impl TargetCase {
    /// Set [`TargetCase::composer_click`].
    pub const fn with_composer_click(mut self, on: bool) -> Self {
        self.composer_click = on;
        self
    }
    /// Set [`TargetCase::undo_paste_on_cleanup`].
    pub const fn with_undo_paste_on_cleanup(mut self, on: bool) -> Self {
        self.undo_paste_on_cleanup = on;
        self
    }
    /// Set [`TargetCase::interrupt_on_cleanup`].
    pub const fn with_interrupt_on_cleanup(mut self, on: bool) -> Self {
        self.interrupt_on_cleanup = on;
        self
    }
    /// Set [`TargetCase::escape_after_send`].
    pub const fn with_escape_after_send(mut self, on: bool) -> Self {
        self.escape_after_send = on;
        self
    }
    /// The matrix row as the receipt records it.
    pub fn to_value(&self) -> Value {
        json!({
            "id": self.id, "app": self.app, "bundleId": self.bundle_id,
            "kind": self.kind, "secureField": self.secure_field,
            "surface": self.surface, "expectedHost": self.expected_host,
        })
    }
}

/// What the operator asked for.
#[derive(Clone, Copy, Debug, Default)]
pub struct DeliveryOptions {
    /// Record whether the target was cold (never quits an app).
    pub cold: bool,
    /// Seed text first, then deliver.
    pub existing_text: bool,
    /// Also submit the text.
    pub send: bool,
    /// Allow the logged-in web surfaces.
    pub web: bool,
}

/// Milliseconds since the Unix epoch.
pub fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

/// `value[path[0]][path[1]]...`.
pub fn json_at<'a>(value: &'a Value, path: &[&str]) -> &'a Value {
    let mut current = value;
    for key in path {
        current = &current[*key];
    }
    current
}

/// The state a case reaches, as the receipt records it.
pub struct CaseResult {
    /// Case id.
    pub id: &'static str,
    /// Application name.
    pub app: &'static str,
    /// What each phase recorded.
    pub phases: Map<String, Value>,
    /// `pass`, `fail`, `skipped` or `blocked`.
    pub status: &'static str,
    /// Why it failed.
    pub error: Option<String>,
    /// Why it was skipped.
    pub reason: Option<String>,
}

impl CaseResult {
    /// A `blocked` result for `entry`, before it has run.
    pub fn new(entry: &TargetCase) -> CaseResult {
        CaseResult {
            id: entry.id,
            app: entry.app,
            phases: Map::new(),
            status: "blocked",
            error: None,
            reason: None,
        }
    }

    /// The receipt row.
    pub fn to_value(&self) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), json!(self.id));
        out.insert("app".into(), json!(self.app));
        out.insert("phases".into(), Value::Object(self.phases.clone()));
        out.insert("status".into(), json!(self.status));
        if let Some(error) = &self.error {
            out.insert("error".into(), json!(error));
        }
        if let Some(reason) = &self.reason {
            out.insert("reason".into(), json!(reason));
        }
        Value::Object(out)
    }
}

/// Flags set while a case runs that the cleanup needs.
#[derive(Default)]
pub struct Cleanup {
    /// The case seeded existing text.
    pub seeded_existing_text: bool,
    /// The second delivery reported `pasted`.
    pub second_delivery_pasted: bool,
}

/// The delivery receipt must say how the text went in (native AX write, clipboard paste, or the clipboard
/// fallback) and carry the ordered proof steps for that path (`delivery.delivery_timing.steps` names:
/// `native_selected_text_write`, `native_capability_probe`, `clipboard_snapshot`,
/// `promised_clipboard_stage`, `paste_keystroke`, `paste_receipt_observed`, `fallback_clipboard_write`).
/// Records the path under the case's `promisedPasteboard` phase.
pub fn assert_promised_pasteboard_evidence(
    delivery: &Value,
    result: &mut CaseResult,
    original_clipboard: &str,
    clipboard_after: &str,
) -> Result<(), String> {
    let record = &delivery["delivery"];
    let outcome_kind = record["outcome"]["kind"].as_str();
    let steps = record["delivery_timing"]["steps"]
        .as_array()
        .ok_or("delivery receipt has no timing steps")?;
    if steps.iter().any(|step| {
        !step.is_object()
            || !step["elapsed_ms"]
                .as_f64()
                .is_some_and(|ms| ms.is_finite() && ms >= 0.0)
    }) {
        return Err("delivery receipt contains invalid timing evidence".into());
    }
    let names: Vec<&str> = steps
        .iter()
        .filter_map(|step| step["name"].as_str())
        .collect();
    let has = |name: &str| names.contains(&name);
    let native = has("native_selected_text_write");
    let clipboard = has("clipboard_snapshot");
    let fallback = has("fallback_clipboard_write");
    if !native && !clipboard && !fallback {
        return Err("delivery receipt does not identify an insertion or fallback path".into());
    }
    if fallback && outcome_kind != Some("copied_fallback") {
        return Err("fallback receipt does not report copied_fallback outcome".into());
    }
    if native {
        if !has("native_capability_probe") {
            return Err("native delivery receipt is missing capability probe evidence".into());
        }
        if clipboard_after != original_clipboard {
            return Err("native delivery changed clipboard unexpectedly".into());
        }
    }
    if clipboard {
        if outcome_kind == Some("pasted") {
            if !has("promised_clipboard_stage") {
                return Err(
                    "promised pasteboard receipt is missing promised_clipboard_stage".into(),
                );
            }
            for required in ["paste_keystroke", "paste_receipt_observed"] {
                if !has(required) {
                    return Err(format!("promised pasteboard receipt is missing {required}"));
                }
            }
            // The restore (quiet window + restore_if_unchanged) now runs on a background
            // thread after Pasted (49ae297d + #299 latency change), so it is no longer a
            // delivery step; it is logged as delivery_clipboard_restore_background and
            // the clipboard_after == original_clipboard check covers the outcome.
        } else if outcome_kind == Some("copied_fallback")
            && !fallback
            && !has("fallback_clipboard_write")
        {
            return Err("fallback receipt lacks fallback_clipboard_write proof".into());
        }
    }
    result.phases.insert(
        "promisedPasteboard".into(),
        json!({
            "path": if native { "native_ax" } else if clipboard { "clipboard_paste" } else { "clipboard_fallback" },
            "steps": names,
            "clipboardRestored": clipboard_after == original_clipboard,
        }),
    );
    Ok(())
}

/// The `.app` under test: an explicit path, else the one fixed staged copy at
/// `<staged_root>/<bundle_name>` (one install path, so macOS privacy grants made for it persist across
/// runs). Never the live install `live_install`. Panics when the path is the live install, is not a
/// `.app` or does not exist.
pub fn resolve_app_under_test(
    explicit: Option<PathBuf>,
    staged_root: Option<PathBuf>,
    default_staged_root: &Path,
    bundle_name: &str,
    live_install: &Path,
) -> PathBuf {
    let staged = staged_root
        .unwrap_or_else(|| default_staged_root.to_path_buf())
        .join(bundle_name);
    let path = explicit.unwrap_or(staged);
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    assert_ne!(
        path,
        live_install,
        "refusing to run QA against the live {}; stage the QA copy",
        live_install.display()
    );
    assert!(
        path.extension().is_some_and(|ext| ext == "app"),
        "the journey needs an installed .app path, got {}",
        path.display()
    );
    assert!(path.exists(), "installed app missing: {}", path.display());
    path
}

/// A boolean setting from the scenario's settings (absent or invalid is false).
pub fn setting_flag(sc: &Scenario, key: &str) -> bool {
    sc.settings().get_bool(key).ok().flatten().unwrap_or(false)
}

/// Write `receipt` pretty-printed (plus a newline) to `path`, keep it in the scenario evidence under
/// `name` and print it. Returns the body.
pub fn write_receipt(sc: &Scenario, name: &str, path: &Path, receipt: &Value) -> String {
    let body = serde_json::to_string_pretty(receipt).expect("receipt json");
    std::fs::write(path, format!("{body}\n")).expect("write receipt");
    sc.keep(name, path);
    println!("{body}");
    body
}

/// The failed cases as `id: error` lines.
pub fn failed_cases(results: &[CaseResult]) -> Vec<String> {
    results
        .iter()
        .filter(|r| r.status == "fail")
        .map(|r| format!("{}: {}", r.id, r.error.as_deref().unwrap_or("failed")))
        .collect()
}

/// What the host names for a delivery run.
pub struct DeliveryConfig<'a> {
    /// Prefix of the text delivered (`<prefix> <case id> <ms>`).
    pub sentinel_prefix: &'a str,
    /// Prefix of the seeded existing text (`<prefix> <case id> <ms> `).
    pub existing_prefix: &'a str,
    /// Prefix of fixture file and folder names in the temp folder (`<prefix>-secure.html`, ...).
    pub fixture_prefix: &'a str,
    /// The VS Code command-line launcher.
    pub vscode_cli: PathBuf,
    /// Composer placeholder texts that count as an empty draft in a chat app.
    pub placeholders: &'a [&'a str],
    /// The option or setting that allows web surfaces, named in the skip reason.
    pub web_option_name: &'a str,
}

/// The URL a web surface opens (`chatgpt` its site, any other the Claude site).
pub fn surface_url(surface: &str) -> &'static str {
    if surface == "chatgpt" {
        "https://chatgpt.com"
    } else {
        "https://claude.ai"
    }
}

/// Where a chat composer sits in a target window: `(x fraction, bottom inset in points)`.
pub fn composer_region(surface: &str) -> (f64, f64) {
    let x_fraction = if surface == "gemini" { 0.65 } else { 0.5 };
    let bottom_inset = match surface {
        "chatgpt" => 90.0,
        "gemini" => 105.0,
        _ => 150.0,
    };
    (x_fraction, bottom_inset)
}

#[cfg(target_os = "macos")]
use crate::desktop_mac as mac;

#[cfg(target_os = "macos")]
fn write_fixture(path: &Path, body: &str) -> Result<(), String> {
    std::fs::write(path, body).map_err(|e| format!("fixture {}: {e}", path.display()))
}

/// Prepare the target app for `entry`: launch it, open a fresh scratch fixture where the case needs
/// one (a password page, a textarea page, a scratch file), and leave keyboard focus where delivery
/// lands. Never quits or closes anything, never opens a document in an app that already has the
/// operator's.
#[cfg(target_os = "macos")]
pub fn prepare_target(entry: &TargetCase, config: &DeliveryConfig) -> Result<(), String> {
    let tmp = std::env::temp_dir();
    let prefix = config.fixture_prefix;
    if let Some(host) = entry.expected_host {
        mac::launch(entry.app, None)?;
        let active = mac::read_active_browser_url(entry.app);
        if !active.contains(host) {
            return Err(format!(
                "live browser target mismatch: expected {host}, got {}",
                if active.is_empty() {
                    "<unknown>"
                } else {
                    &active
                }
            ));
        }
        return Ok(());
    }
    if entry.secure_field {
        let fixture = tmp.join(&format!("{prefix}-secure.html"));
        write_fixture(
            &fixture,
            "<!doctype html><meta charset=\"utf-8\"><input autofocus type=\"password\" aria-label=\"QA secure field\">\n",
        )?;
        mac::launch(entry.app, Some(&format!("file://{}", fixture.display())))?;
        // Safari leaves keyboard focus in its address field after `open -a ... URL`. One Tab moves
        // into this single-control fixture deterministically.
        mac::key_code(48)?;
        mac::sleep(0.2);
        return Ok(());
    }
    if entry.id == "chrome" {
        let fixture = tmp.join(format!("{prefix}-editable.html"));
        write_fixture(
            &fixture,
            "<!doctype html><meta charset=\"utf-8\"><textarea autofocus aria-label=\"QA editable field\"></textarea>\n",
        )?;
        mac::launch(entry.app, Some(&format!("file://{}", fixture.display())))?;
        // Final focus is applied after System Events makes Chrome frontmost.
        mac::sleep(0.2);
        return Ok(());
    }
    if entry.id == "vscode" {
        let root = tmp.join(format!("{prefix}-vscode"));
        let fixture = root.join(format!("{prefix}-vscode.txt"));
        std::fs::create_dir_all(&root).map_err(|e| format!("fixture folder: {e}"))?;
        write_fixture(&fixture, "")?;
        let code_cli = config.vscode_cli.as_path();
        if !code_cli.exists() {
            return Err(format!("VS Code CLI missing: {}", code_cli.display()));
        }
        let status = std::process::Command::new(code_cli)
            .arg("--reuse-window")
            .arg(&fixture)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|e| format!("code: {e}"))?;
        if !status.success() {
            return Err(format!("code --reuse-window exited with {status}"));
        }
        mac::sleep(2.0);
        return Ok(());
    }
    if entry.id == "textedit" {
        // Open a NEW scratch document by file (never Cmd+N, never touching the operator's open
        // documents): a fresh empty .txt each run.
        let fixture = tmp.join(format!("{prefix}-textedit-{}.txt", now_ms()));
        write_fixture(&fixture, "")?;
        let status = std::process::Command::new("open")
            .arg("-a")
            .arg(entry.app)
            .arg(&fixture)
            .status()
            .map_err(|e| format!("open: {e}"))?;
        if !status.success() {
            return Err(format!("open -a {} exited with {status}", entry.app));
        }
        mac::osascript(&format!(
            "tell application {} to activate",
            mac::quote(entry.app)
        ))?;
        mac::sleep(1.0);
        return Ok(());
    }
    mac::launch(entry.app, None)?;
    if entry.kind == "browser" {
        mac::key("l", Some("command"))?;
    }
    // No Cmd+N / new-conversation / new-document for Notes or any chat app: the operator must have
    // a blank scratch note / a sacrificial chat focused (2026-09-02 incident). `--web` surface
    // launches are gated off in staged runs.
    if let Some(surface) = entry.surface {
        mac::launch(entry.app, Some(surface_url(surface)))?;
    }
    Ok(())
}

/// After the target is prepared: VS Code gets its first editor focus, and the cases that restore a
/// window or load a page get a moment to settle.
#[cfg(target_os = "macos")]
pub fn stabilize_prepared_target(entry: &TargetCase) -> Result<(), String> {
    if entry.id == "vscode" {
        mac::key("1", Some("command"))?;
    }
    if entry.id == "chrome" || entry.id == "vscode" || entry.surface.is_some() {
        mac::sleep(0.5);
    }
    Ok(())
}

/// Click where a chat composer sits in the target window (a composer the AX API alone cannot focus);
/// nothing for other cases.
#[cfg(target_os = "macos")]
pub fn focus_composer_region(entry: &TargetCase) -> Result<(), String> {
    if !entry.composer_click && entry.surface.is_none() {
        return Ok(());
    }
    let surface = entry.surface.unwrap_or(entry.id);
    let (x_fraction, bottom_inset) = composer_region(surface);
    mac::click_composer_region(entry.bundle_id, x_fraction, bottom_inset)
}

/// Deliver into the focused field: `(text, send)` to the delivery receipt.
#[cfg(target_os = "macos")]
pub type Deliver<'a> = &'a dyn Fn(&str, bool) -> Result<Value, String>;

/// The case itself. An early `Ok` with a status already set (skipped, or a refused unsafe state) ends
/// the case; an `Err` is a failure.
#[cfg(target_os = "macos")]
pub fn run_case_body(
    deliver: Deliver,
    config: &DeliveryConfig,
    entry: &TargetCase,
    options: &DeliveryOptions,
    result: &mut CaseResult,
    cleanup: &mut Cleanup,
    original_clipboard: &str,
) -> Result<(), String> {
    let sentinel = format!("{} {} {}", config.sentinel_prefix, entry.id, now_ms());
    if !mac::is_installed(entry.app) {
        result.status = "skipped";
        result.reason = Some("target app is not installed".into());
        return Ok(());
    }
    if entry.surface.is_some() && !options.web {
        result.status = "skipped";
        result.reason = Some(format!(
            "web surface {} requires {} and a logged-in browser fixture",
            entry.surface.unwrap_or(""),
            config.web_option_name
        ));
        return Ok(());
    }
    if options.cold && mac::is_running(entry.app) {
        // Do NOT quit the operator's app. Record that the requested cold state was not met and
        // let the case run warm.
        result.phases.insert(
            "coldState".into(),
            json!({
                "requested": true,
                "met": false,
                "note": format!("{} was already running; the journey will not quit a user app. Quit it by hand for a true cold-tree run.", entry.app),
            }),
        );
    } else if options.cold {
        result.phases.insert(
            "coldState".into(),
            json!({ "requested": true, "met": true }),
        );
    }
    prepare_target(entry, config)?;
    mac::focus(entry.bundle_id)?;
    result.phases.insert(
        "coldFocus".into(),
        json!({ "app": entry.app, "frontmost": true, "requested": options.cold }),
    );
    stabilize_prepared_target(entry)?;
    focus_composer_region(entry)?;
    let mut identity = mac::read_frontmost_identity();
    // The QA app's native splash can finish its handoff after target activation on a one-case run.
    // Reassert target focus instead of testing the wrong process.
    let mut attempt = 0;
    while identity.bundle_id != entry.bundle_id && attempt < 4 {
        mac::sleep(0.5);
        mac::focus(entry.bundle_id)?;
        stabilize_prepared_target(entry)?;
        focus_composer_region(entry)?;
        identity = mac::read_frontmost_identity();
        attempt += 1;
    }
    result.phases.insert(
        "target".into(),
        json!({ "name": identity.name, "bundleId": identity.bundle_id, "pid": identity.pid }),
    );
    if identity.bundle_id != entry.bundle_id {
        return Err(format!(
            "target bundle mismatch: expected {}, got {}",
            entry.bundle_id,
            if identity.bundle_id.is_empty() {
                "<unknown>"
            } else {
                &identity.bundle_id
            }
        ));
    }
    // Notes, or any editor the journey cannot open a fresh scratch document in: never type into a
    // field that already holds the operator's content. Require an empty focused field.
    if entry.id == "notes" {
        let existing = mac::read_focused_ax();
        if !existing.trim().is_empty() {
            result.status = "fail";
            result.error = Some(
                "notes case needs a BLANK scratch note focused: the focused field is not empty, refusing to type into the operator's content"
                    .into(),
            );
            return Ok(());
        }
    }
    let mut field = mac::read_focused_ax_metadata();
    if entry.secure_field && !field.secure {
        mac::key_code(48)?;
        mac::sleep(0.2);
        field = mac::read_focused_ax_metadata();
    }
    result.phases.insert(
        "field".into(),
        json!({ "role": field.role, "subrole": field.subrole, "secure": field.secure }),
    );
    if entry.secure_field && !field.secure {
        return Err("secure-field case did not focus an AX secure text field".into());
    }
    let initial = mac::read_focused_ax();
    let existing_prefix = format!("{} {} {} ", config.existing_prefix, entry.id, now_ms());
    if options.existing_text {
        if (entry.composer_click || entry.surface.is_some())
            && !initial.trim().is_empty()
            && !config.placeholders.contains(&initial.trim())
        {
            return Err("existing-text case refuses to modify a non-empty operator draft".into());
        }
        result
            .phases
            .insert("seedDelivery".into(), deliver(&existing_prefix, false)?);
        mac::sleep(0.3);
        cleanup.seeded_existing_text = true;
    }
    let before = mac::read_focused_ax();
    if options.existing_text {
        result.phases.insert(
            "existingSeedObservedExternally".into(),
            json!(before.contains(existing_prefix.trim())),
        );
    }
    let delivery = deliver(&sentinel, options.send)?;
    result.phases.insert("delivery".into(), delivery.clone());
    // Send QA submits the sentinel into a real chat composer. Immediately press in-chat Escape
    // (key code 53) IN THE SAME APP to halt any generation the sentinel started, then prove
    // generation stopped. NEVER quit/close/clear the app, and never Cmd+Z a sent case.
    if options.send && entry.escape_after_send {
        mac::sleep(0.4);
        mac::key_code(53)?;
        mac::sleep(0.8);
        let first = mac::read_focused_ax();
        mac::sleep(1.2);
        let second = mac::read_focused_ax();
        mac::key_code(53)?;
        result.phases.insert(
            "generationStopped".into(),
            json!({
                "escapePressed": true,
                "stableAfterEscape": first == second,
                "note": if first == second {
                    "composer AX text stable across 1.2s after Escape"
                } else {
                    "AX text still changing after Escape: inspect manually"
                },
            }),
        );
    }
    let outcome = json_at(&delivery, &["delivery", "outcome"]).clone();
    let outcome_kind = outcome["kind"].as_str().map(str::to_string);
    cleanup.second_delivery_pasted = outcome_kind.as_deref() == Some("pasted");
    // Read-back is AX-only (System Events `AXValue`): non-invasive. There is no Cmd+A + Cmd+C
    // selection probe: select-all against the operator's editor or composer is forbidden.
    let after_paste = mac::read_focused_ax();
    let observed = after_paste.contains(&sentinel);
    result.phases.insert(
        "paste".into(),
        json!({ "before": before, "after": after_paste, "observed": observed, "outcome": outcome }),
    );
    let mut send_proven = false;
    if options.send {
        let steps = delivery["delivery"]["delivery_timing"]["steps"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        send_proven = steps.iter().any(|step| {
            step["name"]
                .as_str()
                .is_some_and(|name| name.contains("submit"))
        });
        result.phases.insert(
            "send".into(),
            json!({ "attempted": true, "proven": send_proven }),
        );
        if outcome_kind.as_deref() == Some("pasted") && !send_proven {
            return Err("delivery returned pasted without a submit proof step".into());
        }
    } else {
        result
            .phases
            .insert("send".into(), json!({ "attempted": false }));
    }
    // The prior-clipboard restore runs on a background thread after Pasted (quiet window
    // <= 1 s), so let it finish before reading the pasteboard back.
    std::thread::sleep(std::time::Duration::from_millis(1_300));
    let clipboard_after = mac::read_clipboard();
    result.phases.insert(
        "clipboard".into(),
        json!({
            "after": clipboard_after,
            "restored": false,
            "fallbackAvailable": clipboard_after == sentinel,
        }),
    );
    assert_promised_pasteboard_evidence(&delivery, result, original_clipboard, &clipboard_after)?;
    if entry.secure_field {
        if outcome_kind.as_deref() != Some("copied_fallback") || clipboard_after != sentinel {
            return Err("secure field did not fail closed to clipboard fallback".into());
        }
    } else if outcome_kind.as_deref() == Some("pasted") {
        if !options.send && !observed {
            return Err("backend reported pasted without observable AX insertion".into());
        }
        if options.send && !send_proven {
            return Err("backend reported sent without ordered submit proof".into());
        }
    } else if outcome_kind.as_deref() == Some("copied_fallback") {
        return Err(
            "target did not exercise an observable paste; clipboard fallback is not a pass for this case"
                .into(),
        );
    } else if outcome_kind.is_none() {
        return Err("QA delivery command returned no delivery outcome".into());
    }
    result.status = "pass";
    Ok(())
}

/// Restores the operator's clipboard and undoes the sentinel left in a real composer, from `Drop`
/// when the case body or the host closure panics, or through [`CleanupGuard::finish`] otherwise.
#[cfg(target_os = "macos")]
struct CleanupGuard {
    entry: &'static TargetCase,
    send: bool,
    original: String,
    cleanup: Cleanup,
    restored: Option<bool>,
}

#[cfg(target_os = "macos")]
impl CleanupGuard {
    /// Run the cleanup once and return whether the clipboard came back.
    fn finish(&mut self) -> bool {
        if let Some(restored) = self.restored {
            return restored;
        }
        // Paste-only left the sentinel in a real composer. Undo EXACTLY that paste with a single
        // Cmd+Z, and ONLY after the paste was actually observed (never select-all-delete, never quit
        // or close the app, never after a sent case).
        if !self.send && (self.entry.undo_paste_on_cleanup || self.entry.surface.is_some()) {
            if self.cleanup.second_delivery_pasted {
                let _ = mac::key("z", Some("command"));
            }
            if self.cleanup.seeded_existing_text {
                let _ = mac::key("z", Some("command"));
            }
        }
        if !self.send && self.entry.interrupt_on_cleanup {
            let _ = mac::key("c", Some("control"));
        }
        let _ = mac::copy_clipboard(&self.original);
        let restored = mac::read_clipboard() == self.original;
        self.restored = Some(restored);
        restored
    }
}

#[cfg(target_os = "macos")]
impl Drop for CleanupGuard {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Run one case: remember the clipboard, run [`run_case_body`], undo exactly the paste a paste-only run
/// left in a real composer with a single Cmd+Z (and only after the paste was observed; never
/// select-all-delete, never quit or close the app, never after a sent case), then put the clipboard
/// back and fail the case when it cannot be restored. The ordering is part of the contract: the
/// cleanup and the restore run from a `Drop` guard created right after the clipboard is read, so they
/// run on pass, fail and panic (in the body or in the host closure).
#[cfg(target_os = "macos")]
pub fn run_case(
    deliver: Deliver,
    config: &DeliveryConfig,
    entry: &'static TargetCase,
    options: &DeliveryOptions,
) -> CaseResult {
    let mut guard = CleanupGuard {
        entry,
        send: options.send,
        original: mac::read_clipboard(),
        cleanup: Cleanup::default(),
        restored: None,
    };
    let mut result = CaseResult::new(entry);
    let original = guard.original.clone();
    if let Err(error) = run_case_body(
        deliver,
        config,
        entry,
        options,
        &mut result,
        &mut guard.cleanup,
        &original,
    ) {
        result.error = Some(error);
        result.status = "fail";
    }
    let restored = guard.finish();
    result
        .phases
        .insert("clipboard".into(), json!({ "restored": restored }));
    if !restored {
        result.status = "fail";
    }
    result
}
