//! Scoped editing rules : `rules.set`, `rules.list` and `rules.resolve`.
//!
//! Rules persist under the `editing_rules` key of two settings files, each a plain JSON
//! object read and written through the shared settings store:
//!  - the project's `settings.json` (`store: "project"`, needs `dir`), and
//!  - the app settings file (`store: "app"`), where channel, content type and platform
//!    rules live.
//!
//! Every stored rule keeps its provenance (`set_by`, `set_at`). Resolution merges both files
//! into one [`crate::rules::RuleMatrix`]. Auto-edit takes its pad, max-pause and filler values
//! from [`tuned_config`], which applies the resolved values over the preset and reports which
//! rule (and which store) set each one, or that a same-rank conflict left the preset in place.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::cuts::{CutConfig, Preset as CutPreset};
use crate::editorial::EditorialConfig;
use crate::rules::{Context, Parameter, Resolution, Rule, RuleMatrix, Value as RuleValue};
use chrono::{SecondsFormat, Utc};
use rightkit_video_project::ProjectError;
use serde_json::{json, Map, Value};

use rightkit_engine_host::env::Env;
use rightkit_engine_host::project::{bad, open_settings, settings_object, settings_path};

/// Settings key that holds the stored rule list.
const KEY: &str = "editing_rules";

/// Parameters the auto-edit honours. Others (caption width, pace) are reported as unused.
const TUNABLE: [Parameter; 4] = [
    Parameter::PadBeforeMs,
    Parameter::PadAfterMs,
    Parameter::MaxPauseMs,
    Parameter::RemoveFillers,
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Store {
    Project,
    App,
}

impl Store {
    fn name(self) -> &'static str {
        match self {
            Store::Project => "project",
            Store::App => "app",
        }
    }
}

fn parse_store(s: &str) -> Result<Store, ProjectError> {
    match s {
        "project" => Ok(Store::Project),
        "app" => Ok(Store::App),
        other => Err(bad(format!("store must be project or app, not {other:?}"))),
    }
}

struct Stored {
    store: Store,
    rule: Rule,
}

/// Where a rule was stored and what it scopes to, for the report.
struct Origin {
    store: &'static str,
    scope: Value,
}

/// Resolution plus the origin of every rule that took part.
pub struct Resolved {
    resolution: Resolution,
    origins: BTreeMap<String, Origin>,
}

/// Auto-edit config with the resolved values applied, and the report of where each came from.
pub struct Tuned {
    pub cfg: EditorialConfig,
    pub report: Value,
}

fn store_path(store: Store, dir: Option<&Path>, env: &Env) -> Result<PathBuf, ProjectError> {
    match store {
        Store::Project => {
            settings_path(env, Some(dir.ok_or_else(|| bad("project rules need dir"))?))
        }
        Store::App => settings_path(env, None),
    }
}

fn read_rules(path: &Path) -> Result<Vec<Rule>, ProjectError> {
    let map = settings_object(path)?;
    match map.get(KEY) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| {
            bad(format!(
                "{}: {KEY} must be a list of rules: {e}",
                path.display()
            ))
        }),
    }
}

fn load_all(dir: Option<&Path>, env: &Env) -> Result<Vec<Stored>, ProjectError> {
    let mut out = Vec::new();
    if let Some(d) = dir {
        for rule in read_rules(&settings_path(env, Some(d))?)? {
            out.push(Stored {
                store: Store::Project,
                rule,
            });
        }
    }
    for rule in read_rules(&settings_path(env, None)?)? {
        out.push(Stored {
            store: Store::App,
            rule,
        });
    }
    Ok(out)
}

fn save_rules(path: &Path, rules: &[Rule]) -> Result<(), ProjectError> {
    let store = open_settings(path, true)?;
    store
        .patch(&json!({"editing_rules": rules}), None)
        .map_err(|e| bad(format!("saving {}: {e}", path.display())))?;
    Ok(())
}

fn param_name(p: Parameter) -> String {
    serde_json::to_value(p)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// `rules.set {store?, rule}`: inserts or replaces one rule by id. Provenance `set_by` is
/// required; `set_at` defaults to now. A project-scope rule defaults to the project file,
/// any other scope to the app file. An id already held by the other file is refused.
fn set_rule(
    dir: Option<&Path>,
    env: &Env,
    params: &Value,
    dry_run: bool,
) -> Result<Value, ProjectError> {
    let mut raw = params
        .get("rule")
        .cloned()
        .ok_or_else(|| bad("missing param: rule"))?;
    let scope_kind = raw
        .get("scope")
        .and_then(|s| s.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let default_store = if scope_kind == "project" {
        Store::Project
    } else {
        Store::App
    };
    let store = match params.get("store").and_then(Value::as_str) {
        Some(s) => parse_store(s)?,
        None => default_store,
    };
    let prov = raw
        .get_mut("provenance")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| bad("rule needs provenance with set_by"))?;
    prov.entry("set_at")
        .or_insert_with(|| json!(Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)));
    let rule: Rule = serde_json::from_value(raw).map_err(|e| bad(format!("invalid rule: {e}")))?;
    rule.validate().map_err(|e| bad(e.to_string()))?;

    let all = load_all(dir, env)?;
    if all.iter().any(|s| s.rule.id == rule.id && s.store != store) {
        return Err(bad(format!(
            "rule {} already exists in the other rules file",
            rule.id
        )));
    }
    let mut own: Vec<Rule> = all
        .iter()
        .filter(|s| s.store == store)
        .map(|s| s.rule.clone())
        .collect();
    let replaced = match own.iter().position(|r| r.id == rule.id) {
        Some(i) => {
            own[i] = rule.clone();
            true
        }
        None => {
            own.push(rule.clone());
            false
        }
    };
    // Validate the whole set the resolver will see, not just this file.
    let mut combined: Vec<Rule> = all
        .iter()
        .filter(|s| s.store != store)
        .map(|s| s.rule.clone())
        .collect();
    combined.extend(own.iter().cloned());
    RuleMatrix::from_rules(combined).map_err(|e| bad(e.to_string()))?;

    let path = store_path(store, dir, env)?;
    if dry_run {
        return Ok(json!({
            "dry_run": true,
            "would": {
                "command": "rules.set",
                "store": store.name(),
                "path": path.display().to_string(),
                "replaced": replaced,
                "rule": rule,
            },
        }));
    }
    save_rules(&path, &own)?;
    Ok(json!({
        "status": "saved",
        "store": store.name(),
        "path": path.display().to_string(),
        "replaced": replaced,
        "rule": rule,
    }))
}

/// What a user rejected in an auto-edit, reduced to the parameter it corrects.
pub enum Rejection {
    /// A pause cut the user found too aggressive. `duration_ms` is the rejected pause length.
    Pause { edit_id: String, duration_ms: i64 },
    /// A filler cut the user found too aggressive.
    Filler { edit_id: String },
}

/// Records one correction rule per rejection, so the next auto-edit on the same channel keeps
/// what was rejected. A pause rejection sets `max_pause_ms` to the pause length plus 50 ms. A
/// filler rejection turns `remove_fillers` off. The rule id is `correction-<edit id>`. Scope is
/// the channel in `context`, or the project when there is none. Provenance records
/// `set_by: "correction"`, source, edit id and time, retaining the readable `note`.
pub fn record_corrections(
    dir: &Path,
    env: &Env,
    context: Option<&Value>,
    rejections: &[Rejection],
) -> Result<(), ProjectError> {
    if rejections.is_empty() {
        return Ok(());
    }
    let channel = context
        .and_then(|c| c.get("channel"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    for r in rejections {
        let (edit_id, parameter, value) = match r {
            Rejection::Pause {
                edit_id,
                duration_ms,
            } => (
                edit_id,
                "max_pause_ms",
                json!({"type": "int", "value": duration_ms + 50}),
            ),
            Rejection::Filler { edit_id } => (
                edit_id,
                "remove_fillers",
                json!({"type": "bool", "value": false}),
            ),
        };
        let scope = match channel {
            Some(ch) => json!({"kind": "channel", "name": ch}),
            None => json!({
                "kind": "project",
                "name": dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
            }),
        };
        let rule = json!({
            "id": format!("correction-{edit_id}"),
            "scope": scope,
            "effect": {"parameter": parameter, "value": value},
            "provenance": {
                "set_by": "correction",
                "set_at": now,
                "note": format!("source=correction edit_id={edit_id}"),
                "source": "correction",
                "edit_id": edit_id,
            },
        });
        set_rule(Some(dir), env, &json!({"rule": rule}), false)?;
    }
    Ok(())
}

/// `rules.list {dir?}`: every stored rule with the store it lives in.
fn list_rules(dir: Option<&Path>, env: &Env) -> Result<Value, ProjectError> {
    let all = load_all(dir, env)?;
    Ok(json!({
        "rules": all
            .iter()
            .map(|s| json!({"store": s.store.name(), "rule": s.rule}))
            .collect::<Vec<_>>(),
        "project_path": dir
            .map(|d| settings_path(env, Some(d)))
            .transpose()?
            .map(|p| p.display().to_string()),
        "app_path": settings_path(env, None)?.display().to_string(),
    }))
}

/// Builds the editing context. `project` defaults to the project folder name.
pub fn context_from(raw: Option<&Value>, dir: Option<&Path>) -> Result<Context, ProjectError> {
    let mut map = match raw {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m.clone(),
        Some(_) => return Err(bad("context must be an object")),
    };
    if !map.contains_key("project") {
        let name = dir
            .and_then(Path::file_name)
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        map.insert("project".into(), json!(name));
    }
    serde_json::from_value(Value::Object(map)).map_err(|e| bad(format!("invalid context: {e}")))
}

/// Resolves every stored rule (project and app) for `ctx`.
pub fn resolve_for(dir: Option<&Path>, env: &Env, ctx: &Context) -> Result<Resolved, ProjectError> {
    let all = load_all(dir, env)?;
    let origins = all
        .iter()
        .map(|s| {
            (
                s.rule.id.clone(),
                Origin {
                    store: s.store.name(),
                    scope: json!(s.rule.scope),
                },
            )
        })
        .collect();
    let matrix = RuleMatrix::from_rules(all.iter().map(|s| s.rule.clone()).collect())
        .map_err(|e| bad(e.to_string()))?;
    Ok(Resolved {
        resolution: matrix.resolve(ctx),
        origins,
    })
}

fn resolution_json(r: &Resolved) -> Value {
    let res = &r.resolution;
    let origin = |id: &str| r.origins.get(id);
    let effective: Map<String, Value> = res
        .effective
        .iter()
        .map(|(p, e)| {
            let o = origin(e.rule_id.as_str());
            (
                param_name(*p),
                json!({
                    "value": e.value,
                    "rule_id": e.rule_id,
                    "store": o.map(|o| o.store),
                    "scope": o.map(|o| o.scope.clone()),
                    "scope_rank": e.scope_rank,
                    "priority": e.priority,
                }),
            )
        })
        .collect();
    let conflicts: Vec<Value> = res
        .conflicts
        .iter()
        .map(|c| {
            json!({
                "parameter": param_name(c.parameter),
                "scope_rank": c.scope_rank,
                "priority": c.priority,
                "rule_ids": c.rule_ids,
                "values": c.values,
                "stores": c.rule_ids.iter().map(|id| origin(id.as_str()).map(|o| o.store)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let overridden: Vec<Value> = res
        .overridden
        .iter()
        .map(|o| json!({"parameter": param_name(o.parameter), "rule_id": o.rule_id, "by": o.by}))
        .collect();
    json!({
        "effective": effective,
        "conflicts": conflicts,
        "overridden": overridden,
    })
}

/// `rules.resolve {dir?, context}`.
fn resolve_method(dir: Option<&Path>, env: &Env, params: &Value) -> Result<Value, ProjectError> {
    let ctx = context_from(params.get("context"), dir)?;
    let r = resolve_for(dir, env, &ctx)?;
    let mut out = resolution_json(&r);
    out["context"] = json!(ctx);
    Ok(out)
}

fn cut_value(cuts: &CutConfig, p: Parameter) -> Option<RuleValue> {
    Some(match p {
        Parameter::PadBeforeMs => RuleValue::Int(cuts.attack_pad_ms),
        Parameter::PadAfterMs => RuleValue::Int(cuts.release_pad_ms),
        Parameter::MaxPauseMs => RuleValue::Int(cuts.max_pause_ms),
        Parameter::RemoveFillers => RuleValue::Bool(cuts.remove_fillers),
        _ => return None,
    })
}

fn apply_value(cuts: &mut CutConfig, p: Parameter, v: &RuleValue) {
    match (p, v) {
        (Parameter::PadBeforeMs, RuleValue::Int(n)) => cuts.attack_pad_ms = *n,
        (Parameter::PadAfterMs, RuleValue::Int(n)) => cuts.release_pad_ms = *n,
        (Parameter::MaxPauseMs, RuleValue::Int(n)) => cuts.max_pause_ms = *n,
        (Parameter::RemoveFillers, RuleValue::Bool(b)) => cuts.remove_fillers = *b,
        _ => {}
    }
}

/// The preset's editorial config with resolved rule values applied over it. A parameter
/// with a same-rank conflict keeps the preset value and is reported as `preset_conflict`.
pub fn tuned_config(
    preset: CutPreset,
    dir: &Path,
    env: &Env,
    context: Option<&Value>,
) -> Result<Tuned, ProjectError> {
    let mut cfg = EditorialConfig::for_preset(preset);
    let ctx = context_from(context, Some(dir))?;
    let r = resolve_for(Some(dir), env, &ctx)?;
    let res = &r.resolution;
    let mut values = Map::new();
    for p in TUNABLE {
        let preset_value = cut_value(&cfg.cuts, p);
        let entry = match res.effective.get(&p) {
            Some(e) => {
                apply_value(&mut cfg.cuts, p, &e.value);
                let o = r.origins.get(e.rule_id.as_str());
                json!({
                    "value": e.value,
                    "source": "rule",
                    "rule_id": e.rule_id,
                    "store": o.map(|o| o.store),
                    "scope": o.map(|o| o.scope.clone()),
                })
            }
            None => {
                let source = if res.conflicts.iter().any(|c| c.parameter == p) {
                    "preset_conflict"
                } else {
                    "preset"
                };
                json!({"value": preset_value, "source": source})
            }
        };
        values.insert(param_name(p), entry);
    }
    let not_used: Vec<Value> = res
        .effective
        .iter()
        .filter(|(p, _)| !TUNABLE.contains(*p))
        .map(|(p, e)| json!({"parameter": param_name(*p), "rule_id": e.rule_id}))
        .collect();
    let mut report = resolution_json(&r);
    report["context"] = json!(ctx);
    report["values"] = Value::Object(values);
    report["not_used"] = json!(not_used);
    Ok(Tuned { cfg, report })
}

/// Dispatch for the three rules methods. `dir` is optional for every one of them.
pub fn handle(
    method: &str,
    params: &Value,
    env: &Env,
    dry_run: bool,
) -> Result<Value, ProjectError> {
    let dir_buf = params.get("dir").and_then(Value::as_str).map(PathBuf::from);
    let dir = dir_buf.as_deref();
    match method {
        "rules.set" => set_rule(dir, env, params, dry_run),
        "rules.list" => list_rules(dir, env),
        "rules.resolve" => resolve_method(dir, env, params),
        other => Err(bad(format!("unknown rules method {other}"))),
    }
}
