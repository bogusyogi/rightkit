//! Chrome target (feature `browser`) through `rightkit-browser`: real CDP input,
//! headless by default, audio muted, temporary profile inside the run workspace.
//!
//! `rightkit-browser` blocks loopback by default (SSRF policy). A test that drives its own dev
//! server opts in per origin with [`allow_loopback_origin`] (or [`BrowserTarget::launch_for_origins`],
//! or the `allow_loopback` parameter of the `browser_launch` step): only that scheme, loopback host
//! and port are let through, through the policy's allow callback. Every other loopback port, private
//! range and the link-local/metadata hard block stay enforced; the policy is never made unrestricted.
use crate::scenario::params::{dur, pb, pf_or, ps, ps_opt, pv};
use crate::scenario::Ctx;
use crate::util::{err, Error, Result};
use rightkit_browser::{
    AddressClass, BrowserPage, BrowserRequest, BrowserSession, LaunchOptions, NetworkPolicy,
    PolicyDecision, Target,
};
use serde_json::{json, Map, Value};
use std::future::Future;
use std::sync::Arc;

/// One loopback origin a test may reach: `scheme://host:port`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackOrigin {
    /// `http` or `https`; the matching websocket scheme (`ws`/`wss`, dev-server HMR) is allowed too.
    pub scheme: String,
    /// Normalized loopback host: `localhost`, `*.localhost`, `127.x.y.z` or `::1`.
    pub host: String,
    pub port: u16,
}

impl LoopbackOrigin {
    /// Parse `http://127.0.0.1:5173` (a trailing path is ignored). The host must be loopback.
    pub fn parse(origin: &str) -> Result<Self> {
        let (scheme, rest) = origin
            .split_once("://")
            .ok_or_else(|| Error(format!("origin '{origin}' has no scheme")))?;
        let scheme = scheme.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => return err(format!("origin '{origin}': scheme must be http or https")),
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if authority.contains('@') {
            return err(format!("origin '{origin}' must not carry credentials"));
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (h, tail) = v6
                .split_once(']')
                .ok_or_else(|| Error(format!("origin '{origin}': unterminated IPv6 host")))?;
            (h.to_string(), tail.strip_prefix(':'))
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), Some(p)),
                None => (authority.to_string(), None),
            }
        };
        let port = match port {
            Some(p) => p
                .parse::<u16>()
                .map_err(|_| Error(format!("origin '{origin}': bad port '{p}'")))?,
            None => default_port,
        };
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let loopback = host == "localhost"
            || host.ends_with(".localhost")
            || host
                .parse::<std::net::IpAddr>()
                .map(|ip| ip.is_loopback())
                .unwrap_or(false);
        if !loopback {
            return err(format!(
                "origin '{origin}' is not a loopback host; only loopback dev servers can be allowed this way"
            ));
        }
        Ok(Self { scheme, host, port })
    }

    /// Whether a request the browser is about to make targets exactly this origin.
    pub fn matches(&self, r: &BrowserRequest) -> bool {
        let ws = if self.scheme == "https" { "wss" } else { "ws" };
        (r.scheme == self.scheme || r.scheme == ws)
            && r.host == self.host
            && r.port == Some(self.port)
            && r.class == Some(AddressClass::Loopback)
            && !r.addresses.is_empty()
            && r.addresses.iter().all(|ip| ip.is_loopback())
    }
}

/// `policy` plus an explicit allowance for one loopback origin. Requests to that origin are
/// allowed; everything else is decided by the previous allow callback (if any) and then the
/// policy's defaults, so several origins can be chained.
pub fn allow_loopback_origin(policy: NetworkPolicy, origin: &str) -> Result<NetworkPolicy> {
    let o = LoopbackOrigin::parse(origin)?;
    let prev = policy.allow_request.clone();
    Ok(policy.allow_with(move |r: &BrowserRequest| {
        if o.matches(r) {
            PolicyDecision::Allow
        } else {
            prev.as_ref()
                .map(|p| p(r))
                .unwrap_or(PolicyDecision::Default)
        }
    }))
}

pub struct BrowserTarget {
    rt: tokio::runtime::Runtime,
    session: Option<BrowserSession>,
    page: Option<Arc<BrowserPage>>,
}

impl BrowserTarget {
    /// Default policy (loopback blocked).
    pub fn launch(headed: bool, viewport: (u32, u32)) -> Result<Self> {
        Self::launch_for_origins(headed, viewport, &[])
    }

    /// Default policy plus an explicit allowance for each loopback `origins` entry
    /// (e.g. `["http://127.0.0.1:5173"]` for a Vite dev server).
    pub fn launch_for_origins(
        headed: bool,
        viewport: (u32, u32),
        origins: &[&str],
    ) -> Result<Self> {
        let mut network = NetworkPolicy::default();
        for o in origins {
            network = allow_loopback_origin(network, o)?;
        }
        let mut opts = LaunchOptions {
            mute_audio: true,
            viewport,
            network,
            ..Default::default()
        };
        if headed {
            opts = opts.headed();
        }
        Self::launch_with(opts)
    }

    /// Full control over launch options (profile, Chrome path, admission hook, policy).
    pub fn launch_with(opts: LaunchOptions) -> Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
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

    /// The page opened at launch (for Rust tests that drive `rightkit-browser` directly).
    pub fn page(&self) -> Result<&Arc<BrowserPage>> {
        self.page
            .as_ref()
            .ok_or_else(|| Error("no browser launched; add a browser_launch step".into()))
    }

    pub fn session(&self) -> Option<&BrowserSession> {
        self.session.as_ref()
    }

    /// Run a `rightkit-browser` future on this target's runtime.
    pub fn block_on<F: Future>(&self, f: F) -> F::Output {
        self.rt.block_on(f)
    }

    /// Navigate the launch page; a policy denial is an error naming the reason.
    pub fn goto(&self, url: &str) -> Result<()> {
        self.rt
            .block_on(self.page()?.goto(url))
            .map_err(|e| Error(e.to_string()))
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
        // `allow_loopback`: one origin string or an array of them (the app's own dev server).
        let origins: Vec<String> = match pv(p, "allow_loopback") {
            None => vec![],
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(String::from)
                        .ok_or_else(|| Error("allow_loopback entries must be strings".into()))
                })
                .collect::<Result<_>>()?,
            Some(_) => return err("allow_loopback must be an origin string or an array of them"),
        };
        let origins: Vec<&str> = origins.iter().map(String::as_str).collect();
        let b = BrowserTarget::launch_for_origins(
            pb(p, "headed"),
            (
                pf_or(p, "width", 1280.0) as u32,
                pf_or(p, "height", 800.0) as u32,
            ),
            &origins,
        )?;
        if let Some(url) = ps_opt(p, "url") {
            b.goto(&url)?;
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
