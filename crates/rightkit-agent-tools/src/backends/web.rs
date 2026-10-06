//! `browser.*` tools over `rightkit-browser` (Chrome over CDP): open, read
//! (text plus ref-addressed observation), click and type with real
//! `Input.dispatch*` events, screenshot, console, close. The session is lazy
//! (first `browser.open`), isolated (temporary profile unless the caller passes
//! `LaunchOptions` with a named one), muted, and bound by `rightkit-browser`'s
//! network policy (loopback, private ranges and `file:` blocked unless the
//! caller's `LaunchOptions` allow them). Dropping the [`BrowserTools`] shuts the
//! browser down.

use crate::spec::*;
use rightkit_browser::{BrowserError, BrowserPage, BrowserSession, LaunchOptions, Target};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::runtime::Runtime;

fn berr(e: BrowserError) -> ToolError {
    ToolError::new("browser_error", e.to_string())
}

pub struct BrowserTools {
    rt: Runtime,
    opts: LaunchOptions,
    session: Mutex<Option<Arc<BrowserSession>>>,
}

impl BrowserTools {
    pub fn new(opts: LaunchOptions) -> Result<Arc<Self>, ToolError> {
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().map_err(|e| ToolError::new("runtime", e.to_string()))?;
        Ok(Arc::new(Self { rt, opts, session: Mutex::new(None) }))
    }

    fn session(&self) -> Result<Arc<BrowserSession>, ToolError> {
        self.session.lock().unwrap_or_else(|e| e.into_inner()).clone().ok_or_else(|| ToolError::new("no_session", "no browser session: call browser.open first"))
    }

    fn page(&self) -> Result<Arc<BrowserPage>, ToolError> {
        self.session()?.current().map_err(berr)
    }

    /// Close the browser now (also happens on drop).
    pub fn shutdown(&self) {
        let s = self.session.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(s) = s {
            self.rt.block_on(s.shutdown());
        }
    }

    /// The `browser.*` tool set bound to this instance.
    pub fn tools(self: &Arc<Self>) -> Vec<Arc<dyn Tool>> {
        let mk = |name: &str, desc: &str, effect: Effect, schema: Value, run: fn(&BrowserTools, Value) -> Result<Value, ToolError>| -> Arc<dyn Tool> {
            Arc::new(WebTool { spec: ToolSpec::new(name, desc, effect, schema), host: self.clone(), run })
        };
        let target = json!({"ref": {"type": "string", "description": "node ref from browser.read"}, "selector": {"type": "string", "description": "CSS selector matching exactly one element"}});
        vec![
            mk("browser.open", "Open a URL in a new page (starts an isolated, muted Chrome on first use).", Effect::Network, json!({"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}), |h, a| {
                let url = a["url"].as_str().unwrap_or_default().to_string();
                let existing = h.session.lock().unwrap_or_else(|e| e.into_inner()).clone();
                let session = match existing {
                    Some(s) => s,
                    None => {
                        let s = Arc::new(h.rt.block_on(BrowserSession::launch(h.opts.clone())).map_err(berr)?);
                        *h.session.lock().unwrap_or_else(|e| e.into_inner()) = Some(s.clone());
                        s
                    }
                };
                let page = h.rt.block_on(session.new_page(&url)).map_err(berr)?;
                Ok(json!({"page_id": page.id(), "url": h.rt.block_on(page.url()), "title": h.rt.block_on(page.title())}))
            }),
            mk("browser.read", "Visible text and ref-addressed interactive nodes of the current page.", Effect::Observe, json!({"type": "object", "properties": {"max_text": {"type": "integer"}}}), |h, a| {
                let page = h.page()?;
                let mut text = h.rt.block_on(page.text()).map_err(berr)?;
                let max = a["max_text"].as_u64().unwrap_or(20_000) as usize;
                if text.chars().count() > max {
                    text = text.chars().take(max).collect();
                }
                let obs = h.rt.block_on(page.observe()).map_err(berr)?;
                Ok(json!({"url": obs.url, "title": obs.title, "text": text, "epoch": obs.epoch, "nodes": serde_json::to_value(&obs.nodes).unwrap_or(Value::Null), "console_errors": obs.console_errors}))
            }),
            mk("browser.click", "Real click on a node ref or CSS selector.", Effect::Act, json!({"type": "object", "properties": target}), |h, a| {
                let t = parse_target(&a)?;
                h.rt.block_on(h.page()?.click(&t)).map_err(berr)?;
                Ok(json!({"clicked": true}))
            }),
            mk(
                "browser.type",
                "Type text with real key events; with a ref/selector the field is focused and (optionally) cleared first.",
                Effect::Act,
                json!({"type": "object", "properties": {"text": {"type": "string"}, "ref": {"type": "string"}, "selector": {"type": "string"}, "clear": {"type": "boolean"}}, "required": ["text"]}),
                |h, a| {
                    let page = h.page()?;
                    let text = a["text"].as_str().unwrap_or_default();
                    if a["ref"].is_string() || a["selector"].is_string() {
                        h.rt.block_on(page.fill(&parse_target(&a)?, text, a["clear"].as_bool().unwrap_or(true))).map_err(berr)?;
                    } else {
                        h.rt.block_on(page.type_text(text)).map_err(berr)?;
                    }
                    Ok(json!({"typed_chars": text.chars().count()}))
                },
            ),
            mk("browser.press", "Press a key chord, e.g. `Enter` or `ctrl+a`.", Effect::Act, json!({"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"]}), |h, a| {
                h.rt.block_on(h.page()?.press(a["key"].as_str().unwrap_or_default())).map_err(berr)?;
                Ok(json!({"pressed": true}))
            }),
            mk("browser.screenshot", "PNG screenshot of the current page to an absolute path.", Effect::Capture, json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}), |h, a| {
                let p = Path::new(a["path"].as_str().unwrap_or_default());
                if !p.is_absolute() {
                    return Err(ToolError::new(ERR_INVALID_ARGS, "`path` must be absolute"));
                }
                h.rt.block_on(h.page()?.screenshot_to(p)).map_err(berr)?;
                Ok(json!({"path": p.display().to_string(), "bytes": std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)}))
            }),
            mk("browser.console", "Console lines of the current page.", Effect::Observe, json!({"type": "object", "properties": {}}), |h, _| {
                let lines = h.rt.block_on(h.page()?.console());
                Ok(json!(lines.iter().map(|l| json!({"level": l.level, "text": l.text})).collect::<Vec<_>>()))
            }),
            mk("browser.close", "Close the browser and delete its temporary profile.", Effect::Launch, json!({"type": "object", "properties": {}}), |h, _| {
                h.shutdown();
                Ok(json!({"closed": true}))
            }),
        ]
    }
}

impl Drop for BrowserTools {
    fn drop(&mut self) {
        // Runtime is dropped after; block briefly on shutdown so Chrome never outlives us.
        let s = self.session.get_mut().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(s) = s {
            let _ = self.rt.block_on(async { tokio::time::timeout(Duration::from_secs(10), s.shutdown()).await });
        }
    }
}

fn parse_target(a: &Value) -> Result<Target, ToolError> {
    match (a["ref"].as_str(), a["selector"].as_str()) {
        (Some(r), _) => Ok(Target::node_ref(r)),
        (None, Some(s)) => Ok(Target::css(s)),
        _ => Err(ToolError::new(ERR_INVALID_ARGS, "give `ref` or `selector`")),
    }
}

struct WebTool {
    spec: ToolSpec,
    host: Arc<BrowserTools>,
    run: fn(&BrowserTools, Value) -> Result<Value, ToolError>,
}

impl Tool for WebTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn call(&self, _ctx: &CallCtx, args: Value) -> Result<Value, ToolError> {
        (self.run)(&self.host, args)
    }
}
