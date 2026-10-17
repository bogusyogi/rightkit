//! Page-side QA primitives that run through `Control::eval` (or any evaluator):
//! frame-scoped selectors, painted-not-acknowledged timing, and active-pane
//! geometry. These port the `frames`, `paint`, and `panes` modules of the TS
//! package; the scripts are plain function bodies (`return ...`).
use crate::control::Control;
use crate::util::{err, sleep_ms, Result};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

/// Anything that can evaluate a JS function body and return its JSON value.
pub trait Eval {
    fn eval_js(&self, js: &str) -> Result<Value>;
}
impl Eval for Control {
    fn eval_js(&self, js: &str) -> Result<Value> {
        self.eval(js)
    }
}

fn q(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// DOM role/name locator for QA eval bodies (#469). Prefix an eval body with this script to bind
/// `QA`, which offers `byRole(role, name?, exact?, scope?)`, `byText`, `name`, `roleOf`, `visible`
/// and `tag` (a stable `data-rightkit-qa` selector). A pragmatic DOM locator, not a complete ARIA
/// accessible-name computation.
pub const ACCESSIBLE_LOCATOR_SCRIPT: &str = include_str!("page/locator.js");

// ----------------------------------------------------------------- frames

/// Index path from the top document to a nested frame (`[]` is the top document).
pub type FramePath = Vec<usize>;

#[derive(Debug, Clone, PartialEq)]
pub struct FrameMatch {
    pub path: FramePath,
    pub count: usize,
}

pub fn describe_frame(path: &[usize]) -> String {
    if path.is_empty() {
        "top document".into()
    } else {
        format!(
            "frame {}",
            path.iter()
                .map(|i| i.to_string())
                .collect::<Vec<_>>()
                .join(".")
        )
    }
}

/// Script that reports every same-origin document (top and nested) matching `selector`.
pub fn find_across_frames_script(selector: &str, max_depth: usize) -> String {
    format!(
        "var out=[];function visit(w,path,d){{try{{var n=w.document.querySelectorAll({sel}).length;if(n>0)out.push({{path:path,count:n}});}}catch(e){{}}\
         if(d>={max_depth})return;var len=0;try{{len=w.frames.length}}catch(e){{}}\
         for(var i=0;i<len;i++){{var c;try{{c=w.frames[i]}}catch(e){{continue}}visit(c,path.concat([i]),d+1)}}}}\
         visit(window,[],0);return out;",
        sel = q(selector)
    )
}

pub fn parse_frame_matches(v: &Value) -> Vec<FrameMatch> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|m| FrameMatch {
                    path: m["path"]
                        .as_array()
                        .map(|p| {
                            p.iter()
                                .filter_map(|i| i.as_u64().map(|i| i as usize))
                                .collect()
                        })
                        .unwrap_or_default(),
                    count: m["count"].as_u64().unwrap_or(0) as usize,
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn find_across_frames(
    page: &dyn Eval,
    selector: &str,
    max_depth: usize,
) -> Result<Vec<FrameMatch>> {
    Ok(parse_frame_matches(&page.eval_js(
        &find_across_frames_script(selector, max_depth),
    )?))
}

/// The one document a selector may match; ambiguity is an error naming both places.
pub fn resolve_unique_frame(
    page: &dyn Eval,
    selector: &str,
    max_depth: usize,
) -> Result<FramePath> {
    let matches = find_across_frames(page, selector, max_depth)?;
    match matches.len() {
        0 => err(format!("selector matched no document: {selector}")),
        1 => Ok(matches[0].path.clone()),
        n => err(format!(
            "selector {selector} matches in {n} documents: {}. Scope it with eval_in_frame() so the assertion names the document it means.",
            matches.iter().map(|m| format!("{} ({})", describe_frame(&m.path), m.count)).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// Run a function body inside the same-origin frame at `path` (entering and
/// leaving is implicit: each call starts from the top window, so a failure can
/// never leave later selectors scoped to an iframe).
pub fn eval_in_frame_script(path: &[usize], body: &str) -> String {
    format!(
        "var w=window;var p={};for(var i=0;i<p.length;i++){{w=w.frames[p[i]];if(!w)throw new Error('no frame '+p.join('.'));}}\
         return w.eval('(function(){{'+{}+'}})()');",
        json!(path),
        q(body)
    )
}

pub fn eval_in_frame(page: &dyn Eval, path: &[usize], body: &str) -> Result<Value> {
    page.eval_js(&eval_in_frame_script(path, body))
}

// ------------------------------------------------------------------ paint

pub const DEFAULT_SETTLE_MS: u64 = 120;
pub const DEFAULT_PAINT_TIMEOUT_MS: u64 = 10_000;

#[derive(Debug, Clone)]
pub struct PaintMetrics {
    pub request_id: String,
    pub duration_ms: f64,
    pub settle_ms: f64,
    pub mutations: u64,
    /// False when the document was hidden and rAF is throttled: honest, not silent.
    pub animation_frames: bool,
}

fn paint_begin_script(id: &str, selector: Option<&str>) -> String {
    let root = selector
        .map(|s| format!("document.querySelector({})", q(s)))
        .unwrap_or_else(|| "document.documentElement".into());
    format!(
        "var L=window.__rkQaPaint=window.__rkQaPaint||{{marks:{{}},results:{{}}}};var root={root};\
         if(!root)throw new Error('paint target not found');\
         var m={{startedAt:performance.now(),mutations:0,last:performance.now()}};\
         m.observer=new MutationObserver(function(r){{m.mutations+=r.length;m.last=performance.now();}});\
         m.observer.observe(root,{{subtree:true,childList:true,characterData:true,attributes:true}});\
         L.marks[{id}]=m;return true;",
        id = q(id)
    )
}

fn paint_arm_script(id: &str, settle_ms: u64, timeout_ms: u64) -> String {
    format!(
        "var L=window.__rkQaPaint;var id={id};var m=L&&L.marks[id];if(!m)throw new Error('no paint mark');\
         var deadline=performance.now()+{timeout_ms};\
         function finish(af){{m.observer.disconnect();delete L.marks[id];L.results[id]={{durationMs:performance.now()-m.startedAt,settleMs:m.last-m.startedAt,mutations:m.mutations,animationFrames:af}};}}\
         (function poll(){{var quiet=performance.now()-m.last;\
         if(quiet>={settle_ms}){{if(document.hidden){{finish(false);return;}}requestAnimationFrame(function(){{requestAnimationFrame(function(){{finish(true);}});}});return;}}\
         if(performance.now()>=deadline){{m.observer.disconnect();delete L.marks[id];L.results[id]={{error:'paint did not settle within {timeout_ms}ms; '+m.mutations+' mutations'}};return;}}\
         setTimeout(poll,16);}})();return true;",
        id = q(id)
    )
}

fn paint_result_script(id: &str) -> String {
    format!(
        "var L=window.__rkQaPaint;return (L&&L.results[{id}])||null;",
        id = q(id)
    )
}

/// Run `trigger`, then return when the resulting render has actually been
/// painted (DOM quiet, then two animation frames). The mark is installed before
/// the trigger so no mutation slips between them.
pub fn measure_paint(
    page: &dyn Eval,
    trigger: &mut dyn FnMut() -> Result<()>,
    request_id: &str,
    selector: Option<&str>,
    settle_ms: u64,
    timeout_ms: u64,
) -> Result<PaintMetrics> {
    page.eval_js(&paint_begin_script(request_id, selector))?;
    trigger()?;
    page.eval_js(&paint_arm_script(request_id, settle_ms, timeout_ms))?;
    let deadline = Instant::now() + Duration::from_millis(timeout_ms + 2_000);
    loop {
        let r = page.eval_js(&paint_result_script(request_id))?;
        if !r.is_null() {
            if let Some(e) = r.get("error").and_then(Value::as_str) {
                return err(format!("paint measurement failed for {request_id}: {e}"));
            }
            return Ok(PaintMetrics {
                request_id: request_id.into(),
                duration_ms: r["durationMs"].as_f64().unwrap_or(0.0),
                settle_ms: r["settleMs"].as_f64().unwrap_or(0.0),
                mutations: r["mutations"].as_u64().unwrap_or(0),
                animation_frames: r["animationFrames"].as_bool().unwrap_or(false),
            });
        }
        if Instant::now() >= deadline {
            return err(format!("paint measurement for {request_id} never reported"));
        }
        sleep_ms(25);
    }
}

pub fn assert_paint_within(m: &PaintMetrics, budget_ms: f64, label: &str) -> Result<()> {
    if !(budget_ms.is_finite() && budget_ms > 0.0) {
        return err("paint budget must be a positive number of milliseconds");
    }
    if m.duration_ms > budget_ms {
        return err(format!(
            "{label} painted in {}ms, over its {budget_ms}ms budget ({} DOM mutations, settled after {}ms, requestId {})",
            m.duration_ms.round(),
            m.mutations,
            m.settle_ms.round(),
            m.request_id
        ));
    }
    Ok(())
}

// ------------------------------------------------------------------ panes

#[derive(Debug, Clone)]
pub struct PaneGeometry {
    pub selector: String,
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
    pub visible: bool,
    pub active: bool,
    pub matches: usize,
}

fn pane_script(selector: &str) -> String {
    format!(
        "var nodes=document.querySelectorAll({sel});if(nodes.length===0)return null;var chosen=null;\
         function act(n){{return n.getAttribute('aria-selected')==='true'||n.getAttribute('aria-current')==='true'||n.dataset.active==='true'||n.classList.contains('active');}}\
         for(var i=0;i<nodes.length;i++){{if(act(nodes[i])){{chosen=nodes[i];break;}}}}if(!chosen)chosen=nodes[0];\
         var r=chosen.getBoundingClientRect();var s=getComputedStyle(chosen);\
         var hs=s.display==='none'||s.visibility==='hidden'||Number(s.opacity)===0;\
         var ha=chosen.hasAttribute('hidden')||chosen.getAttribute('aria-hidden')==='true';\
         return {{x:r.x,y:r.y,width:r.width,height:r.height,visible:!hs&&!ha&&r.width>0&&r.height>0,\
         active:act(chosen)||(nodes.length===1&&!hs&&!ha),matches:nodes.length}};",
        sel = q(selector)
    )
}

pub fn read_pane_geometry(page: &dyn Eval, selector: &str) -> Result<PaneGeometry> {
    let v = page.eval_js(&pane_script(selector))?;
    if v.is_null() {
        return err(format!("pane selector matched no element: {selector}"));
    }
    Ok(PaneGeometry {
        selector: selector.into(),
        x: v["x"].as_f64().unwrap_or(0.0),
        y: v["y"].as_f64().unwrap_or(0.0),
        width: v["width"].as_f64().unwrap_or(0.0),
        height: v["height"].as_f64().unwrap_or(0.0),
        visible: v["visible"].as_bool().unwrap_or(false),
        active: v["active"].as_bool().unwrap_or(false),
        matches: v["matches"].as_u64().unwrap_or(0) as usize,
    })
}

/// The pane under test must be visible and active; a background tab that happens
/// to be laid out fails "active", a hidden pane fails "visible".
pub fn assert_active_pane(g: &PaneGeometry) -> Result<()> {
    if !g.visible {
        return err(format!(
            "{} is not visible ({}x{} at {},{}); the assertion would describe a pane the user cannot see",
            g.selector, g.width, g.height, g.x, g.y
        ));
    }
    if !g.active {
        return err(format!(
            "{} matched {} panes and the chosen one is not the active pane; scope the selector to the active tab before asserting",
            g.selector, g.matches
        ));
    }
    Ok(())
}

/// Wait until the ACTIVE pane's box has changed from `before` and then held still.
pub fn wait_for_pane_resize(
    page: &dyn Eval,
    selector: &str,
    before: (f64, f64),
    timeout: Duration,
    stable_reads: u32,
) -> Result<PaneGeometry> {
    let deadline = Instant::now() + timeout;
    let mut stable = 0;
    let mut previous: Option<(f64, f64)> = None;
    let mut latest: Option<PaneGeometry> = None;
    while Instant::now() < deadline {
        let g = read_pane_geometry(page, selector)?;
        assert_active_pane(&g)?;
        let changed = g.width != before.0 || g.height != before.1;
        let held = previous == Some((g.width, g.height));
        if changed && held {
            stable += 1;
            if stable >= stable_reads {
                return Ok(g);
            }
        } else if !held {
            stable = 0;
        }
        previous = Some((g.width, g.height));
        latest = Some(g);
        sleep_ms(50);
    }
    err(format!(
        "{selector} did not settle at a new size within {}ms: before {}x{}, last {}",
        timeout.as_millis(),
        before.0,
        before.1,
        latest
            .map(|g| format!("{}x{}", g.width, g.height))
            .unwrap_or_else(|| "unreadable".into())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_scripts_quote_selectors() {
        let s = find_across_frames_script("a[href=\"x\"]", 3);
        assert!(s.contains("a[href=\\\"x\\\"]") && s.contains("d>=3"));
        assert!(eval_in_frame_script(&[0, 2], "return 1;").contains("[0,2]"));
    }

    #[test]
    fn describes_frames_and_matches() {
        assert_eq!(describe_frame(&[]), "top document");
        assert_eq!(describe_frame(&[1, 0]), "frame 1.0");
        let m = parse_frame_matches(&json!([{"path": [], "count": 1}, {"path": [0], "count": 2}]));
        assert_eq!(m.len(), 2);
        assert_eq!(
            m[1],
            FrameMatch {
                path: vec![0],
                count: 2
            }
        );
    }

    struct Fake(Value);
    impl Eval for Fake {
        fn eval_js(&self, _: &str) -> Result<Value> {
            Ok(self.0.clone())
        }
    }

    #[test]
    fn ambiguous_selector_names_both_documents() {
        let e = resolve_unique_frame(
            &Fake(json!([{"path": [], "count": 1}, {"path": [0], "count": 1}])),
            ".t",
            4,
        )
        .unwrap_err();
        assert!(e.0.contains("top document (1), frame 0 (1)"), "{e}");
        assert!(resolve_unique_frame(&Fake(json!([])), ".t", 4).is_err());
        assert_eq!(
            resolve_unique_frame(&Fake(json!([{"path": [2], "count": 1}])), ".t", 4).unwrap(),
            vec![2]
        );
    }

    #[test]
    fn pane_assertions() {
        let mut g = PaneGeometry {
            selector: ".p".into(),
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
            visible: true,
            active: true,
            matches: 1,
        };
        assert!(assert_active_pane(&g).is_ok());
        g.active = false;
        assert!(assert_active_pane(&g)
            .unwrap_err()
            .0
            .contains("not the active pane"));
        g.visible = false;
        assert!(assert_active_pane(&g)
            .unwrap_err()
            .0
            .contains("not visible"));
    }

    #[test]
    fn paint_budget() {
        let m = PaintMetrics {
            request_id: "r".into(),
            duration_ms: 300.0,
            settle_ms: 100.0,
            mutations: 4,
            animation_frames: true,
        };
        assert!(assert_paint_within(&m, 500.0, "open").is_ok());
        assert!(assert_paint_within(&m, 100.0, "open")
            .unwrap_err()
            .0
            .contains("over its 100ms budget"));
    }
}
