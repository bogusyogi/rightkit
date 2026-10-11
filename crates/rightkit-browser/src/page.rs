use crate::error::{cdp, BrowserError, Result};
use crate::keys::{self, KeyDef};
use crate::policy::{ActionKind, AdmissionRequest, BlockedRequest, Guard, LifecycleKind};
use chromiumoxide::cdp::browser_protocol::dom::SetFileInputFilesParams;
use chromiumoxide::cdp::browser_protocol::fetch::{
    ContinueRequestParams, EnableParams as FetchEnableParams, EventRequestPaused,
    FailRequestParams, RequestPattern,
};
use chromiumoxide::cdp::browser_protocol::input::{
    DispatchKeyEventParams, DispatchKeyEventType, DispatchMouseEventParams, DispatchMouseEventType,
    MouseButton,
};
use chromiumoxide::cdp::browser_protocol::inspector::{
    EnableParams as InspectorEnableParams, EventTargetCrashed,
};
use chromiumoxide::cdp::browser_protocol::network::ErrorReason;
use chromiumoxide::cdp::browser_protocol::page::CaptureScreenshotFormat;
use chromiumoxide::cdp::js_protocol::runtime::{EventConsoleApiCalled, EventExceptionThrown};
use chromiumoxide::page::{Page, ScreenshotParams};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

pub const MAX_CONSOLE_LINES: usize = 500;
const MAX_UPLOAD_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, Serialize)]
pub struct ConsoleLine {
    pub level: String,
    pub text: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct ObservedNode {
    /// `r<epoch>.<n>`; valid only while the page stays unchanged.
    pub node_ref: String,
    pub role: String,
    pub name: String,
    pub tag: String,
    pub selector: String,
    pub visible: bool,
    pub disabled: bool,
    pub bounds: Rect,
}

#[derive(Clone, Debug, Serialize)]
pub struct Observation {
    pub page_id: String,
    pub epoch: u64,
    pub url: String,
    pub title: String,
    pub viewport: Rect,
    pub nodes: Vec<ObservedNode>,
    pub console_errors: usize,
    /// Refs/roles/names added, removed, or changed since the previous observation.
    pub added: Vec<String>,
    pub removed: Vec<String>,
}

/// What an action addresses.
#[derive(Clone, Debug)]
pub enum Target {
    /// Ref from [`BrowserPage::observe`]; checked for staleness.
    Ref(String),
    /// A CSS selector that must match exactly one element.
    Selector(String),
    /// Viewport coordinates.
    Point(f64, f64),
}

impl Target {
    pub fn css(s: impl Into<String>) -> Self {
        Target::Selector(s.into())
    }
    pub fn node_ref(s: impl Into<String>) -> Self {
        Target::Ref(s.into())
    }
}

#[derive(Clone, Debug, Deserialize)]
struct RawNode {
    role: String,
    name: String,
    tag: String,
    selector: String,
    visible: bool,
    disabled: bool,
    #[serde(default)]
    sensitive: bool,
    bounds: Rect,
}

#[derive(Clone, Debug)]
struct RefEntry {
    selector: String,
    tag: String,
    role: String,
    name: String,
}

#[derive(Default)]
struct ObsState {
    epoch: u64,
    fingerprint: String,
    refs: HashMap<String, RefEntry>,
    previous: HashMap<String, String>,
}

pub struct BrowserPage {
    id: String,
    inner: Page,
    console: Arc<Mutex<Vec<ConsoleLine>>>,
    obs: StdMutex<ObsState>,
    mouse: StdMutex<(f64, f64)>,
    buttons: StdMutex<i64>,
    /// Serializes mutations so concurrent callers commit in order.
    lane: Mutex<()>,
    upload_root: Option<PathBuf>,
    pumps: StdMutex<Vec<JoinHandle<()>>>,
    guard: Arc<Guard>,
    blocked: Arc<StdMutex<Vec<BlockedRequest>>>,
}

impl BrowserPage {
    pub(crate) async fn attach(
        id: String,
        inner: Page,
        upload_root: Option<PathBuf>,
        guard: Arc<Guard>,
    ) -> Result<Self> {
        let console = Arc::new(Mutex::new(Vec::new()));
        let mut pumps = Vec::new();
        let mut ev = inner
            .event_listener::<EventConsoleApiCalled>()
            .await
            .map_err(cdp)?;
        let sink = console.clone();
        pumps.push(tokio::spawn(async move {
            while let Some(e) = ev.next().await {
                let text = e
                    .args
                    .iter()
                    .map(|a| match (&a.value, &a.description) {
                        (Some(Value::String(s)), _) => s.clone(),
                        (Some(v), _) => v.to_string(),
                        (None, Some(d)) => d.clone(),
                        _ => String::new(),
                    })
                    .collect::<Vec<_>>()
                    .join(" ");
                push_capped(
                    &sink,
                    ConsoleLine {
                        level: format!("{:?}", e.r#type).to_lowercase(),
                        text,
                    },
                )
                .await;
            }
        }));
        let mut ex = inner
            .event_listener::<EventExceptionThrown>()
            .await
            .map_err(cdp)?;
        let sink = console.clone();
        pumps.push(tokio::spawn(async move {
            while let Some(e) = ex.next().await {
                let d = &e.exception_details;
                let text = d
                    .exception
                    .as_ref()
                    .and_then(|x| x.description.clone())
                    .unwrap_or_else(|| d.text.clone());
                push_capped(
                    &sink,
                    ConsoleLine {
                        level: "exception".into(),
                        text,
                    },
                )
                .await;
            }
        }));
        // Renderer crash -> `Crash` lifecycle event for this page.
        let mut crashed = inner
            .event_listener::<EventTargetCrashed>()
            .await
            .map_err(cdp)?;
        let (g, page_id) = (guard.clone(), id.clone());
        pumps.push(tokio::spawn(async move {
            while crashed.next().await.is_some() {
                g.emit_kind(LifecycleKind::Crash, "renderer crashed", |e| {
                    e.page_id = Some(page_id.clone());
                });
            }
        }));
        inner
            .execute(InspectorEnableParams::default())
            .await
            .map_err(cdp)?;
        let blocked = Arc::new(StdMutex::new(Vec::new()));
        if !guard.network.unrestricted {
            // Pause every request (document, redirect hops, subresources) and
            // decide before it leaves the browser. Enabled before any navigation.
            let mut paused = inner
                .event_listener::<EventRequestPaused>()
                .await
                .map_err(cdp)?;
            let page = inner.clone();
            let g = guard.clone();
            let sink = blocked.clone();
            let page_id = id.clone();
            pumps.push(tokio::spawn(async move {
                while let Some(e) = paused.next().await {
                    let page = page.clone();
                    let g = g.clone();
                    let sink = sink.clone();
                    let pid = page_id.clone();
                    tokio::spawn(async move {
                        let kind = format!("{:?}", e.resource_type);
                        match g.network.check(&e.request.url, &kind).await {
                            Ok(()) => {
                                let _ = page
                                    .execute(ContinueRequestParams::new(e.request_id.clone()))
                                    .await;
                            }
                            Err(reason) => {
                                let _ = page
                                    .execute(FailRequestParams::new(
                                        e.request_id.clone(),
                                        ErrorReason::BlockedByClient,
                                    ))
                                    .await;
                                g.denied_network(Some(&pid), &kind, &reason);
                                let mut b = sink.lock().unwrap();
                                if b.len() < 500 {
                                    b.push(BlockedRequest {
                                        url: e.request.url.clone(),
                                        reason: reason.to_string(),
                                    });
                                }
                            }
                        }
                    });
                }
            }));
            let en = FetchEnableParams {
                patterns: Some(vec![RequestPattern {
                    url_pattern: Some("*".into()),
                    resource_type: None,
                    request_stage: None,
                }]),
                ..Default::default()
            };
            inner.execute(en).await.map_err(cdp)?;
        }
        Ok(Self {
            id,
            inner,
            console,
            obs: StdMutex::new(ObsState::default()),
            mouse: StdMutex::new((0.0, 0.0)),
            buttons: StdMutex::new(0),
            lane: Mutex::new(()),
            upload_root,
            pumps: StdMutex::new(pumps),
            guard,
            blocked,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    /// Requests the network policy refused on this page (redirect hops and subresources included).
    pub fn blocked_requests(&self) -> Vec<BlockedRequest> {
        self.blocked.lock().unwrap().clone()
    }

    /// Typed pre-effect admission. Reads no page state beyond the current URL.
    async fn admit(
        &self,
        action: ActionKind,
        url: Option<String>,
        target: Option<String>,
        detail: Option<String>,
    ) -> Result<()> {
        let url = match url {
            Some(u) => Some(u),
            None if self.guard.admission.is_some() => Some(self.url().await),
            None => None,
        };
        self.guard
            .admit(AdmissionRequest {
                session_id: self.guard.session_id.clone(),
                page_id: self.id.clone(),
                action,
                url,
                target,
                detail,
            })
            .await
    }

    /// Escape hatch to the underlying chromiumoxide page.
    pub fn raw(&self) -> &Page {
        &self.inner
    }

    pub(crate) fn abort_pumps(&self) {
        if let Ok(mut p) = self.pumps.lock() {
            for h in p.drain(..) {
                h.abort();
            }
        }
    }

    pub(crate) async fn close_inner(&self) -> Result<()> {
        self.abort_pumps();
        self.inner.clone().close().await.map_err(cdp)
    }

    pub async fn bring_to_front(&self) -> Result<()> {
        self.inner.bring_to_front().await.map_err(cdp)?;
        Ok(())
    }

    // ---- navigation ------------------------------------------------------

    fn invalidate(&self) {
        let mut o = self.obs.lock().unwrap();
        o.epoch += 1;
        o.refs.clear();
        o.fingerprint.clear();
    }

    /// Waits until this new tab's own first navigation (to about:blank) has committed.
    ///
    /// A tab is created at about:blank and that navigation commits a little later. A
    /// navigation sent before then is discarded by that commit: Chrome answers it with
    /// `net::ERR_ABORTED` and no load ever follows, so the caller waits out its whole request
    /// timeout. Seen on Windows, where a new tab's renderer starts more slowly.
    ///
    /// Before the commit the tab's committed history entry has an empty URL. The tab counts
    /// as settled when that entry has a URL and the main frame's loader is the same on two
    /// consecutive polls. Bounded at two seconds, and it stops at once if Chrome does not
    /// answer, so a browser that behaves differently is not waited on.
    pub(crate) async fn settle_initial_document(&self) {
        use chromiumoxide::cdp::browser_protocol::page::{
            GetFrameTreeParams, GetNavigationHistoryParams,
        };
        use std::time::{Duration, Instant};
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut seen: Option<String> = None;
        while Instant::now() < deadline {
            let Ok(history) = self
                .inner
                .execute(GetNavigationHistoryParams::default())
                .await
            else {
                return;
            };
            let Ok(tree) = self.inner.execute(GetFrameTreeParams::default()).await else {
                return;
            };
            let committed = usize::try_from(history.result.current_index)
                .ok()
                .and_then(|index| history.result.entries.get(index))
                .is_some_and(|entry| !entry.url.is_empty());
            let loader = format!("{:?}", tree.result.frame_tree.frame.loader_id);
            if !committed {
                seen = None;
            } else if seen.as_deref() == Some(loader.as_str()) {
                return;
            } else {
                seen = Some(loader);
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }

    pub async fn goto(&self, url: &str) -> Result<()> {
        // EFF-001 order: policy, then approval; both before any side effect.
        self.guard
            .check_url(Some(&self.id), url, "Navigation")
            .await?;
        self.admit(ActionKind::Navigate, Some(url.to_string()), None, None)
            .await?;
        let _l = self.lane.lock().await;
        self.inner
            .goto(url)
            .await
            .map_err(|e| BrowserError::Cdp(format!("navigate '{url}': {e}")))?;
        self.inner.wait_for_navigation().await.map_err(cdp)?;
        self.invalidate();
        Ok(())
    }

    pub async fn reload(&self) -> Result<()> {
        self.admit(ActionKind::Reload, None, None, None).await?;
        let _l = self.lane.lock().await;
        self.inner.reload().await.map_err(cdp)?;
        self.inner.wait_for_navigation().await.map_err(cdp)?;
        self.invalidate();
        Ok(())
    }

    pub async fn history(&self, delta: i32) -> Result<()> {
        self.admit(
            ActionKind::History,
            None,
            None,
            Some(format!("delta={delta}")),
        )
        .await?;
        let _l = self.lane.lock().await;
        self.inner
            .evaluate(format!("history.go({delta})"))
            .await
            .map_err(cdp)?;
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.inner.wait_for_navigation().await.map_err(cdp)?;
        self.invalidate();
        Ok(())
    }

    pub async fn url(&self) -> String {
        self.inner.url().await.ok().flatten().unwrap_or_default()
    }

    pub async fn title(&self) -> String {
        self.inner
            .get_title()
            .await
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    /// Rendered text (`document.body.innerText`).
    pub async fn text(&self) -> Result<String> {
        self.eval_internal("document.body ? document.body.innerText : ''")
            .await
            .map(|v| v.as_str().unwrap_or_default().to_string())
    }

    /// Evaluate a JS expression and return its JSON value. Script can navigate and
    /// act, so it is admitted like any other effect.
    pub async fn eval(&self, expr: &str) -> Result<Value> {
        self.admit(
            ActionKind::Eval,
            None,
            None,
            Some(expr.chars().take(2000).collect()),
        )
        .await?;
        self.eval_internal(expr).await
    }

    /// Runtime-owned evaluation (observation, geometry); never user script.
    async fn eval_internal(&self, expr: &str) -> Result<Value> {
        let r = self.inner.evaluate(expr).await.map_err(cdp)?;
        Ok(r.value().cloned().unwrap_or(Value::Null))
    }

    pub async fn set_viewport(&self, width: u32, height: u32, scale: f64) -> Result<()> {
        use chromiumoxide::cdp::browser_protocol::emulation::SetDeviceMetricsOverrideParams;
        self.inner
            .execute(SetDeviceMetricsOverrideParams::new(
                i64::from(width),
                i64::from(height),
                scale,
                false,
            ))
            .await
            .map_err(cdp)?;
        Ok(())
    }

    // ---- observation -----------------------------------------------------

    pub async fn observe(&self) -> Result<Observation> {
        let _l = self.lane.lock().await;
        let raw: Vec<RawNode> = self
            .inner
            .evaluate(OBSERVE_JS)
            .await
            .map_err(cdp)?
            .into_value()
            .map_err(|e| BrowserError::Cdp(format!("decode observation: {e}")))?;
        let viewport: Rect = self
            .inner
            .evaluate("({x:0,y:0,width:window.innerWidth,height:window.innerHeight})")
            .await
            .map_err(cdp)?
            .into_value()
            .map_err(cdp)?;
        let fingerprint = raw
            .iter()
            .map(|n| {
                format!(
                    "{}|{}|{}|{}|{}",
                    n.selector, n.role, n.name, n.visible, n.disabled
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (epoch, nodes, added, removed) = {
            let mut o = self.obs.lock().unwrap();
            if o.fingerprint != fingerprint {
                o.epoch += 1;
                o.fingerprint = fingerprint;
            }
            let epoch = o.epoch;
            o.refs.clear();
            let mut cur = HashMap::new();
            let mut nodes = Vec::new();
            for (i, n) in raw.into_iter().enumerate() {
                let name = if n.sensitive {
                    "••••••••".to_string()
                } else {
                    n.name.clone()
                };
                let node_ref = format!("r{epoch}.{}", i + 1);
                if !n.selector.is_empty() {
                    o.refs.insert(
                        node_ref.clone(),
                        RefEntry {
                            selector: n.selector.clone(),
                            tag: n.tag.clone(),
                            role: n.role.clone(),
                            name: name.clone(),
                        },
                    );
                    cur.insert(n.selector.clone(), format!("{}|{}", n.role, name));
                }
                nodes.push(ObservedNode {
                    node_ref,
                    role: n.role,
                    name,
                    tag: n.tag,
                    selector: n.selector,
                    visible: n.visible,
                    disabled: n.disabled,
                    bounds: n.bounds,
                });
            }
            let added = cur
                .keys()
                .filter(|k| !o.previous.contains_key(*k))
                .cloned()
                .collect();
            let removed = o
                .previous
                .keys()
                .filter(|k| !cur.contains_key(*k))
                .cloned()
                .collect();
            o.previous = cur;
            (epoch, nodes, added, removed)
        };
        let console_errors = self
            .console
            .lock()
            .await
            .iter()
            .filter(|l| matches!(l.level.as_str(), "error" | "exception"))
            .count();
        Ok(Observation {
            page_id: self.id.clone(),
            epoch,
            url: self.url().await,
            title: self.title().await,
            viewport,
            nodes,
            console_errors,
            added,
            removed,
        })
    }

    /// Resolve a target to a unique CSS selector, rejecting stale refs.
    async fn resolve(&self, target: &Target) -> Result<Option<String>> {
        match target {
            Target::Point(..) => Ok(None),
            Target::Selector(s) => {
                self.unique(s).await?;
                Ok(Some(s.clone()))
            }
            Target::Ref(r) => {
                let (entry, current_epoch) = {
                    let o = self.obs.lock().unwrap();
                    (o.refs.get(r).cloned(), o.epoch)
                };
                let entry = entry.ok_or_else(|| BrowserError::StaleRef(r.clone()))?;
                let ref_epoch: u64 = r
                    .trim_start_matches('r')
                    .split('.')
                    .next()
                    .and_then(|e| e.parse().ok())
                    .unwrap_or(0);
                if ref_epoch != current_epoch {
                    return Err(BrowserError::StaleRef(r.clone()));
                }
                // Identity check: the element behind the selector must still look
                // like what was observed, else the page changed under the ref.
                let sig = self
                    .eval(&format!(
                        "(()=>{{const l=document.querySelectorAll({s});if(l.length!==1)return null;const e=l[0];\
                         return {{tag:e.tagName.toLowerCase(),name:(e.getAttribute('aria-label')||e.innerText||e.value||e.getAttribute('placeholder')||'').trim().slice(0,160)}}}})()",
                        s = js(&entry.selector)
                    ))
                    .await?;
                let ok = sig.get("tag").and_then(Value::as_str) == Some(entry.tag.as_str())
                    && sig.get("name").and_then(Value::as_str) == Some(entry.name.as_str())
                    || (sig.get("tag").and_then(Value::as_str) == Some(entry.tag.as_str())
                        && matches!(entry.tag.as_str(), "input" | "textarea" | "select")); // value changes as the user types
                if !ok {
                    return Err(BrowserError::StaleRef(r.clone()));
                }
                let _ = &entry.role;
                Ok(Some(entry.selector))
            }
        }
    }

    async fn unique(&self, selector: &str) -> Result<()> {
        let n = self
            .eval_internal(&format!(
                "document.querySelectorAll({}).length",
                js(selector)
            ))
            .await?
            .as_u64()
            .unwrap_or(0);
        match n {
            1 => Ok(()),
            0 => Err(BrowserError::NoElement(selector.into())),
            n => Err(BrowserError::Ambiguous(selector.into(), n)),
        }
    }

    /// Scroll into view, return the centre point, and verify nothing covers it.
    async fn point_of(&self, target: &Target) -> Result<(f64, f64)> {
        if let Target::Point(x, y) = target {
            return Ok((*x, *y));
        }
        let sel = self.resolve(target).await?.expect("selector");
        let v = self
            .eval(&format!(
                "(()=>{{const e=document.querySelector({s});e.scrollIntoView({{block:'center',inline:'center'}});\
                 const r=e.getBoundingClientRect();if(r.width<=0||r.height<=0)return null;\
                 const x=r.x+r.width/2,y=r.y+r.height/2;const t=document.elementFromPoint(x,y);\
                 const hit=!!t&&(t===e||e.contains(t)||t.contains(e));return {{x,y,hit}}}})()",
                s = js(&sel)
            ))
            .await?;
        if v.is_null() {
            return Err(BrowserError::NotInteractable(sel));
        }
        if v.get("hit").and_then(Value::as_bool) != Some(true) {
            return Err(BrowserError::NotInteractable(format!(
                "{sel} (covered by another element)"
            )));
        }
        Ok((
            v["x"].as_f64().unwrap_or(0.0),
            v["y"].as_f64().unwrap_or(0.0),
        ))
    }

    // ---- real pointer input ----------------------------------------------

    async fn mouse_event(
        &self,
        kind: DispatchMouseEventType,
        x: f64,
        y: f64,
        button: MouseButton,
        click_count: i64,
    ) -> Result<()> {
        let mut p = DispatchMouseEventParams::new(kind.clone(), x, y);
        p.button = Some(button);
        p.click_count = Some(click_count);
        p.buttons = Some(match kind {
            DispatchMouseEventType::MousePressed => 1,
            DispatchMouseEventType::MouseReleased => 0,
            _ => *self.buttons.lock().unwrap(),
        });
        self.inner.execute(p).await.map_err(cdp)?;
        *self.mouse.lock().unwrap() = (x, y);
        Ok(())
    }

    async fn move_to(&self, x: f64, y: f64) -> Result<()> {
        self.mouse_event(
            DispatchMouseEventType::MouseMoved,
            x,
            y,
            MouseButton::None,
            0,
        )
        .await
    }

    pub async fn hover(&self, target: &Target) -> Result<()> {
        self.admit(ActionKind::Hover, None, Some(describe(target)), None)
            .await?;
        let _l = self.lane.lock().await;
        let (x, y) = self.point_of(target).await?;
        self.move_to(x, y).await
    }

    pub async fn click(&self, target: &Target) -> Result<()> {
        self.admit(ActionKind::Click, None, Some(describe(target)), None)
            .await?;
        let _l = self.lane.lock().await;
        self.click_n(target, 1).await
    }

    pub async fn double_click(&self, target: &Target) -> Result<()> {
        self.admit(ActionKind::DoubleClick, None, Some(describe(target)), None)
            .await?;
        let _l = self.lane.lock().await;
        self.click_n(target, 2).await
    }

    /// Caller holds the lane.
    async fn click_n(&self, target: &Target, count: i64) -> Result<()> {
        let (x, y) = self.point_of(target).await?;
        self.move_to(x, y).await?;
        for n in 1..=count {
            self.mouse_event(
                DispatchMouseEventType::MousePressed,
                x,
                y,
                MouseButton::Left,
                n,
            )
            .await?;
            self.mouse_event(
                DispatchMouseEventType::MouseReleased,
                x,
                y,
                MouseButton::Left,
                n,
            )
            .await?;
        }
        Ok(())
    }

    /// Press at `from`, move in `steps` increments, release at `to`.
    pub async fn drag(&self, from: &Target, to: &Target, steps: u32) -> Result<()> {
        self.admit(
            ActionKind::Drag,
            None,
            Some(format!("{} -> {}", describe(from), describe(to))),
            None,
        )
        .await?;
        let _l = self.lane.lock().await;
        let (x0, y0) = self.point_of(from).await?;
        let (x1, y1) = self.point_of(to).await?;
        self.move_to(x0, y0).await?;
        self.mouse_event(
            DispatchMouseEventType::MousePressed,
            x0,
            y0,
            MouseButton::Left,
            1,
        )
        .await?;
        *self.buttons.lock().unwrap() = 1;
        let steps = steps.max(1);
        for i in 1..=steps {
            let t = f64::from(i) / f64::from(steps);
            self.mouse_event(
                DispatchMouseEventType::MouseMoved,
                x0 + (x1 - x0) * t,
                y0 + (y1 - y0) * t,
                MouseButton::Left,
                0,
            )
            .await?;
        }
        *self.buttons.lock().unwrap() = 0;
        self.mouse_event(
            DispatchMouseEventType::MouseReleased,
            x1,
            y1,
            MouseButton::Left,
            1,
        )
        .await
    }

    /// Mouse-wheel scroll at the target (or viewport centre when `None`).
    pub async fn wheel(&self, at: Option<&Target>, dx: f64, dy: f64) -> Result<()> {
        self.admit(
            ActionKind::Wheel,
            None,
            at.map(describe),
            Some(format!("dx={dx} dy={dy}")),
        )
        .await?;
        let _l = self.lane.lock().await;
        let (x, y) = match at {
            Some(t) => self.point_of(t).await?,
            None => {
                let v = self
                    .eval_internal("({x:innerWidth/2,y:innerHeight/2})")
                    .await?;
                (
                    v["x"].as_f64().unwrap_or(100.0),
                    v["y"].as_f64().unwrap_or(100.0),
                )
            }
        };
        let mut p = DispatchMouseEventParams::new(DispatchMouseEventType::MouseWheel, x, y);
        p.delta_x = Some(dx);
        p.delta_y = Some(dy);
        self.inner.execute(p).await.map_err(cdp)?;
        Ok(())
    }

    // ---- real keyboard input ---------------------------------------------

    async fn key_event(
        &self,
        kind: DispatchKeyEventType,
        d: &KeyDef,
        mods: i64,
        with_text: bool,
    ) -> Result<()> {
        let mut p = DispatchKeyEventParams::new(kind);
        p.modifiers = Some(mods);
        p.key = Some(d.key.clone());
        if !d.code.is_empty() {
            p.code = Some(d.code.clone());
        }
        p.windows_virtual_key_code = Some(d.vk);
        p.native_virtual_key_code = Some(d.vk);
        if with_text {
            p.text = d.text.clone();
            p.unmodified_text = d.text.clone();
        }
        self.inner.execute(p).await.map_err(cdp)?;
        Ok(())
    }

    async fn press_locked(&self, spec: &str) -> Result<()> {
        let (mods, mod_keys, def) = keys::parse_chord(spec)?;
        for (_, m) in &mod_keys {
            self.key_event(DispatchKeyEventType::RawKeyDown, m, mods, false)
                .await?;
        }
        // Shortcut chords (Ctrl/Meta/Alt) carry no text.
        let text = mods & (keys::CTRL | keys::META | keys::ALT) == 0;
        let kind = if text && def.text.is_some() {
            DispatchKeyEventType::KeyDown
        } else {
            DispatchKeyEventType::RawKeyDown
        };
        self.key_event(kind, &def, mods, text).await?;
        self.key_event(DispatchKeyEventType::KeyUp, &def, mods, false)
            .await?;
        for (_, m) in mod_keys.iter().rev() {
            self.key_event(DispatchKeyEventType::KeyUp, m, 0, false)
                .await?;
        }
        Ok(())
    }

    /// Press a key or chord (`Enter`, `Tab`, `Control+a`) on the focused element.
    pub async fn press(&self, spec: &str) -> Result<()> {
        self.admit(ActionKind::Press, None, Some(spec.to_string()), None)
            .await?;
        let _l = self.lane.lock().await;
        self.press_locked(spec).await
    }

    /// Type text one key event at a time into the focused element.
    pub async fn type_text(&self, text: &str) -> Result<()> {
        self.admit(
            ActionKind::TypeText,
            None,
            None,
            Some(format!("chars={}", text.chars().count())),
        )
        .await?;
        let _l = self.lane.lock().await;
        self.type_locked(text).await
    }

    async fn type_locked(&self, text: &str) -> Result<()> {
        for c in text.chars() {
            let d = keys::char_key(c);
            self.key_event(DispatchKeyEventType::KeyDown, &d, 0, true)
                .await?;
            self.key_event(DispatchKeyEventType::KeyUp, &d, 0, false)
                .await?;
        }
        Ok(())
    }

    /// Click the target to focus it, then type.
    pub async fn fill(&self, target: &Target, text: &str, clear: bool) -> Result<()> {
        self.admit(
            ActionKind::Fill,
            None,
            Some(describe(target)),
            Some(format!("chars={} clear={clear}", text.chars().count())),
        )
        .await?;
        let _l = self.lane.lock().await;
        self.click_n(target, 1).await?;
        if clear {
            let sel_all = if cfg!(target_os = "macos") {
                "Meta+a"
            } else {
                "Control+a"
            };
            self.press_locked(sel_all).await?;
            self.press_locked("Backspace").await?;
        }
        self.type_locked(text).await
    }

    // ---- other actions -----------------------------------------------------

    /// Choose a `<select>` option by value/label. Native popups cannot be driven
    /// over CDP, so this sets the value and fires `input`/`change` (documented gap).
    pub async fn select_option(&self, target: &Target, value: &str) -> Result<()> {
        self.admit(
            ActionKind::Select,
            None,
            Some(describe(target)),
            Some(format!("value={value}")),
        )
        .await?;
        let _l = self.lane.lock().await;
        let sel = self
            .resolve(target)
            .await?
            .ok_or_else(|| BrowserError::Invalid("select needs an element".into()))?;
        let r = self
            .eval(&format!(
                "(()=>{{const e=document.querySelector({s});const w={v};\
                 const o=[...e.options].find(o=>o.value===w||o.label===w||o.text.trim()===w);if(!o)return 'nooption';\
                 e.value=o.value;e.dispatchEvent(new Event('input',{{bubbles:true}}));e.dispatchEvent(new Event('change',{{bubbles:true}}));return 'ok'}})()",
                s = js(&sel),
                v = js(value)
            ))
            .await?;
        if r.as_str() == Some("ok") {
            Ok(())
        } else {
            Err(BrowserError::Invalid(format!(
                "'{sel}' has no option '{value}'"
            )))
        }
    }

    pub async fn upload(&self, target: &Target, file: &Path) -> Result<()> {
        self.admit(
            ActionKind::Upload,
            None,
            Some(describe(target)),
            Some(file.display().to_string()),
        )
        .await?;
        let _l = self.lane.lock().await;
        let file = file.canonicalize()?;
        if !file.is_file() {
            return Err(BrowserError::Invalid(
                "upload must be a regular file".into(),
            ));
        }
        if let Some(root) = &self.upload_root {
            if !file.starts_with(root.canonicalize()?) {
                return Err(BrowserError::Invalid(
                    "upload outside the configured upload root".into(),
                ));
            }
        }
        if std::fs::metadata(&file)?.len() > MAX_UPLOAD_BYTES {
            return Err(BrowserError::Invalid("upload exceeds 64 MiB".into()));
        }
        let sel = self
            .resolve(target)
            .await?
            .ok_or_else(|| BrowserError::Invalid("upload needs an element".into()))?;
        let el = self
            .inner
            .find_element(sel.as_str())
            .await
            .map_err(|_| BrowserError::NoElement(sel.clone()))?;
        let node = el.description().await.map_err(cdp)?;
        self.inner
            .execute(
                SetFileInputFilesParams::builder()
                    .file(file.to_string_lossy().to_string())
                    .backend_node_id(node.backend_node_id)
                    .build()
                    .map_err(BrowserError::Invalid)?,
            )
            .await
            .map_err(cdp)?;
        Ok(())
    }

    pub async fn wait_for(&self, selector: &str, timeout: Duration) -> Result<()> {
        self.wait_eval(
            &format!("!!document.querySelector({})", js(selector)),
            timeout,
        )
        .await
    }

    /// Poll a JS expression until truthy.
    pub async fn wait_eval(&self, expr: &str, timeout: Duration) -> Result<()> {
        self.admit(
            ActionKind::Eval,
            None,
            None,
            Some(expr.chars().take(2000).collect()),
        )
        .await?;
        let deadline = Instant::now() + timeout;
        loop {
            let v = self.eval_internal(expr).await?;
            if v.as_bool()
                .unwrap_or(!v.is_null() && v != Value::Bool(false))
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(BrowserError::Timeout(expr.to_string()));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    pub async fn screenshot(&self) -> Result<Vec<u8>> {
        self.inner
            .screenshot(
                ScreenshotParams::builder()
                    .format(CaptureScreenshotFormat::Png)
                    .build(),
            )
            .await
            .map_err(cdp)
    }

    pub async fn screenshot_to(&self, path: &Path) -> Result<()> {
        std::fs::write(path, self.screenshot().await?)?;
        Ok(())
    }

    pub async fn console(&self) -> Vec<ConsoleLine> {
        self.console.lock().await.clone()
    }

    pub async fn clear_console(&self) {
        self.console.lock().await.clear();
    }
}

async fn push_capped(sink: &Arc<Mutex<Vec<ConsoleLine>>>, line: ConsoleLine) {
    let mut g = sink.lock().await;
    if g.len() >= MAX_CONSOLE_LINES {
        g.remove(0);
    }
    g.push(line);
}

fn describe(t: &Target) -> String {
    match t {
        Target::Ref(r) => format!("ref:{r}"),
        Target::Selector(s) => format!("css:{s}"),
        Target::Point(x, y) => format!("point:{x},{y}"),
    }
}

fn js(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

const OBSERVE_JS: &str = r#"(() => {
  const esc = globalThis.CSS?.escape || (v => String(v).replace(/[^a-zA-Z0-9_-]/g, '\\$&'));
  const unique = c => { if (!c) return null; try { return document.querySelectorAll(c).length === 1 ? c : null; } catch (_) { return null; } };
  const selectorFor = el => {
    if (el.id) { const s = unique('#' + esc(el.id)); if (s) return s; }
    for (const a of ['data-testid', 'name', 'aria-label']) {
      const v = el.getAttribute(a);
      if (v) { const s = unique(el.tagName.toLowerCase() + '[' + a + '="' + esc(v) + '"]'); if (s) return s; }
    }
    const parts = [];
    for (let n = el; n && n.nodeType === 1; n = n.parentElement) {
      let p = n.tagName.toLowerCase();
      const sib = n.parentElement ? [...n.parentElement.children].filter(x => x.tagName === n.tagName) : [];
      if (sib.length > 1) p += ':nth-of-type(' + (sib.indexOf(n) + 1) + ')';
      parts.unshift(p);
      const c = parts.join(' > ');
      if (unique(c)) return c;
    }
    return '';
  };
  const sel = 'a[href],button,input,select,textarea,[role],[tabindex],[contenteditable="true"],[onclick]';
  return [...document.querySelectorAll(sel)].slice(0, 300).map(el => {
    const r = el.getBoundingClientRect();
    const ac = (el.getAttribute('autocomplete') || '').trim().toLowerCase();
    const sensitive = (el instanceof HTMLInputElement && el.type.toLowerCase() === 'password') || ac === 'current-password' || ac === 'new-password';
    const visible = (() => {
      for (let n = el; n && n.nodeType === 1; n = n.parentElement) {
        const s = getComputedStyle(n);
        if (n.hidden || n.getAttribute('aria-hidden') === 'true' || s.visibility === 'hidden' || s.display === 'none' || s.opacity === '0') return false;
      }
      return r.width > 0 && r.height > 0;
    })();
    return {
      role: el.getAttribute('role') || ({A:'link',BUTTON:'button',INPUT:'textbox',SELECT:'combobox',TEXTAREA:'textbox'}[el.tagName] || el.tagName.toLowerCase()),
      name: (el.getAttribute('aria-label') || el.innerText || el.value || el.getAttribute('placeholder') || '').trim().slice(0, 160),
      tag: el.tagName.toLowerCase(), selector: selectorFor(el), visible,
      disabled: !!el.disabled || el.getAttribute('aria-disabled') === 'true', sensitive,
      bounds: {x: r.x, y: r.y, width: r.width, height: r.height}
    };
  });
})()"#;
