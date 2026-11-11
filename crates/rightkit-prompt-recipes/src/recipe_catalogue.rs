//! studio.recipes.catalogue / studio.recipes.catalogue_validate: the recipe catalogue as declarative
//! graphs (DATA), with a loader that computes served status and a validator.
//!
//! This module is not an executor. A graph runs only through the existing pipeline runner
//! (`studio.pipelines.step_begin` and its siblings in pipeline_workflows.rs). Nothing here calls a
//! provider, spends, or writes a run.
//!
//! Plan: the G11 execution-plan item. G11 asks for recipes as
//! declarative graphs with typed inputs, nodes that name a served route or `ask`, `depends_on`,
//! output bindings, a review requirement, and a catalogue that is data, not code. The plan names
//! the recipes listed in data/recipes/catalogue/_index.json. Three have existing node content in the
//! repository and keep it: motion_transfer and object_replacement (G2 pair, templates.json) and
//! vox_director (pipeline_workflows.rs). Nine more are seeded as structure only (kinds, inputs and
//! wiring; every node on route "ask", prompts supplied as inputs, no invented prompt text). The rest
//! are listed as named, not seeded, with the reason in the index.
//!
//! ## Format (one file per graph under data/recipes/catalogue/, embedded at build time)
//!
//! ```text
//! { "id", "title", "source", "inputs": [{name, type}], "nodes": [...], "outputs": [{name, from}],
//!   "review": {required, policy} }
//! node: { id, kind, route, route_alternatives?, params, depends_on }
//!   kind   an engine modality (the engine media CLI modality list) or "native" (then "output"
//!          is one of image|video|audio|text|data, and "native" names the engine action)
//!   route  {provider, model} to prefer, or "ask"
//!   params a literal, {"input": name} or {"node": id}; a reference may carry "type"
//!   depends_on  node ids; every referenced node must be listed here
//! input type: image|video|audio|text|number|boolean
//! ```
//!
//! A reference's `"type"` is required when the route is unserved, because the catalogue cannot type
//! an input the served entry does not declare. With a served route the catalogue role decides the
//! type (start/end/reference/mask → image, video → video, audio → audio; String → text; Number and
//! Integer → number; Boolean → boolean). A stored `status` or `served` field is refused: served
//! status is computed on each load.
//!
//! ## Served status (computed, never stored)
//!
//! A route is `served` when the engine's provider registry (`rightkit_media_gen::providers::registry()`,
//! the list `media modalities` and `models` read) lists the provider, model and kind. Built-in
//! provider models count, so a route the engine serves is never reported unserved.
//!
//! The declared catalogue entry (`Catalogue::get()`: models.json plus the user override) is used only
//! for param checks: unknown params, missing required params, and the input roles that type a
//! reference. A served node with no declared entry reports `params_checked: false` and is not
//! failed; its references must then declare `"type"`. Each unserved node reports its preferred route
//! and `route_alternatives` as `missing_routes`.
//!
//! ## Validation (typed errors, each naming the node)
//!
//! `cycle`, `dangling_reference`, `undeclared_dependency`, `type_mismatch`, `untyped_reference`,
//! `unknown_kind`, `unknown_output_type`, `unknown_param` (declared entry only),
//! `missing_required_param` (declared entry only), `invalid_route`, `unknown_input_type`,
//! `duplicate_node`, `stored_status`, `invalid_graph`.
//!
//! ## What "on RightKit job primitives" would require (gap, for the lead to report)
//!
//! G11 says recipes "are declarative graphs executed through RightKit job primitives (dependencies,
//! output bindings, fan-out, duration handling, collection)". The published crate
//! rightkit-job-queue (0.3.1, as read at 0.2.9) has none of the graph-level pieces:
//!
//! - No inter-job dependency. The `Job` record (`src/store.rs`)
//!   holds `job_id`, `request_id`, `kind`, an opaque `request` JSON, `status`, `attempts`, `error`,
//!   `result` and ack fields. It has no parent, predecessor or depends-on field. A grep of
//!   rightkit-job-queue/src for depend, predecessor, fan-out and collection finds nothing relevant.
//! - Admission is by single job id, FIFO, under a limit. `StagedPool::enqueue(&self, job_id)`
//!   (`src/pool.rs`) takes one id; the dispatcher admits "queued
//!   job ids in FIFO order" (pool.rs:6, dispatch at pool.rs:485). There is no readiness predicate
//!   beyond the admission limit.
//! - Per-job outcome only. `Staged::{Done, Compute, Handled}` (pool.rs:53-61) describes what one
//!   job's `prepare` decided. No job can wait on
//!   another job's result, and nothing binds one job's output into another's request.
//! - No fan-out or collection. One node producing N jobs, or N results merged into one, has no
//!   primitive. Scheduling is per job: `Priority::{Interactive, Batch}` (sched.rs:19) and step
//!   estimates live in the pipeline runner, not in rightkit-job-queue.
//!
//! Until those exist, the pipeline runner (pipeline_workflows.rs `step_begin`, line ~1555, which
//! returns a submission handoff that the mounted panel settles) executes graphs. Building a second
//! executor here, or claiming RightKit job primitives today, would be wrong.
//!
//! Commands: `studio.recipes.catalogue` (list with per-node served status and missing routes) and
//! `studio.recipes.catalogue_validate` (`{id}` for a seeded graph, `{graph}` for an inline one).

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use rightkit_media_gen::catalogue::{Catalogue, Entry, InputType, Role};
use serde_json::{json, Map, Value};

use crate::{RecipeError, RecipeResult};
use rightkit_media_gen::Context;

pub const COMMANDS: &[&str] = &[
    "studio.recipes.catalogue",
    "studio.recipes.catalogue_validate",
];

const SEEDED_SRC: &[(&str, &str)] = &[
    (
        "motion_transfer",
        include_str!("../data/recipes/catalogue/motion_transfer.json"),
    ),
    (
        "object_replacement",
        include_str!("../data/recipes/catalogue/object_replacement.json"),
    ),
    (
        "vox_director",
        include_str!("../data/recipes/catalogue/vox_director.json"),
    ),
    (
        "character_sheet_environment",
        include_str!("../data/recipes/catalogue/character_sheet_environment.json"),
    ),
    (
        "studio_packshot",
        include_str!("../data/recipes/catalogue/studio_packshot.json"),
    ),
    (
        "product_recreate_recompose",
        include_str!("../data/recipes/catalogue/product_recreate_recompose.json"),
    ),
    (
        "paper_cut_motion",
        include_str!("../data/recipes/catalogue/paper_cut_motion.json"),
    ),
    (
        "claymation",
        include_str!("../data/recipes/catalogue/claymation.json"),
    ),
    (
        "talking_object",
        include_str!("../data/recipes/catalogue/talking_object.json"),
    ),
    (
        "singing_ad",
        include_str!("../data/recipes/catalogue/singing_ad.json"),
    ),
    (
        "ugc_character_review_15s",
        include_str!("../data/recipes/catalogue/ugc_character_review_15s.json"),
    ),
    (
        "tiktok_shop_pitch_18s",
        include_str!("../data/recipes/catalogue/tiktok_shop_pitch_18s.json"),
    ),
];
const INDEX_SRC: &str = include_str!("../data/recipes/catalogue/_index.json");

/// The engine modality kinds, mirroring the engine media CLI modality list.
const ENGINE_KINDS: &[&str] = &[
    "image",
    "video",
    "voice",
    "avatar",
    "music",
    "sfx",
    "upscale",
    "lipsync",
    "text",
    "extend",
    "motion_control",
    "replace_object",
    "restyle",
    "transcribe",
];
const NATIVE_KIND: &str = "native";
const INPUT_TYPES: &[&str] = &["image", "video", "audio", "text", "number", "boolean"];
const OUTPUT_TYPES: &[&str] = &["image", "video", "audio", "text", "data"];

fn seeded() -> &'static Vec<(String, Value)> {
    static SEEDED: OnceLock<Vec<(String, Value)>> = OnceLock::new();
    SEEDED.get_or_init(|| {
        SEEDED_SRC
            .iter()
            .map(|(id, src)| {
                let graph = serde_json::from_str(src)
                    .unwrap_or_else(|e| panic!("embedded recipe graph {id}: {e}"));
                (id.to_string(), graph)
            })
            .collect()
    })
}

fn index() -> &'static Value {
    static INDEX: OnceLock<Value> = OnceLock::new();
    INDEX.get_or_init(|| {
        serde_json::from_str(INDEX_SRC).unwrap_or_else(|e| panic!("embedded recipe index: {e}"))
    })
}

pub fn handle(_ctx: &Context, name: &str, input: &Value) -> RecipeResult<Value> {
    match name {
        "studio.recipes.catalogue" => Ok(catalogue()),
        "studio.recipes.catalogue_validate" => validate_command(input),
        other => Err(RecipeError::new(
            "unknown_command",
            format!("not a recipe catalogue command: {other}"),
        )),
    }
}

fn validate_command(input: &Value) -> RecipeResult<Value> {
    if let Some(id) = input.get("id").and_then(Value::as_str) {
        let (_, graph) = seeded()
            .iter()
            .find(|(k, _)| k.as_str() == id)
            .ok_or_else(|| {
                RecipeError::new("not_found", format!("no seeded recipe graph {id:?}"))
            })?;
        return Ok(validate(graph));
    }
    match input.get("graph") {
        Some(graph) if graph.is_object() => Ok(validate(graph)),
        _ => Err(RecipeError::new(
            "invalid_input",
            "pass {id} for a seeded graph or {graph} for an inline graph",
        )),
    }
}

/// `studio.recipes.catalogue`: every seeded graph with its validation, per-node served status and
/// missing routes, plus the plan's named list (seeded or not, and why).
fn catalogue() -> Value {
    let idx = index();
    let graphs: Vec<Value> = seeded()
        .iter()
        .map(|(gid, graph)| {
            let v = validate(graph);
            let mut missing: Vec<Value> = Vec::new();
            for n in arr(&v, "nodes") {
                for m in arr(n, "missing_routes") {
                    let mut m = m.clone();
                    m["node"] = n["id"].clone();
                    missing.push(m);
                }
            }
            json!({
                "id": gid,
                "title": graph["title"],
                "source": graph["source"],
                "inputs": graph["inputs"],
                "outputs": graph["outputs"],
                "review": graph["review"],
                "valid": v["ok"],
                "errors": v["errors"],
                "nodes": v["nodes"],
                "missing_routes": missing,
            })
        })
        .collect();
    json!({"ok": true, "graphs": graphs, "plan": idx["plan"], "plan_named": idx["plan_named"],
           "pipeline_templates_not_named_by_plan": idx["pipeline_templates_not_named_by_plan"]})
}

/// Validate one graph. Errors name the node they concern; `ok` is true only with no errors.
fn validate(graph: &Value) -> Value {
    let id = s(graph, "id").to_string();
    if !graph.is_object() {
        return report(
            &id,
            vec![issue("invalid_graph", None, "a graph is a JSON object")],
            Vec::new(),
        );
    }
    let mut errs: Vec<Value> = Vec::new();

    let mut input_types: HashMap<String, String> = HashMap::new();
    for inp in arr(graph, "inputs") {
        let (name, ty) = (s(inp, "name"), s(inp, "type"));
        if name.is_empty() || !INPUT_TYPES.contains(&ty) {
            errs.push(issue(
                "unknown_input_type",
                None,
                format!(
                    "input {:?} has type {:?}; allowed: {:?}",
                    name, ty, INPUT_TYPES
                ),
            ));
        }
        input_types.insert(name.to_string(), ty.to_string());
    }

    // Pass 1: ids, kinds and output types.
    let mut by_id: HashMap<&str, &Value> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    let mut out_type: HashMap<String, Option<String>> = HashMap::new();
    for n in arr(graph, "nodes") {
        let nid = s(n, "id");
        if nid.is_empty() {
            errs.push(issue("invalid_graph", None, "a node has no id"));
            continue;
        }
        if by_id.contains_key(nid) {
            errs.push(issue(
                "duplicate_node",
                Some(nid),
                "node id is declared twice",
            ));
            continue;
        }
        by_id.insert(nid, n);
        order.push(nid.to_string());
        for k in ["status", "served"] {
            if !n[k].is_null() {
                errs.push(issue(
                    "stored_status",
                    Some(nid),
                    format!("{:?} is computed on load and must not be stored", k),
                ));
            }
        }
        let kind = s(n, "kind");
        let ty: Option<String> = if kind == NATIVE_KIND {
            let o = s(n, "output");
            if OUTPUT_TYPES.contains(&o) {
                Some(o.to_string())
            } else {
                errs.push(issue(
                    "unknown_output_type",
                    Some(nid),
                    format!("native output {:?} is not one of {:?}", o, OUTPUT_TYPES),
                ));
                None
            }
        } else if ENGINE_KINDS.contains(&kind) {
            kind_output(kind).map(str::to_string)
        } else {
            errs.push(issue(
                "unknown_kind",
                Some(nid),
                format!("kind {:?} is neither an engine kind nor native", kind),
            ));
            None
        };
        out_type.insert(nid.to_string(), ty);
    }

    // Pass 2: routes, dependencies, params and reference types.
    let mut statuses: Vec<Value> = Vec::new();
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for nid_s in &order {
        let nid = nid_s.as_str();
        let n = by_id[nid];
        let kind = s(n, "kind");
        // Declared catalogue entry: used only for param names and required params. Built-in models
        // without one are not param-checked (`params_checked: false`); they are not failed.
        let entry: Option<&'static Entry> = match route_of(n) {
            Some((p, m)) if kind != NATIVE_KIND => declared_entry(&p, &m, kind),
            _ => None,
        };
        if kind != NATIVE_KIND
            && ENGINE_KINDS.contains(&kind)
            && n["route"].as_str() != Some("ask")
            && route_of(n).is_none()
        {
            errs.push(issue(
                "invalid_route",
                Some(nid),
                "route must be {provider, model} or \"ask\"",
            ));
        }

        let mut deps: Vec<String> = Vec::new();
        for d in arr(n, "depends_on") {
            let d = d.as_str().unwrap_or("");
            if out_type.contains_key(d) {
                deps.push(d.to_string());
            } else {
                errs.push(issue(
                    "dangling_reference",
                    Some(nid),
                    format!("depends_on names {:?}, which is not a node", d),
                ));
            }
        }
        let declared: HashSet<&str> = arr(n, "depends_on")
            .iter()
            .filter_map(Value::as_str)
            .collect();

        let empty = Map::new();
        let params = n["params"].as_object().unwrap_or(&empty);
        let mut given: HashSet<String> = HashSet::new();
        for (key, val) in params {
            given.insert(key.clone());
            let accepted = entry.and_then(|e| e.inputs.iter().find(|i| i.param == *key));
            if entry.is_some() && accepted.is_none() {
                errs.push(issue(
                    "unknown_param",
                    Some(nid),
                    format!("the served route does not accept param {:?}", key),
                ));
            }
            let declared_type = val.get("type").and_then(Value::as_str);
            let mut actual: Option<String> = None;
            let is_ref;
            if let Some(target) = val.get("node").and_then(Value::as_str) {
                is_ref = true;
                match out_type.get(target) {
                    None => errs.push(issue(
                        "dangling_reference",
                        Some(nid),
                        format!(
                            "param {:?} reads node {:?}, which does not exist",
                            key, target
                        ),
                    )),
                    Some(t) => {
                        actual = t.clone();
                        if !declared.contains(target) {
                            errs.push(issue(
                                "undeclared_dependency",
                                Some(nid),
                                format!(
                                    "param {:?} reads node {:?} but depends_on does not list it",
                                    key, target
                                ),
                            ));
                        }
                    }
                }
            } else if let Some(name) = val.get("input").and_then(Value::as_str) {
                is_ref = true;
                match input_types.get(name) {
                    None => errs.push(issue(
                        "dangling_reference",
                        Some(nid),
                        format!(
                            "param {:?} reads input {:?}, which is not declared",
                            key, name
                        ),
                    )),
                    Some(t) => actual = Some(t.clone()),
                }
            } else {
                is_ref = false;
            }
            if !is_ref {
                continue; // literal value: nothing to type-check
            }

            let from_catalogue: Option<String> = accepted.and_then(|i| match i.role.as_ref() {
                Some(r) => Some(role_type(r).to_string()),
                None => ty_type(&i.ty).map(str::to_string),
            });
            if entry.is_none() && declared_type.is_none() {
                errs.push(issue(
                    "untyped_reference",
                    Some(nid),
                    format!("param {:?} has no declared catalogue entry to type it, so it must declare a \"type\"", key),
                ));
            }
            let wants: Vec<String> = [from_catalogue, declared_type.map(str::to_string)]
                .into_iter()
                .flatten()
                .collect();
            if let Some(a) = actual.as_deref() {
                for want in wants {
                    if a != want.as_str() {
                        errs.push(issue(
                            "type_mismatch",
                            Some(nid),
                            format!(
                                "param {:?} expects {}, but its source produces {}",
                                key, want, a
                            ),
                        ));
                    }
                }
            }
        }
        if let Some(e) = entry {
            for i in &e.inputs {
                if i.required && !given.contains(&i.param) {
                    errs.push(issue(
                        "missing_required_param",
                        Some(nid),
                        format!("the served route requires param {:?}", i.param),
                    ));
                }
            }
        }

        edges.insert(nid.to_string(), deps);
        let mut st = node_status(n);
        st["id"] = json!(nid);
        st["kind"] = json!(kind);
        st["output"] = json!(out_type.get(nid).cloned().flatten());
        st["depends_on"] = n.get("depends_on").cloned().unwrap_or_else(|| json!([]));
        statuses.push(st);
    }

    for o in arr(graph, "outputs") {
        let from = s(o, "from");
        if !out_type.contains_key(from) {
            errs.push(issue(
                "dangling_reference",
                None,
                format!(
                    "output {:?} reads node {:?}, which does not exist",
                    s(o, "name"),
                    from
                ),
            ));
        }
    }

    // Cycle check: repeatedly settle nodes whose dependencies have settled; the rest are on a cycle
    // or downstream of one.
    let mut done: HashSet<String> = HashSet::new();
    loop {
        let before = done.len();
        for id in &order {
            if !done.contains(id)
                && edges
                    .get(id)
                    .is_none_or(|ds| ds.iter().all(|d| done.contains(d)))
            {
                done.insert(id.clone());
            }
        }
        if done.len() == before {
            break;
        }
    }
    for id in &order {
        if !done.contains(id) {
            errs.push(issue(
                "cycle",
                Some(id.as_str()),
                format!("node {:?} cannot be ordered: it sits on a dependency cycle or downstream of one", id),
            ));
        }
    }

    report(&id, errs, statuses)
}

fn report(id: &str, errors: Vec<Value>, nodes: Vec<Value>) -> Value {
    let ok = errors.is_empty();
    json!({"ok": ok, "id": id, "errors": errors, "nodes": nodes})
}

fn issue(code: &str, node: Option<&str>, message: impl Into<String>) -> Value {
    json!({"code": code, "node": node, "message": message.into()})
}

/// Served status for one node, computed from the live catalogue. Never stored.
fn node_status(n: &Value) -> Value {
    let kind = s(n, "kind");
    if kind == NATIVE_KIND {
        return json!({"status": "native", "route": null, "missing_routes": []});
    }
    if n["route"].as_str() == Some("ask") {
        return json!({"status": "ask", "route": "ask", "missing_routes": []});
    }
    let Some((p, m)) = route_of(n) else {
        return json!({"status": "invalid_route", "route": null, "missing_routes": []});
    };
    if registry_served(&p, &m, kind) {
        let params_checked = declared_entry(&p, &m, kind).is_some();
        return json!({"status": "served", "route": {"provider": p, "model": m},
                      "params_checked": params_checked, "missing_routes": []});
    }
    let mut missing = vec![json!({"provider": p.clone(), "model": m.clone()})];
    for alt in arr(n, "route_alternatives") {
        missing.push(json!({"provider": s(alt, "provider"), "model": s(alt, "model")}));
    }
    json!({"status": "unserved", "route": {"provider": p, "model": m}, "missing_routes": missing})
}

/// Served: the engine's provider registry lists this provider, model and kind. This is the source
/// `media modalities` filters, so a route served by a built-in provider counts as served.
pub fn registry_served(provider: &str, model: &str, kind: &str) -> bool {
    registry_models()
        .iter()
        .any(|(p, m, k)| p == provider && m == model && k == kind)
}

/// The declared catalogue entry (models.json or the user override) for the same route and kind.
/// Its `inputs` drive param-name and required-param checks; absent for many built-in models.
fn declared_entry(provider: &str, model: &str, kind: &str) -> Option<&'static Entry> {
    Catalogue::get()
        .find(provider, model)
        .filter(|e| kind_name(e) == kind)
}

/// (provider id, model id, kind) for every model in the engine's provider registry, read once.
fn registry_models() -> &'static [(String, String, String)] {
    static MODELS: OnceLock<Vec<(String, String, String)>> = OnceLock::new();
    MODELS.get_or_init(|| {
        let mut out = Vec::new();
        for p in rightkit_media_gen::providers::registry() {
            for mi in p.models() {
                let kind = serde_json::to_value(mi.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(str::to_string))
                    .unwrap_or_default();
                out.push((p.id().to_string(), mi.model.to_string(), kind));
            }
        }
        out
    })
}

fn route_of(n: &Value) -> Option<(String, String)> {
    let r = &n["route"];
    Some((
        r.get("provider")?.as_str()?.to_string(),
        r.get("model")?.as_str()?.to_string(),
    ))
}

fn kind_name(e: &Entry) -> String {
    serde_json::to_value(e.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn kind_output(kind: &str) -> Option<&'static str> {
    match kind {
        "image" | "upscale" => Some("image"),
        "video" | "avatar" | "extend" | "lipsync" | "motion_control" | "replace_object"
        | "restyle" => Some("video"),
        "voice" | "music" | "sfx" => Some("audio"),
        "text" | "transcribe" => Some("text"),
        _ => None,
    }
}

fn role_type(r: &Role) -> &'static str {
    match r {
        Role::Start | Role::End | Role::Reference | Role::Mask => "image",
        Role::Video => "video",
        Role::Audio => "audio",
    }
}

fn ty_type(t: &InputType) -> Option<&'static str> {
    match t {
        InputType::String => Some("text"),
        InputType::Number | InputType::Integer => Some("number"),
        InputType::Boolean => Some("boolean"),
        _ => None,
    }
}

fn s<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}

fn arr<'a>(v: &'a Value, key: &str) -> &'a [Value] {
    v[key].as_array().map(|a| a.as_slice()).unwrap_or(&[])
}
