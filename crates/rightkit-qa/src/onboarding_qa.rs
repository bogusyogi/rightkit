//! Journey helpers for the RightKit onboarding frame on a live app: the DOM probes of a step and of the
//! model step, the per-step comparer (`assert_step`), step and rail waits, parked-setup settings,
//! resume-at-step rewriting, a model-install guard, a confined local model copy and an engine
//! readiness check.
//!
//! The probes read the frame's own markup (`[data-testid=onboarding]`, `.rk-onboarding__*`). Everything
//! the app owns comes in as arguments: step ids, expected strings with the source each came from,
//! the progress label, the forbidden strings, the command names and the model id. Expected strings are
//! the host's copy; nothing here knows a product's wording.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use crate::harness::Scenario;
use crate::live_app::LiveApp;
use crate::proof::Proof;

/// Everything a journey reads from the onboarding frame for one step: `{stepId, description,
/// eyebrowDisplay, railDisplay, mainColumns, mainMaxWidth, heading, content, hint, footer:
/// [{text, disabled, primary}], mainText, bodyInner}`, or `null` when the frame is not on screen.
pub const STEP_PROBE: &str = r#"
const main = document.querySelector('[data-testid=onboarding]');
if (!main) return null;
const t = (e) => (e ? e.textContent.replace(/\s+/g, ' ').trim() : null);
const cs = (e, p) => (e ? getComputedStyle(e)[p] : null);
const content = main.querySelector('.rk-onboarding__content');
const heading = content ? content.querySelector('h1, h2') : null;
return {
  stepId: main.dataset.stepId,
  description: t(main.querySelector('.rk-onboarding__description')),
  eyebrowDisplay: cs(main.querySelector('.rk-onboarding__eyebrow'), 'display'),
  railDisplay: cs(main.querySelector('.rk-onboarding__rail'), 'display'),
  mainColumns: getComputedStyle(main).gridTemplateColumns,
  mainMaxWidth: cs(main.querySelector('.rk-onboarding__main'), 'maxWidth'),
  heading: t(heading),
  content: t(content),
  hint: t(main.querySelector('footer .rk-onboarding__hint')) || '',
  footer: [...main.querySelectorAll('footer button')].map((b) => ({ text: t(b), disabled: b.disabled, primary: b.classList.contains('rk-btn--primary') })),
  mainText: t(main),
  bodyInner: document.body.innerText,
};
"#;

/// JS for the model step's own elements, for the model row `data-model-id="<model_id>"` and the
/// highlights section labelled `reel_label`: `{rowName, rowDesc, rowActions, rowButtons, progressbars,
/// reel}`, or `null` when the step is not mounted.
pub fn model_probe_js(model_id: &str, reel_label: &str) -> String {
    let model = Value::String(model_id.into());
    let reel = Value::String(format!("section[aria-label=\"{reel_label}\"]"));
    format!(
        r#"
const m = document.querySelector('[data-testid=onboarding-models]');
if (!m) return null;
const t = (e) => (e ? e.textContent.replace(/\s+/g, ' ').trim() : null);
const row = m.querySelector('[data-model-id=' + JSON.stringify({model}) + ']');
return {{
  rowName: row ? t(row.querySelector('strong')) : null,
  rowDesc: row ? t(row.querySelector('p')) : null,
  rowActions: row ? t(row.querySelector('.rk-onboarding__row-actions')) : null,
  rowButtons: row ? [...row.querySelectorAll('button')].map(t) : null,
  progressbars: m.querySelectorAll('[role=progressbar]').length,
  reel: !!document.querySelector({reel}),
}};
"#
    )
}

/// What one step must show. Strings are the host's copy; each `*_src` names where the source
/// renders it, and appears in the failure message and the evidence note.
pub struct StepWant<'a> {
    /// 1-based step number.
    pub n: usize,
    /// The step id the frame reports.
    pub id: &'a str,
    /// Where the id is defined.
    pub id_src: &'a str,
    /// The acceptable headings (a step whose heading depends on the plan lists both).
    pub heading: &'a [&'a str],
    /// Where the heading is defined.
    pub heading_src: &'a str,
    /// `(string the source renders, its source)`, each required in the step's content.
    pub body: &'a [(&'a str, &'a str)],
    /// The primary footer button's label.
    pub primary: &'a str,
    /// `Some(state)` asserts the primary button's enabled state; `None` reads it and notes it.
    pub primary_enabled: Option<bool>,
    /// `(hint, source)` the frame shows beside a disabled primary button, when this step has one.
    pub blocked_hint: Option<(&'a str, &'a str)>,
    /// Whether Back is disabled (the first step).
    pub back_disabled: bool,
}

/// What every step must satisfy, as the host's copy and sources. `StepRules::new` gives neutral
/// defaults (no progress label, no eyebrow or forbidden-text checks, empty sources).
pub struct StepRules<'a> {
    /// The progress description a step n shows (for example `Step 3 of 8`); `None` skips the check.
    pub progress: Option<&'a dyn Fn(usize) -> String>,
    /// Where the progress label is defined.
    pub progress_src: &'a str,
    /// `(computed display the frame eyebrow must have, source)`; `None` skips the check.
    pub eyebrow_display: Option<(&'a str, &'a str)>,
    /// `(text, source)` that must not appear anywhere in the page's visible text.
    pub forbid_page: &'a [(&'a str, &'a str)],
    /// `(text, source)` that must not appear in the onboarding frame's text.
    pub forbid_frame: &'a [(&'a str, &'a str)],
    /// The label of the Back button (default `Back`).
    pub back_label: &'a str,
    /// Where the footer's button set (Back and the primary) is defined.
    pub footer_src: &'a str,
    /// Where the primary button's `rk-btn--primary` class is defined.
    pub primary_class_src: &'a str,
    /// Where Back's disabled rule is defined.
    pub back_src: &'a str,
    /// Where the primary's enabled rule is defined.
    pub primary_state_src: &'a str,
    /// Where "no hint without a blocked primary" is defined.
    pub hint_src: &'a str,
    /// Where an undefined blocked hint is defined (a step with no `blocked_hint`).
    pub hint_undefined_src: &'a str,
    /// Why a read-but-not-asserted primary state depends on the machine (appears in the note).
    pub read_only_note: &'a str,
}

impl<'a> StepRules<'a> {
    /// Neutral defaults: nothing beyond the footer, hint and heading checks.
    pub fn new() -> Self {
        StepRules {
            progress: None,
            progress_src: "",
            eyebrow_display: None,
            forbid_page: &[],
            forbid_frame: &[],
            back_label: "Back",
            footer_src: "",
            primary_class_src: "",
            back_src: "",
            primary_state_src: "",
            hint_src: "",
            hint_undefined_src: "",
            read_only_note: "not asserted either way",
        }
    }
}

impl Default for StepRules<'_> {
    fn default() -> Self {
        Self::new()
    }
}

/// Read the step the frame shows and compare it with `want` under `rules`: id, progress label, eyebrow,
/// forbidden strings, heading, body strings, the footer's buttons (exactly Back and the primary), the
/// primary class, Back's and the primary's enabled state, and the blocked-Continue hint (present
/// exactly while the primary is disabled). Every held assertion is a `Proof` note. Returns the probe
/// reading (`STEP_PROBE`).
pub fn assert_step(
    p: &mut Proof,
    app: &LiveApp,
    sc: &Scenario,
    want: &StepWant,
    rules: &StepRules,
) -> Value {
    let s = app.js(STEP_PROBE);
    assert!(
        !s.is_null(),
        "step {}: the onboarding shell ([data-testid=onboarding]) is not on screen",
        want.n
    );
    let get = |k: &str| s[k].as_str().unwrap_or("").to_string();
    let tag = format!("step {} ({})", want.n, want.id);

    p.eq(&format!("{tag} id"), &get("stepId"), want.id, want.id_src);
    if let Some(progress) = rules.progress {
        p.eq(
            &format!("{tag} progress label"),
            &get("description"),
            &progress(want.n),
            rules.progress_src,
        );
    }
    if let Some((display, src)) = rules.eyebrow_display {
        p.eq(
            &format!("{tag} frame eyebrow display"),
            &get("eyebrowDisplay"),
            display,
            src,
        );
    }
    for (text, src) in rules.forbid_page {
        p.lacks(
            &format!("{tag} no {text:?} anywhere visible"),
            &get("bodyInner"),
            text,
            src,
        );
    }
    for (text, src) in rules.forbid_frame {
        p.lacks(&format!("{tag} no {text:?}"), &get("mainText"), text, src);
    }
    let heading = get("heading");
    p.yes(
        &format!("{tag} heading is one of {:?}", want.heading),
        want.heading.contains(&heading.as_str()),
        &heading,
        want.heading_src,
    );
    for (needle, src) in want.body {
        p.has(&format!("{tag} body"), &get("content"), needle, src);
    }

    let footer = s["footer"].as_array().expect("footer buttons");
    let labels: Vec<String> = footer
        .iter()
        .map(|b| b["text"].as_str().unwrap_or("").to_string())
        .collect();
    p.eq(
        &format!("{tag} footer buttons"),
        &labels.join(" | "),
        &format!("{} | {}", rules.back_label, want.primary),
        rules.footer_src,
    );
    p.yes(
        &format!("{tag} last footer button is the primary (.rk-btn--primary)"),
        footer.last().is_some_and(|b| b["primary"] == json!(true)),
        &s["footer"].to_string(),
        rules.primary_class_src,
    );
    p.yes(
        &format!("{tag} Back disabled = {}", want.back_disabled),
        footer[0]["disabled"] == json!(want.back_disabled),
        &s["footer"].to_string(),
        rules.back_src,
    );
    let primary_disabled = footer
        .last()
        .map(|b| b["disabled"] == json!(true))
        .unwrap_or(true);
    match want.primary_enabled {
        Some(enabled) => p.yes(
            &format!("{tag} primary '{}' enabled = {enabled}", want.primary),
            primary_disabled != enabled,
            &s["footer"].to_string(),
            rules.primary_state_src,
        ),
        None => sc.note(format!(
            "READ {tag}: primary '{}' disabled = {primary_disabled} ({})",
            want.primary, rules.read_only_note
        )),
    }
    let hint = get("hint");
    match (primary_disabled, want.blocked_hint) {
        (true, Some((expected, src))) => p.eq(
            &format!("{tag} hint while Continue is blocked"),
            &hint,
            expected,
            src,
        ),
        (true, None) => p.eq(
            &format!("{tag} hint while Continue is blocked (this step defines none)"),
            &hint,
            "",
            rules.hint_undefined_src,
        ),
        (false, _) => p.eq(
            &format!("{tag} no hint once Continue is enabled"),
            &hint,
            "",
            rules.hint_src,
        ),
    }
    s
}

/// Wait until the frame reports step `id`, then until every finite animation has finished.
pub fn wait_step(app: &LiveApp, id: &str, within: Duration) {
    app.wait_js(
        &format!(
            "return document.querySelector('[data-testid=onboarding]')?.dataset.stepId === {}",
            Value::String(id.into())
        ),
        within,
    );
    app.wait_settled(Duration::from_secs(10));
}

/// Click rail entry `index` (unless it is already the current step: the rail persists every click, and
/// two persists in flight resolve in either order, so a click on the current step could land the walk
/// back on it), then wait for step `id`.
pub fn rail_to(app: &LiveApp, index: usize, id: &str, within: Duration) {
    app.js(&format!(
        "const b = document.querySelectorAll('.rk-onboarding__rail nav button')[{index}]; if (b && b.getAttribute('aria-current') !== 'step') b.click(); return true;"
    ));
    wait_step(app, id, within);
}

/// Wait until rail entry `index` is the current step (`aria-current=step`).
pub fn wait_rail_current(app: &LiveApp, index: usize, within: Duration) {
    app.wait_js(
        &format!("const b = document.querySelectorAll('.rk-onboarding__rail nav button')[{index}]; return !!b && b.getAttribute('aria-current') === 'step';"),
        within,
    );
}

/// The rail entry `index`'s label as a slug (the leading counter word dropped, lower-case, runs of
/// non-letters as `-`), for evidence file names.
pub fn rail_step_slug(app: &LiveApp, index: usize) -> String {
    app.js(&format!("return (document.querySelectorAll('.rk-onboarding__rail nav button')[{index}].innerText || '').replace(/\\s+/g, ' ').trim().replace(/^\\S+\\s/, '').toLowerCase().replace(/[^a-z]+/g, '-');"))
        .as_str()
        .unwrap_or("step")
        .to_string()
}

/// Settings that park setup on step `current_step` with the legal receipt in place: `base` (the
/// seeded settings JSON) with `onboarding_complete` false and `onboarding.state` set to
/// `{currentStep, completed: [], eligibilityBasis}`. The rail always allows going back, so every
/// earlier step is one rail click away.
pub fn park_onboarding_settings(
    base: &str,
    current_step: usize,
    eligibility_basis: &str,
) -> String {
    let mut s: Value = serde_json::from_str(base).expect("seed settings json");
    s["onboarding_complete"] = json!(false);
    s["onboarding.state"] = json!({
        "currentStep": current_step,
        "completed": [],
        "eligibilityBasis": eligibility_basis,
    });
    s.to_string()
}

/// Refuse to go on unless the app says a model is installed: a step that auto-starts a download would
/// otherwise start it. Calls `command` with `args` through the webview, notes the reply, and panics
/// with `refusal` (a `NOT PROVEN` line naming what would be downloaded) when `installed` is not true.
pub fn require_model_installed(
    app: &LiveApp,
    sc: &Scenario,
    command: &str,
    args: Value,
    refusal: &str,
) {
    let status = app
        .invoke(command, args.clone())
        .unwrap_or_else(|e| panic!("{command} failed: {e}"));
    sc.note(format!("{command}({args}) = {status}"));
    if status["installed"] != json!(true) {
        panic!("NOT PROVEN: the app reports the model as not installed ({status}). {refusal}");
    }
}

/// Read and parse a JSON file; panics naming the path.
pub fn read_json(path: &Path) -> Value {
    serde_json::from_slice(
        &std::fs::read(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
    )
    .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
}

/// The first of `candidates` that is a directory holding every file in `required`: a local model copy
/// to seed instead of downloading.
pub fn first_complete_dir(candidates: &[Option<PathBuf>], required: &[&str]) -> Option<PathBuf> {
    candidates
        .iter()
        .flatten()
        .find(|dir| required.iter().all(|f| dir.join(f).is_file()))
        .cloned()
}

/// Copy `source` to the absent `store` inside the shared models folder (`store`'s parent, which is
/// created) with a confined copy: it refuses overlapping aliases, links and any existing destination.
/// The folder must be covered by the run's snapshot.
pub fn seed_model_copy(source: &Path, store: &Path) {
    let models = store.parent().expect("shared models folder");
    std::fs::create_dir_all(models).expect("create snapshotted models folder");
    rightkit_fs::copy_tree_confined(
        source,
        store,
        &rightkit_fs::ConfinedCopy::new(source, models),
    )
    .expect("seed the absent model store");
}

/// Persist onboarding progress as a user who finished `completed` steps and stands at rail index
/// `current_step`: rewrites `onboarding.state` of the settings file in `app_data`. The app must be
/// stopped, because it rewrites its settings.
pub fn resume_onboarding_at(
    app_data: &Path,
    settings_file: &str,
    current_step: usize,
    completed: &[&str],
) {
    let path = app_data.join(settings_file);
    let mut settings = read_json(&path);
    let state = settings
        .get_mut("onboarding.state")
        .and_then(Value::as_object_mut)
        .expect("the walk persisted onboarding.state");
    state.insert("currentStep".into(), json!(current_step));
    state.insert("completed".into(), json!(completed));
    std::fs::write(&path, serde_json::to_vec(&settings).expect("settings json"))
        .expect("write settings file");
}

/// The resident engine says hello: `prewarm_command` (waited on for `engine_wait`, a cold load plus the
/// handshake) must succeed, then `info_command` must report a non-empty `mode`. Notes the reply.
pub fn assert_engine_ready(
    sc: &Scenario,
    app: &LiveApp,
    prewarm_command: &str,
    info_command: &str,
    engine_wait: Duration,
    info_wait: Duration,
) {
    app.invoke_within(prewarm_command, json!({}), engine_wait)
        .unwrap_or_else(|error| panic!("the resident engine did not reach hello: {error}"));
    let info = app
        .invoke_within(info_command, json!({}), info_wait)
        .expect("engine info");
    let mode = info["mode"].as_str().unwrap_or("");
    assert!(!mode.is_empty(), "the engine reported its mode: {info}");
    sc.note(format!("resident engine ready: {info}"));
}
