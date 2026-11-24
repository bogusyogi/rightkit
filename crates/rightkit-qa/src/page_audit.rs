//! Page-audit probes and helpers for visual journeys on a live app: a per-screen layout measurement,
//! scroll helpers, sidebar-footer and tooltip geometry probes, hover and pointer parking, a theme
//! switch through the command palette, and the settled screenshot and JSON log writers.
//!
//! Everything product-specific comes in as an argument: selectors, copy, the viewport, the palette
//! labels. The probes are JavaScript bodies that `return` a value, run with [`crate::live_app::LiveApp::js`]
//! or waited on with [`crate::live_app::LiveApp::wait_js`].

use std::time::Duration;

use serde_json::{json, Value};

use crate::harness::Scenario;
use crate::live_app::{LiveApp, SETTLED_JS};

/// The measurement taken on every screen: controls that cannot be reached (outside the window with no
/// scroll container), controls clipped by an overflow-hidden parent, scroll containers and how far
/// they overflow, truncated text, and how far the content runs past the window. Returns
/// `{viewport, documentOverflow, contentBottom, contentPastWindow, unreachable, clipped, scrollers, truncated}`.
pub const MEASURE_JS: &str = r#"
const vw = innerWidth, vh = innerHeight;
const visible = (e) => { const s = getComputedStyle(e); if (s.display === 'none' || s.visibility === 'hidden' || Number(s.opacity) === 0) return false; const r = e.getBoundingClientRect(); return r.width > 0 && r.height > 0; };
const label = (e) => ((e.innerText || e.getAttribute('aria-label') || e.getAttribute('title') || e.value || e.tagName) + '').trim().replace(/\s+/g, ' ').slice(0, 48);
const scroller = (e) => { for (let p = e.parentElement; p; p = p.parentElement) { const s = getComputedStyle(p); if (/(auto|scroll)/.test(s.overflowY) && p.scrollHeight > p.clientHeight + 1) return p; } return null; };
const controls = [...document.querySelectorAll('button, a[href], input, select, textarea, [role=button], [role=tab], [role=switch], [role=checkbox], [role=radio], [role=menuitem]')].filter(visible);
const unreachable = [], clipped = [];
for (const e of controls) {
  const r = e.getBoundingClientRect();
  if ((r.bottom > vh + 1 || r.right > vw + 1 || r.top < -1 || r.left < -1) && !scroller(e)) unreachable.push({ label: label(e), rect: [r.x, r.y, r.width, r.height].map(Math.round) });
  for (let p = e.parentElement; p && p !== document.body; p = p.parentElement) {
    const s = getComputedStyle(p);
    if (/(auto|scroll)/.test(s.overflowY)) break;
    if (/(hidden|clip)/.test(s.overflowY) || /(hidden|clip)/.test(s.overflowX)) {
      const pr = p.getBoundingClientRect();
      if (r.bottom > pr.bottom + 1 || r.top < pr.top - 1 || r.right > pr.right + 1 || r.left < pr.left - 1) clipped.push({ label: label(e), by: (p.className + '').slice(0, 60) });
      break;
    }
  }
}
const scrollers = [...document.querySelectorAll('*')].filter((e) => { const s = getComputedStyle(e); return /(auto|scroll)/.test(s.overflowY) && e.scrollHeight > e.clientHeight + 1; })
  .map((e) => ({ el: (e.className + '' || e.tagName).slice(0, 60), visible: e.clientHeight, content: e.scrollHeight, overflow: e.scrollHeight - e.clientHeight }));
const truncated = [...document.querySelectorAll('body *')].filter((e) => visible(e) && e.children.length === 0 && (e.innerText || '').trim() && e.scrollWidth > e.clientWidth + 1 && getComputedStyle(e).overflowX !== 'visible')
  .map((e) => label(e)).slice(0, 12);
let contentBottom = 0;
for (const e of document.querySelectorAll('body *')) { if (!visible(e) || e.children.length) continue; const r = e.getBoundingClientRect(); const sc = scroller(e); const b = sc ? r.bottom - sc.getBoundingClientRect().top + sc.scrollTop + sc.getBoundingClientRect().top : r.bottom; contentBottom = Math.max(contentBottom, b); }
return { viewport: [vw, vh], documentOverflow: document.documentElement.scrollHeight - vh, contentBottom: Math.round(contentBottom), contentPastWindow: Math.max(0, Math.round(contentBottom - vh)), unreachable, clipped, scrollers, truncated };
"#;

/// Scroll every scroll container to its end.
pub const SCROLL_TO_BOTTOM_JS: &str = "for (const e of document.querySelectorAll('*')) { const s = getComputedStyle(e); if (/(auto|scroll)/.test(s.overflowY) && e.scrollHeight > e.clientHeight + 1) e.scrollTop = e.scrollHeight; } return true;";

/// Scroll every scrolled element back to its start.
pub const SCROLL_TO_TOP_JS: &str =
    "for (const e of document.querySelectorAll('*')) { if (e.scrollTop) e.scrollTop = 0; } return true;";

/// `macos` or `windows`: the platform prefix of evidence file names.
pub fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else {
        "windows"
    }
}

/// The scenario requirement tag for this platform (`platform:darwin` or `platform:windows`).
pub fn platform_tier() -> &'static str {
    if cfg!(target_os = "macos") {
        "platform:darwin"
    } else {
        "platform:windows"
    }
}

/// The command-palette modifier key name for this platform (`cmd` or `ctrl`).
pub fn mod_key() -> &'static str {
    if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    }
}

/// A JavaScript string literal for `s`.
pub fn js_str(s: &str) -> String {
    Value::String(s.to_string()).to_string()
}

/// Lower-case ASCII slug: runs of non-alphanumerics become one `-`.
pub fn slugify(text: &str) -> String {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_lowercase())
        .collect::<Vec<_>>()
        .join("-")
}

/// JS that returns the centre of the first element matching `selector`, in viewport CSS px
/// (`{x, y}`, or `null` when there is none).
pub fn point_js(selector: &str) -> String {
    format!(
        "const e = document.querySelector({}); if (!e) return null; const r = e.getBoundingClientRect(); return {{ x: r.x + r.width / 2, y: r.y + r.height / 2 }};",
        js_str(selector)
    )
}

/// Real pointer move onto a `{x, y}` point read from the page, then time for a hover tooltip delay
/// (`settle_ms`, 450 in the journeys it was written for) and the hover transition. Panics naming
/// `what` when `point` has no box.
pub fn point_to(app: &LiveApp, point: Value, what: &str, settle_ms: u64) {
    let x = point["x"]
        .as_f64()
        .unwrap_or_else(|| panic!("{what}: no box to hover"));
    let y = point["y"]
        .as_f64()
        .unwrap_or_else(|| panic!("{what}: no box to hover"));
    app.ctl.move_to(x, y).expect("pointer move");
    std::thread::sleep(Duration::from_millis(settle_ms));
}

/// Move the pointer to the window's bottom-right corner (inside a `width` by `height` viewport),
/// away from every control, so no hover state is left behind.
pub fn park(app: &LiveApp, width: u32, height: u32) {
    app.ctl
        .move_to(f64::from(width) - 4.0, f64::from(height) - 4.0)
        .expect("pointer park");
    std::thread::sleep(Duration::from_millis(250));
}

/// Whether an enabled `button` whose trimmed text starts with `text` is on the page.
pub fn has_button(app: &LiveApp, text: &str) -> bool {
    app.js(&format!(
        "return [...document.querySelectorAll('button')].some((b) => !b.disabled && b.textContent.trim().startsWith({}));",
        js_str(text)
    )) == json!(true)
}

/// Switch the theme through the command palette: open it with the platform modifier plus `k`, type
/// `<command_prefix><label>` (for example `Appearance: Dark`), wait for the command
/// `[data-command="theme:<choice>"]` to be selected, press Enter, then wait in three stages so a
/// timeout names the one that did not happen: the palette closes, the root carries the chosen theme
/// (`documentElement.dataset.theme`), and no finite animation is still running. `choice` is the theme
/// id (`dark`, `light`) and `label` its palette label (`Dark`, `Light`).
pub fn set_theme_via_palette(
    app: &LiveApp,
    choice: &str,
    command_prefix: &str,
    label: &str,
    wait: Duration,
) {
    app.ctl
        .key(&format!("{}+k", mod_key()))
        .expect("open command palette");
    app.wait_js(
        "return !!document.querySelector('.rk-palette input');",
        wait,
    );
    app.ctl
        .type_text(&format!("{command_prefix}{label}"))
        .expect("type theme command");
    app.wait_js(
        &format!("return !!document.querySelector('[data-command=\"theme:{choice}\"][aria-selected=true]');"),
        wait,
    );
    app.ctl.key("enter").expect("choose theme");
    app.wait_js("return !document.querySelector('.rk-palette');", wait);
    app.wait_js(
        &format!("return document.documentElement.dataset.theme === '{choice}';"),
        wait,
    );
    app.wait_settled(wait);
}

/// Every finite animation has finished, then one full-window PNG under `<os>-<name>.png`, kept in the
/// scenario evidence. Returns the file path.
pub fn capture_settled(sc: &Scenario, app: &LiveApp, name: &str) -> std::path::PathBuf {
    app.wait_js_finishing(SETTLED_JS, Duration::from_secs(15));
    let path = sc.dir().join(format!("{}-{name}.png", os_name()));
    app.ctl
        .screenshot_to(&path)
        .expect("save scenario screenshot");
    sc.keep(name, &path);
    path
}

/// Write `<os>-<stem>-<tag>.json` into the scenario folder:
/// `{os, viewport: [width, height], entries}`.
pub fn write_state_log(
    sc: &Scenario,
    stem: &str,
    tag: &str,
    viewport: (u32, u32),
    entries: &[Value],
) {
    std::fs::write(
        sc.dir().join(format!("{}-{stem}-{tag}.json", os_name())),
        serde_json::to_string_pretty(
            &json!({ "os": os_name(), "viewport": [viewport.0, viewport.1], "entries": entries }),
        )
        .unwrap_or_default(),
    )
    .expect("write state log");
}

/// The settings JSON `base` with `edit` applied.
pub fn settings_with(base: &str, edit: impl FnOnce(&mut Value)) -> String {
    let mut settings: Value = serde_json::from_str(base).expect("seed settings json");
    edit(&mut settings);
    settings.to_string()
}

/// Layout measurement of every screen a journey captures, with one PNG per screen and one JSON
/// report: `<os>-screen-audit-<part>.json` holding `{os, screens: [{case, file, measure}]}`.
pub struct ScreenAudit {
    /// Platform prefix of evidence names ([`os_name`]).
    pub os: &'static str,
    /// One row per captured screen.
    pub rows: Vec<Value>,
}

impl ScreenAudit {
    /// An empty audit for this platform.
    pub fn new() -> Self {
        ScreenAudit {
            os: os_name(),
            rows: Vec::new(),
        }
    }

    /// Wait until the page has settled, measure it ([`MEASURE_JS`]), save a full-window PNG under
    /// `<os>-<name>.png` and keep it in the scenario evidence.
    pub fn capture(&mut self, sc: &Scenario, app: &LiveApp, name: &str) {
        app.wait_js_finishing(SETTLED_JS, Duration::from_secs(15));
        let measure = app.js(MEASURE_JS);
        let path = sc.dir().join(format!("{}-{name}.png", self.os));
        app.ctl
            .screenshot_to(&path)
            .expect("save scenario screenshot");
        sc.keep(name, &path);
        self.rows.push(json!({ "case": name, "file": path.file_name().map(|n| n.to_string_lossy().into_owned()), "measure": measure }));
    }

    /// Write the report for `part` into the scenario folder.
    pub fn write(&self, sc: &Scenario, part: &str) {
        std::fs::write(
            sc.dir()
                .join(format!("{}-screen-audit-{part}.json", self.os)),
            serde_json::to_string_pretty(&json!({ "os": self.os, "screens": self.rows }))
                .unwrap_or_default(),
        )
        .expect("write audit");
    }
}

impl Default for ScreenAudit {
    fn default() -> Self {
        Self::new()
    }
}

/// JS body for a sidebar footer's geometry. `status_selector` matches the visible element the footer
/// is found from (its parent is the footer); `state_js` is statements that fill the object `state`
/// from `rows` (`[{tip, aria, rect}]`, one per footer button with a `data-tooltip`) and may read
/// `status`. Returns `{state, rows, overflow, overlaps, outside, footer, viewport}`: truncated text,
/// overlapping controls and controls outside the window are the lists a journey asserts empty
/// ([`assert_footer_clean`]).
pub fn footer_probe_js(status_selector: &str, state_js: &str) -> String {
    format!(
        r#"
const status = [...document.querySelectorAll({sel})].find((e) => e.offsetParent !== null);
if (!status) return null;
const footer = status.parentElement;
const box = (e) => {{ const r = e.getBoundingClientRect(); return [r.left, r.top, r.right, r.bottom].map(Math.round); }};
const rows = [...footer.querySelectorAll('button[data-tooltip]:not([data-tooltip=""])')].map((b) => ({{ tip: b.dataset.tooltip, aria: b.getAttribute('aria-label') || '', rect: box(b) }}));
const state = {{}};
{state_js}
const version = status.querySelector('span.truncate');
const items = [...footer.querySelectorAll('button')].filter((b) => !b.closest('.sr-only') && b.getBoundingClientRect().width > 0)
  .map((b) => ({{ label: (b.getAttribute('aria-label') || b.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 40), el: b }}));
if (version) items.push({{ label: 'version', el: version }});
const overflow = [...footer.querySelectorAll('*')].filter((e) => e.children.length === 0 && !e.closest('.sr-only') && (e.textContent || '').trim() && e.scrollWidth > e.clientWidth + 1)
  .map((e) => e.textContent.trim().slice(0, 40));
const overlaps = [];
for (let i = 0; i < items.length; i++) for (let j = i + 1; j < items.length; j++) {{
  const a = items[i].el, b = items[j].el;
  if (a.contains(b) || b.contains(a)) continue;
  const ra = a.getBoundingClientRect(), rb = b.getBoundingClientRect();
  if (Math.min(ra.right, rb.right) - Math.max(ra.left, rb.left) > 1 && Math.min(ra.bottom, rb.bottom) - Math.max(ra.top, rb.top) > 1) overlaps.push([items[i].label, items[j].label]);
}}
const outside = items.filter((it) => {{ const r = it.el.getBoundingClientRect(); return r.left < -1 || r.top < -1 || r.right > innerWidth + 1 || r.bottom > innerHeight + 1; }}).map((it) => it.label);
return {{ state, rows, overflow, overlaps, outside, footer: box(footer), viewport: [innerWidth, innerHeight] }};
"#,
        sel = js_str(status_selector),
        state_js = state_js
    )
}

/// The hover tooltip element (`[role=tooltip]`), once it is placed and visible. Returns
/// `{text, rect, inside, truncated}` or `null`.
pub const TOOLTIP_JS: &str = r#"
const t = document.querySelector('[role=tooltip]');
if (!t || getComputedStyle(t).visibility !== 'visible') return null;
const r = t.getBoundingClientRect();
return { text: (t.textContent || '').trim(), rect: [r.left, r.top, r.right, r.bottom].map(Math.round), inside: r.left >= 0 && r.top >= 0 && r.right <= innerWidth && r.bottom <= innerHeight, truncated: t.scrollWidth > t.clientWidth + 1 };
"#;

/// Rendered footer controls the tooltip overlaps; an empty list when the tooltip clears every control
/// in the footer found from `status_selector`.
pub fn tooltip_clear_js(status_selector: &str) -> String {
    format!(
        r#"
const t = document.querySelector('[role=tooltip]');
if (!t) return null;
const f = [...document.querySelectorAll({sel})].find((e) => e.offsetParent !== null)?.parentElement;
if (!f) return null;
const a = t.getBoundingClientRect();
return [...f.querySelectorAll('button')].filter((b) => {{
  const r = b.getBoundingClientRect();
  return r.width > 0 && r.left < a.right && r.right > a.left && r.top < a.bottom && r.bottom > a.top;
}}).map((b) => (b.getAttribute('aria-label') || b.textContent || '').trim().replace(/\s+/g, ' ').slice(0, 40));
"#,
        sel = js_str(status_selector)
    )
}

/// JS that returns the centre of footer row `index` (a `button[data-tooltip]` under the footer found
/// from `status_selector`), in viewport CSS px.
pub fn hover_row_js(status_selector: &str, index: usize) -> String {
    format!(
        "const f = [...document.querySelectorAll({sel})].find((e) => e.offsetParent !== null).parentElement; const e = f.querySelectorAll('button[data-tooltip]:not([data-tooltip=\"\"])')[{index}]; if (!e) return null; const r = e.getBoundingClientRect(); return {{ x: r.x + r.width / 2, y: r.y + r.height / 2 }};",
        sel = js_str(status_selector)
    )
}

/// JS for the first enabled, visible button whose class list contains `class_fragment` (outside the
/// navigation `exclude_selector` and any tooltip): its label and centre, the hover target of a
/// primary button. Returns `{label, x, y}` or `null`.
pub fn primary_button_js(class_fragment: &str, exclude_selector: &str) -> String {
    format!(
        r#"
const b = [...document.querySelectorAll('button')].find((e) => !e.disabled && e.offsetParent !== null && e.className.includes({class}) && !e.closest({exclude}) && !e.closest('[role=tooltip]'));
if (!b) return null;
const r = b.getBoundingClientRect();
return {{ label: b.textContent.trim().replace(/\s+/g, ' ').slice(0, 40), x: r.x + r.width / 2, y: r.y + r.height / 2 }};
"#,
        class = js_str(class_fragment),
        exclude = js_str(exclude_selector)
    )
}

/// Panics unless `footer` (a [`footer_probe_js`] reading) is an object with no truncated text, no
/// overlapping controls and nothing outside the window; `theme` names the state in the message.
pub fn assert_footer_clean(footer: &Value, theme: &str) {
    assert!(
        footer.is_object(),
        "{theme}: the sidebar status footer rendered"
    );
    for key in ["overflow", "overlaps", "outside"] {
        let list = footer[key].as_array().cloned().unwrap_or_default();
        assert!(list.is_empty(), "{theme}: sidebar footer {key}: {list:?}");
    }
}

/// A state slug from a footer reading: `key-value` for each of `keys` the reading's `state` holds,
/// joined with `-`; `unknown` when none.
pub fn footer_slug(footer: &Value, keys: &[&str]) -> String {
    let state = &footer["state"];
    let parts: Vec<String> = keys
        .iter()
        .filter_map(|key| state[*key].as_str().map(|v| format!("{key}-{v}")))
        .collect();
    if parts.is_empty() {
        "unknown".to_string()
    } else {
        parts.join("-")
    }
}

/// Panics unless a [`TOOLTIP_JS`] reading shows `tip` fully inside the window and untruncated.
pub fn assert_tooltip_fits(tooltip: &Value, tip: &str, theme: &str) {
    assert_eq!(
        tooltip["text"].as_str(),
        Some(tip),
        "{theme}: the hovered row shows its own tooltip"
    );
    assert_eq!(
        tooltip["inside"],
        json!(true),
        "{theme}: tooltip inside the window: {tooltip}"
    );
    assert_eq!(
        tooltip["truncated"],
        json!(false),
        "{theme}: tooltip text fits: {tooltip}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_and_literals() {
        assert_eq!(slugify("Plan: Pro / AI polish!"), "plan-pro-ai-polish");
        assert_eq!(js_str("a\"b"), "\"a\\\"b\"");
        assert!(point_js("#x").contains("getBoundingClientRect"));
        assert!(footer_probe_js("[data-s]", "").contains("[data-s]"));
        assert!(hover_row_js("[data-s]", 2).contains("[2]"));
    }

    #[test]
    fn footer_slug_and_asserts() {
        let footer = json!({"state": {"plan": "free", "sync": "off"}, "overflow": [], "overlaps": [], "outside": []});
        assert_eq!(
            footer_slug(&footer, &["plan", "polish", "sync"]),
            "plan-free-sync-off"
        );
        assert_eq!(footer_slug(&json!({}), &["plan"]), "unknown");
        assert_footer_clean(&footer, "dark");
        assert_tooltip_fits(
            &json!({"text": "t", "inside": true, "truncated": false}),
            "t",
            "dark",
        );
    }

    #[test]
    fn settings_edit() {
        let out = settings_with("{\"a\":1}", |s| s["b"] = json!(2));
        assert_eq!(
            serde_json::from_str::<Value>(&out).unwrap(),
            json!({"a":1,"b":2})
        );
    }
}
