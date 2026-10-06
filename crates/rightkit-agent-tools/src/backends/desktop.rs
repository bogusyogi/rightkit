//! `desktop.*` tools over `rightkit-automation`: observe and act on any
//! allowlisted app (macOS AX, Windows UIA) through the automation engine's own
//! gates (allowlist, kill switch, lease fence, freshness, secure-field and
//! credential gates, admission hook, tier ladder). This module adds only the
//! typed JSON surface; it deliberately exposes no text *delivery*, no clipboard
//! and no dictation entry points (HeardRight's rules, see `exposure`).

use crate::spec::*;
use rightkit_automation::{
    Action, ActOptions, Automation, Button, Error as AutoError, Fence, Guard, LeaseRequest, Limits, Node, Platform, RiskClass, Target, Tier,
};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

fn err(e: AutoError) -> ToolError {
    let code = match &e {
        AutoError::PermissionDenied(_) => "permission_denied",
        AutoError::NotFound(_) => "not_found",
        AutoError::Blocked(_) => "blocked",
        AutoError::NeedsConfirmation(_) => "needs_confirmation",
        AutoError::UnknownSnapshot(_) => "unknown_snapshot",
        AutoError::NoTier(_) => "no_tier",
        AutoError::FocusMoved { .. } => "focus_moved",
        AutoError::Invalid(_) => ERR_INVALID_ARGS,
        AutoError::Denied(_) => ERR_DENIED,
        AutoError::LeaseHeld { .. } => "lease_held",
        AutoError::StaleLease { .. } => "stale_lease",
        AutoError::OutOfOrder { .. } => "out_of_order",
        AutoError::Replayed { .. } => "replayed",
        _ => "automation_error",
    };
    ToolError::new(code, e.to_string())
}

fn tier(s: Option<&str>) -> Result<Tier, ToolError> {
    match s {
        None | Some("targeted") => Ok(Tier::Targeted),
        Some("semantic") => Ok(Tier::Semantic),
        Some("in_app") => Ok(Tier::InApp),
        // Foreground input is the explicit opt-in the engine requires; agents
        // may not request it through this interface.
        Some("foreground") => Err(ToolError::new(ERR_NOT_ALLOWED, "foreground input is not available through agent tools")),
        Some(o) => Err(ToolError::new(ERR_INVALID_ARGS, format!("unknown tier `{o}`"))),
    }
}

fn risk(s: &str) -> Option<RiskClass> {
    Some(match s {
        "delete" => RiskClass::Delete,
        "send" => RiskClass::Send,
        "purchase" => RiskClass::Purchase,
        "close" => RiskClass::Close,
        _ => return None,
    })
}

fn node_json(n: &Node) -> Value {
    json!({
        "id": n.id, "role": n.role, "name": n.name, "value": if n.secure { None } else { n.value.clone() },
        "enabled": n.enabled, "secure": n.secure, "caps": n.caps,
        "frame": n.frame.map(|f| json!({"x": f.x, "y": f.y, "w": f.w, "h": f.h})),
        "children": n.children.iter().map(node_json).collect::<Vec<_>>(),
    })
}

fn action(a: &Value) -> Result<Action, ToolError> {
    let s = |k: &str| a[k].as_str().map(str::to_string).ok_or_else(|| ToolError::new(ERR_INVALID_ARGS, format!("action.{k} is required")));
    Ok(match a["kind"].as_str().unwrap_or_default() {
        "press" => Action::Press,
        "focus" => Action::Focus,
        "set_value" => Action::SetValue(s("text")?),
        "type_text" => Action::TypeText(s("text")?),
        "keys" => Action::Keys(s("keys")?),
        "click" => Action::Click {
            point: match (a["x"].as_f64(), a["y"].as_f64()) {
                (Some(x), Some(y)) => Some((x, y)),
                _ => None,
            },
            button: if a["button"] == "right" { Button::Right } else { Button::Left },
            count: a["count"].as_u64().unwrap_or(1) as u32,
        },
        "scroll" => Action::Scroll { dx: a["dx"].as_f64().unwrap_or(0.0), dy: a["dy"].as_f64().unwrap_or(0.0) },
        "menu_press" => Action::MenuPress(a["path"].as_array().map(|v| v.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default()),
        other => return Err(ToolError::new(ERR_INVALID_ARGS, format!("unknown action kind `{other}`"))),
    })
}

struct FnTool<P: Platform> {
    spec: ToolSpec,
    auto: Arc<Automation<P>>,
    run: fn(&Automation<P>, Value) -> Result<Value, ToolError>,
}

impl<P: Platform> Tool for FnTool<P> {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }
    fn call(&self, _ctx: &CallCtx, args: Value) -> Result<Value, ToolError> {
        (self.run)(&self.auto, args)
    }
}

fn pid(a: &Value) -> Result<i32, ToolError> {
    a["pid"].as_i64().map(|p| p as i32).ok_or_else(|| ToolError::new(ERR_INVALID_ARGS, "`pid` is required"))
}

fn target(a: &Value) -> Result<Target, ToolError> {
    let p = pid(a)?;
    Ok(match a["window_id"].as_u64() {
        Some(w) => Target::window(p, w),
        None => Target::app(p),
    })
}

/// `desktop.*` tools over one automation engine (native, or the mock in tests).
pub fn desktop_tools<P: Platform>(auto: Arc<Automation<P>>) -> Vec<Arc<dyn Tool>> {
    let mk = |name: &str, desc: &str, effect: Effect, schema: Value, run: fn(&Automation<P>, Value) -> Result<Value, ToolError>| -> Arc<dyn Tool> {
        Arc::new(FnTool { spec: ToolSpec::new(name, desc, effect, schema), auto: auto.clone(), run })
    };
    let pid_only = json!({"type": "object", "properties": {"pid": {"type": "integer"}}, "required": ["pid"]});
    vec![
        mk("desktop.status", "Permissions and platform of the automation backend.", Effect::Observe, json!({"type": "object", "properties": {}}), |a, _| {
            let s = a.status();
            Ok(json!({"platform": format!("{:?}", s.platform), "accessibility": s.accessibility, "screen_capture": s.screen_capture, "notes": s.notes, "allowed_apps": a.policy().allowed()}))
        }),
        mk("desktop.apps", "Running GUI apps (pid, app id, name).", Effect::Observe, json!({"type": "object", "properties": {}}), |a, _| {
            Ok(json!(a.apps().into_iter().map(|x| json!({"pid": x.pid, "app_id": x.app_id, "name": x.name})).collect::<Vec<_>>()))
        }),
        mk(
            "desktop.windows",
            "Windows (id, owner, title, bounds).",
            Effect::Observe,
            json!({"type": "object", "properties": {"on_screen_only": {"type": "boolean"}}}),
            |a, args| {
                let on = args["on_screen_only"].as_bool().unwrap_or(true);
                Ok(json!(a
                    .windows(on)
                    .into_iter()
                    .map(|w| json!({"window_id": w.window_id, "pid": w.pid, "owner": w.owner, "title": w.title, "on_screen": w.on_screen}))
                    .collect::<Vec<_>>()))
            },
        ),
        mk(
            "desktop.snapshot",
            "Accessibility snapshot with stable element ids. Returns the snapshot id, a rendered outline and the node tree. Secure fields carry no value.",
            Effect::Observe,
            json!({"type": "object", "properties": {"pid": {"type": "integer"}, "window_id": {"type": "integer"}, "max_depth": {"type": "integer"}, "max_nodes": {"type": "integer"}}, "required": ["pid"]}),
            |a, args| {
                let d = Limits::default();
                let lim = Limits { max_depth: args["max_depth"].as_u64().map(|v| v as usize).unwrap_or(d.max_depth), max_nodes: args["max_nodes"].as_u64().map(|v| v as usize).unwrap_or(d.max_nodes) };
                let s = a.snapshot(target(&args)?, lim).map_err(err)?;
                let mut outline = String::new();
                s.root.render(&mut outline, 0);
                Ok(json!({"snapshot": s.id, "pid": s.pid, "app_id": s.app_id, "truncated": s.truncated, "focused": s.focused, "nodes": s.root.count(), "outline": outline, "tree": node_json(&s.root)}))
            },
        ),
        mk(
            "desktop.act",
            "Act on an element of a snapshot through the tier ladder (semantic, in_app, targeted). Returns the receipt (tier used, gate that refused). Risky actions need `confirm`.",
            Effect::Act,
            json!({"type": "object", "properties": {
                "snapshot": {"type": "string"}, "element": {"type": "string"},
                "action": {"type": "object", "description": "{kind: press|focus|set_value|type_text|keys|click|scroll|menu_press, text?, keys?, x?, y?, button?, count?, dx?, dy?, path?}"},
                "max_tier": {"type": "string", "enum": ["semantic", "in_app", "targeted"]},
                "confirm": {"type": "array"}, "guard": {"type": "string", "enum": ["structure", "element"]},
                "fence": {"type": "object", "description": "{epoch, seq} lease fence"}
            }, "required": ["snapshot", "action"]}),
            |a, args| {
                let act = action(&args["action"])?;
                let mut o = ActOptions { max_tier: tier(args["max_tier"].as_str())?, ..ActOptions::default() };
                o.guard = if args["guard"] == "element" { Guard::Element } else { Guard::Structure };
                o.confirm = args["confirm"].as_array().map(|v| v.iter().filter_map(|x| x.as_str().and_then(risk)).collect()).unwrap_or_default();
                if let (Some(e), Some(s)) = (args["fence"]["epoch"].as_u64(), args["fence"]["seq"].as_u64()) {
                    o.fence = Some(Fence { epoch: e, seq: s });
                }
                let out = a.act(args["snapshot"].as_str().unwrap_or_default(), args["element"].as_str(), act, &o).map_err(err)?;
                let receipt: Value = serde_json::from_str(&out.receipt.to_json()).unwrap_or(Value::Null);
                Ok(json!({"done": out.done(), "receipt": receipt, "fresh_snapshot": out.fresh.as_ref().map(|s| s.id.clone())}))
            },
        ),
        mk(
            "desktop.screenshot",
            "Window screenshot with secure and credential fields masked; `path` must be absolute.",
            Effect::Capture,
            json!({"type": "object", "properties": {"pid": {"type": "integer"}, "window_id": {"type": "integer"}, "path": {"type": "string"}}, "required": ["pid", "path"]}),
            |a, args| {
                let r = a.screenshot_report(&target(&args)?, Path::new(args["path"].as_str().unwrap_or_default())).map_err(err)?;
                Ok(json!({"window_id": r.window_id, "width": r.width, "height": r.height, "masked": r.masked, "path": args["path"]}))
            },
        ),
        mk("desktop.launch", "Launch an allowlisted app without activating it.", Effect::Launch, json!({"type": "object", "properties": {"spec": {"type": "string"}}, "required": ["spec"]}), |a, args| {
            let app = a.launch(args["spec"].as_str().unwrap_or_default()).map_err(err)?;
            Ok(json!({"pid": app.pid, "app_id": app.app_id, "name": app.name}))
        }),
        mk("desktop.terminate", "Quit an allowlisted app.", Effect::Launch, pid_only.clone(), |a, args| {
            a.terminate(pid(&args)?).map_err(err)?;
            Ok(json!({"terminated": true}))
        }),
        mk(
            "desktop.lease_acquire",
            "Take the control lease on an app. Returns the epoch; every later act must carry fence {epoch, seq} starting at seq 1. A newer lease fences the older controller.",
            Effect::Act,
            json!({"type": "object", "properties": {"pid": {"type": "integer"}, "holder": {"type": "string"}, "ttl_ms": {"type": "integer"}, "takeover": {"type": "boolean"}}, "required": ["pid", "holder"]}),
            |a, args| {
                let mut req = LeaseRequest::new(args["holder"].as_str().unwrap_or_default());
                if let Some(ms) = args["ttl_ms"].as_u64() {
                    req = req.ttl(Duration::from_millis(ms));
                }
                if args["takeover"] == true {
                    req = req.takeover();
                }
                let l = a.acquire_lease(pid(&args)?, req).map_err(err)?;
                Ok(json!({"pid": l.pid, "holder": l.holder, "epoch": l.epoch}))
            },
        ),
        mk(
            "desktop.lease_status",
            "Current lease on an app: holder, epoch, highest acknowledged input sequence (resend from acked_seq + 1).",
            Effect::Observe,
            pid_only.clone(),
            |a, args| {
                Ok(match a.lease_status(pid(&args)?) {
                    Some(s) => json!({"leased": true, "holder": s.holder, "epoch": s.epoch, "acked_seq": s.acked_seq, "expires_in_ms": s.expires_in_ms}),
                    None => json!({"leased": false}),
                })
            },
        ),
        mk(
            "desktop.lease_release",
            "Release a lease you hold.",
            Effect::Act,
            json!({"type": "object", "properties": {"pid": {"type": "integer"}, "holder": {"type": "string"}, "epoch": {"type": "integer"}}, "required": ["pid", "holder", "epoch"]}),
            |a, args| {
                let l = rightkit_automation::Lease { pid: pid(&args)?, holder: args["holder"].as_str().unwrap_or_default().into(), epoch: args["epoch"].as_u64().unwrap_or(0) };
                Ok(json!({"released": a.release_lease(&l)}))
            },
        ),
    ]
}
