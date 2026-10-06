//! Chrome target (feature `browser`) through `rightkit-browser`: real CDP input,
//! headless by default, audio muted, temporary profile inside the run workspace.
use crate::scenario::params::{dur, pb, pf_or, ps, ps_opt};
use crate::scenario::Ctx;
use crate::util::{err, Error, Result};
use rightkit_browser::{BrowserPage, BrowserSession, LaunchOptions, Target};
use serde_json::{json, Map, Value};
use std::sync::Arc;

pub struct BrowserTarget {
    rt: tokio::runtime::Runtime,
    session: Option<BrowserSession>,
    page: Option<Arc<BrowserPage>>,
}

impl BrowserTarget {
    pub fn launch(headed: bool, viewport: (u32, u32)) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let mut opts = LaunchOptions {
            mute_audio: true,
            viewport,
            ..Default::default()
        };
        if headed {
            opts = opts.headed();
        }
        let session = rt
            .block_on(BrowserSession::launch(opts))
            .map_err(|e| Error(e.to_string()))?;
        let page = rt
            .block_on(session.new_page("about:blank"))
            .map_err(|e| Error(e.to_string()))?;
        Ok(Self {
            rt,
            session: Some(session),
            page: Some(page),
        })
    }

    pub fn stop(&mut self) {
        self.page.take();
        if let Some(s) = self.session.take() {
            drop(s);
        }
    }

    fn page(&self) -> Result<&Arc<BrowserPage>> {
        self.page
            .as_ref()
            .ok_or_else(|| Error("no browser launched; add a browser_launch step".into()))
    }
}

fn target(p: &Map<String, Value>) -> Result<Target> {
    Ok(Target::css(ps(p, "selector")?))
}

pub fn exec(ctx: &mut Ctx, kind: &str, p: &Map<String, Value>) -> Result<Value> {
    if kind == "browser_launch" {
        if ctx.browser.is_some() {
            return err("a browser is already running in this scenario");
        }
        let b = BrowserTarget::launch(
            pb(p, "headed"),
            (
                pf_or(p, "width", 1280.0) as u32,
                pf_or(p, "height", 800.0) as u32,
            ),
        )?;
        if let Some(url) = ps_opt(p, "url") {
            b.rt.block_on(b.page()?.goto(&url))
                .map_err(|e| Error(e.to_string()))?;
        }
        ctx.browser = Some(b);
        return Ok(json!({"launched": true}));
    }
    let shot_dir = ctx.log_dir.clone();
    let b = ctx
        .browser
        .as_ref()
        .ok_or_else(|| Error("no browser launched; add a browser_launch step".into()))?;
    let page = b.page()?.clone();
    let e = |e: rightkit_browser::BrowserError| Error(e.to_string());
    let out = match kind {
        "browser_goto" => {
            b.rt.block_on(page.goto(&ps(p, "url")?)).map_err(e)?;
            json!({"url": b.rt.block_on(page.url())})
        }
        "browser_click" => {
            b.rt.block_on(page.click(&target(p)?)).map_err(e)?;
            json!({"ok": true})
        }
        "browser_hover" => {
            b.rt.block_on(page.hover(&target(p)?)).map_err(e)?;
            json!({"ok": true})
        }
        "browser_fill" => {
            b.rt.block_on(page.fill(&target(p)?, &ps(p, "text")?, true))
                .map_err(e)?;
            json!({"ok": true})
        }
        "browser_press" => {
            b.rt.block_on(page.press(&ps(p, "key")?)).map_err(e)?;
            json!({"ok": true})
        }
        "browser_type" => {
            b.rt.block_on(page.type_text(&ps(p, "text")?)).map_err(e)?;
            json!({"ok": true})
        }
        "browser_eval" => b.rt.block_on(page.eval(&ps(p, "expr")?)).map_err(e)?,
        "browser_wait" => {
            b.rt.block_on(page.wait_for(&ps(p, "selector")?, dur(p, "timeout_s", 20.0)))
                .map_err(e)?;
            json!({"ok": true})
        }
        "browser_wait_eval" => {
            b.rt.block_on(page.wait_eval(&ps(p, "expr")?, dur(p, "timeout_s", 20.0)))
                .map_err(e)?;
            json!({"ok": true})
        }
        "browser_text" => json!({"text": b.rt.block_on(page.text()).map_err(e)?}),
        "browser_screenshot" => {
            let name = ps_opt(p, "name").unwrap_or_else(|| "browser".into());
            let path = shot_dir.join(format!("{name}.png"));
            b.rt.block_on(page.screenshot_to(&path)).map_err(e)?;
            let size = crate::media::png_size(&path)?;
            ctx.artifacts
                .push((format!("{}/{name}.png", ctx.scenario), path.clone()));
            json!({"path": path.to_string_lossy(), "size": size})
        }
        "browser_console" => {
            let lines = b.rt.block_on(page.console());
            json!({"count": lines.len(), "lines": format!("{lines:?}")})
        }
        "browser_stop" => {
            if let Some(mut b) = ctx.browser.take() {
                b.stop();
            }
            json!({"stopped": true})
        }
        other => return err(format!("unknown step kind '{other}'")),
    };
    Ok(out)
}
