//! studio.recipes — the image recipe composer, its libraries (aesthetics,
//! composition, lens, lighting, depth, colour, skin, references, editing,
//! per-model quirks) and the guarded preflight → operator-approved run flow.
//!
//! Ported from `tools/recipes/image-composer.mjs` (+ `tools/recipes/image/*.json`,
//! shipped byte for byte under `data/recipes/`) and the Media Console's
//! `server/studio-image-request.mjs` / `studio-image-run.mjs` (a7271cf).
//!
//! Preflight never calls a provider. It renders the prompt and params through
//! the model's contract, prices the run, and writes a draft. Only
//! `studio.recipes.approve` with an app-minted approval token (see `approvals`;
//! `operator_approved: true` alone is refused) turns a draft into the request
//! the app hands to `generate`, and the approval is bound to a hash of the
//! exact prompt, params and references.
//!
//! Commands: `studio.recipes.{libraries,compose,compose_edit,preview,preflight,draft,approve}`.
//!
//! Recipe templates (`data/recipes/templates.json`): motion transfer and object replacement with
//! ordered, typed reference slots, a moderation guide and staged resolution
//! (`studio.recipes.{templates,template_compose,template_preflight,template_approve}`), and the
//! character reference batch with a data-only contact-sheet review
//! (`studio.recipes.{character_batch,batch_review}`).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Map, Value};

use crate::catalog;
use crate::contracts::{self, first_present, first_truthy, js_str, slot, truthy, ContractError};
use rightkit_media_gen::{Context, GenError, GenResult};

pub const COMMANDS: &[&str] = &[
    "studio.recipes.libraries",
    "studio.recipes.compose",
    "studio.recipes.compose_edit",
    "studio.recipes.preview",
    "studio.recipes.preflight",
    "studio.recipes.draft",
    "studio.recipes.approve",
    "studio.recipes.templates",
    "studio.recipes.template_compose",
    "studio.recipes.template_preflight",
    "studio.recipes.template_approve",
    "studio.recipes.character_batch",
    "studio.recipes.batch_review",
];

const SCHEMA_VERSION: &str = "1.0.0";

const LIB_SOURCES: &[(&str, &str)] = &[
    (
        "aesthetics",
        include_str!("../data/recipes/aesthetics.json"),
    ),
    (
        "composition",
        include_str!("../data/recipes/composition.json"),
    ),
    ("lens", include_str!("../data/recipes/lens.json")),
    ("lighting", include_str!("../data/recipes/lighting.json")),
    ("depth", include_str!("../data/recipes/depth.json")),
    (
        "color",
        include_str!("../data/recipes/color-psychology.json"),
    ),
    ("skin", include_str!("../data/recipes/skin-texture.json")),
    (
        "references",
        include_str!("../data/recipes/references.json"),
    ),
    ("editing", include_str!("../data/recipes/editing.json")),
    (
        "models",
        include_str!("../data/recipes/per-model-quirks.json"),
    ),
];

/// The libraries a person picks from, in picker order.
const PICKER_LIBS: &[&str] = &[
    "aesthetics",
    "composition",
    "lens",
    "lighting",
    "depth",
    "color",
    "skin",
    "editing",
];

static LIBS: OnceLock<Vec<(&'static str, Value)>> = OnceLock::new();

fn libs() -> &'static Vec<(&'static str, Value)> {
    LIBS.get_or_init(|| {
        LIB_SOURCES
            .iter()
            .map(|(name, text)| {
                let v = serde_json::from_str(text)
                    .unwrap_or_else(|e| panic!("embedded recipe library {name}: {e}"));
                (*name, v)
            })
            .collect()
    })
}

fn lib(name: &str) -> &'static Value {
    libs()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| v)
        .expect("known recipe library")
}

// ── helpers ───────────────────────────────────────────────────────────────

fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|m| m.get(key))
}

fn pick_item(library: &'static Value, key: Option<&str>) -> Option<&'static Value> {
    let key = key.filter(|k| !k.is_empty())?;
    get(library, "items").and_then(|i| i.get(key))
}

fn brand_default(library: &Value, brand: Option<&str>) -> Option<String> {
    let brand = brand?;
    get(library, "_brand_defaults")
        .and_then(|b| b.get(brand))
        .and_then(|d| d.get("item_key"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Explicit override wins, else the brand default.
fn resolve_key(library: &Value, brand: Option<&str>, over: Option<&Value>) -> Option<String> {
    match first_present(&[over]) {
        Some(v) => Some(js_str(v)),
        None => brand_default(library, brand),
    }
}

fn fragment(item: Option<&Value>) -> Option<String> {
    item.and_then(|i| get(i, "fragment"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn model_spec(model: &str) -> Result<&'static Value, ComposeError> {
    let items = get(lib("models"), "items");
    items
        .and_then(|i| i.get(model))
        .and_then(|m| m.get("model_spec"))
        .ok_or_else(|| {
            let known = items
                .and_then(Value::as_object)
                .map(|m| m.keys().cloned().collect::<Vec<_>>().join(", "))
                .unwrap_or_default();
            ComposeError::Message(format!("Unknown model \"{model}\". Known: {known}"))
        })
}

fn ratio(s: &str) -> f64 {
    static RE: OnceLock<Regex> = OnceLock::new();
    let re = RE.get_or_init(|| Regex::new(r"(\d+(?:\.\d+)?)\s*[:x]\s*(\d+(?:\.\d+)?)").unwrap());
    match re.captures(s) {
        Some(c) => c[1].parse::<f64>().unwrap_or(1.0) / c[2].parse::<f64>().unwrap_or(1.0),
        None => 1.0,
    }
}

/// Snap an aspect to the closest model-allowed entry (Imagen rejects 2:3).
fn snap_aspect(aspect: &str, allowed: Option<&Vec<Value>>) -> String {
    let Some(allowed) = allowed.filter(|a| !a.is_empty()) else {
        return aspect.to_string();
    };
    let names: Vec<&str> = allowed.iter().filter_map(Value::as_str).collect();
    if names.contains(&aspect) {
        return aspect.to_string();
    }
    let target = ratio(aspect);
    let mut best = names.first().copied().unwrap_or(aspect);
    let mut best_delta = f64::INFINITY;
    for a in names {
        let d = (ratio(a) - target).abs();
        if d < best_delta {
            best_delta = d;
            best = a;
        }
    }
    best.to_string()
}

fn aspect_to_fal_image_size(aspect: &str) -> Value {
    match aspect {
        "square" | "1:1" => json!("square_hd"),
        "landscape" => json!("landscape_4_3"),
        "4:3" | "3:4" => json!("portrait_4_3"),
        "9:16" => json!("portrait_16_9"),
        "16:9" => json!("landscape_16_9"),
        "portrait" | "2:3" => json!({"width": 1024, "height": 1536}),
        "3:2" => json!({"width": 1536, "height": 1024}),
        _ => json!("square_hd"),
    }
}

fn aspect_to_qwen_size(aspect: &str) -> &'static str {
    match aspect {
        "square" | "1:1" => "1024*1024",
        "portrait" | "2:3" => "832*1216",
        "landscape" | "3:2" => "1216*832",
        "9:16" => "720*1280",
        "16:9" => "1280*720",
        "3:4" => "768*1024",
        "4:3" => "1024*768",
        _ => "1024*1024",
    }
}

fn now_iso() -> String {
    let fmt = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
    );
    time::OffsetDateTime::now_utc()
        .format(&fmt)
        .unwrap_or_default()
}

fn to_positive_constraint(value: &str) -> String {
    static LEAD: OnceLock<Regex> = OnceLock::new();
    static SHOW: OnceLock<Regex> = OnceLock::new();
    let lead = LEAD.get_or_init(|| {
        Regex::new(r"(?i)^(?:no|without|exclude|remove|avoid|lacking)\s+").unwrap()
    });
    let show = SHOW.get_or_init(|| Regex::new(r"(?i)^(?:don't|do not)\s+show\s+").unwrap());
    let t = value.trim();
    let t = lead.replace(t, "");
    let t = show.replace(&t, "");
    t.trim().to_string()
}

/// BFL-class positive rewrite of the assembled prompt. Applies the contract's
/// `negative_prompt_policy.rewrite_table` first (longest keys win), then maps
/// any remaining negation lead-in to "free of" phrasing so authored fragments
/// like "without the artifice" stay contract-safe.
fn positivize(prompt: &str, model: &str) -> String {
    let mut out = prompt.to_string();
    if let Ok(c) = contracts::load_contract(model) {
        let table = get(c, "negative_prompt_policy")
            .and_then(|p| get(p, "rewrite_table"))
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut pairs: Vec<(String, String)> =
            table.iter().map(|(k, v)| (k.clone(), js_str(v))).collect();
        pairs.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
        for (k, v) in pairs {
            let re = Regex::new(&format!("(?i){}", regex::escape(&k))).unwrap();
            out = re.replace_all(&out, v.as_str()).to_string();
        }
    }
    static NEG: OnceLock<Regex> = OnceLock::new();
    let neg = NEG.get_or_init(|| {
        Regex::new(
            r"(?i)\b(?:do\s+not\s+show|don't\s+show|without|exclude|remove|avoid|lacking|no)\s+",
        )
        .unwrap()
    });
    tidy(&neg.replace_all(&out, "free of "))
}

// ── errors ────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub enum ComposeError {
    Message(String),
    /// The composed payload failed the model's contract.
    Contract(Box<ContractError>),
}

impl ComposeError {
    pub fn message(&self) -> String {
        match self {
            ComposeError::Message(m) => m.clone(),
            ComposeError::Contract(e) => format!(
                "[image-composer] Contract violation for model=\"{}\":\n  {}\n  (Bypass with opts.skip_contract=true if intentional.)",
                e.model_slug,
                e.message()
            ),
        }
    }
    pub fn to_json(&self) -> Value {
        match self {
            ComposeError::Message(m) => json!({"ok": false, "message": m, "violations": []}),
            ComposeError::Contract(e) => {
                let mut v = e.to_json();
                v["message"] = json!(self.message());
                v
            }
        }
    }
}

impl std::fmt::Display for ComposeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message())
    }
}

impl From<Box<ContractError>> for ComposeError {
    fn from(e: Box<ContractError>) -> Self {
        ComposeError::Contract(e)
    }
}

// ── negatives and references ──────────────────────────────────────────────

struct Negatives {
    prompt_clause: String,
    param_fields: Map<String, Value>,
    warning: Option<&'static str>,
}

fn compile_negatives(strategy: &str, negatives: &[String]) -> Result<Negatives, ComposeError> {
    let mut out = Negatives {
        prompt_clause: String::new(),
        param_fields: Map::new(),
        warning: None,
    };
    if negatives.is_empty() {
        return Ok(out);
    }
    match strategy {
        // Plain comma-separated nouns; the field name is the same for both.
        "imagen_field" | "natural_language_negative_prompt" => {
            out.param_fields
                .insert("negative_prompt".into(), json!(negatives.join(", ")));
        }
        "natural_language_inline" => {
            out.prompt_clause = format!(" Do not include: {}.", negatives.join(", "))
        }
        "preservation_clauses" => {
            let flipped: Vec<String> = negatives
                .iter()
                .map(|n| to_positive_constraint(n))
                .collect();
            out.prompt_clause =
                format!(" The image must remain clean from {}.", flipped.join(", "));
        }
        "positive_rewrite" => {
            out.warning = Some("Runway-class model — negatives skipped. Rewrite as positive intent in subject/aesthetic.");
        }
        "mj_dash_no" => out.prompt_clause = format!(" --no {}", negatives.join(", ")),
        "natural_language_edit" => {
            out.prompt_clause = format!(" Constraints: {} must not appear.", negatives.join(", "))
        }
        other => {
            return Err(ComposeError::Message(format!(
                "Unknown negative_compile_strategy \"{other}\""
            )))
        }
    }
    Ok(out)
}

struct Refs {
    prompt_clause: String,
    param_fields: Map<String, Value>,
    used: usize,
    key: Option<&'static str>,
}

fn compile_references(refs: Option<&Value>, model: &str) -> Result<Refs, ComposeError> {
    let list: Vec<Value> = refs.and_then(Value::as_array).cloned().unwrap_or_default();
    if list.is_empty() {
        return Ok(Refs {
            prompt_clause: String::new(),
            param_fields: Map::new(),
            used: 0,
            key: None,
        });
    }
    let spec = model_spec(model)?;
    let field = get(spec, "reference_field")
        .filter(|v| truthy(v))
        .map(js_str)
        .unwrap_or_else(|| "image_urls".into());
    let max = get(spec, "reference_max")
        .and_then(Value::as_u64)
        .unwrap_or(4) as usize;
    let normalized: Vec<(String, String)> = list
        .iter()
        .take(max)
        .map(|r| match r {
            Value::String(s) => ("style".to_string(), s.clone()),
            other => (
                get(other, "type")
                    .filter(|v| truthy(v))
                    .map(js_str)
                    .unwrap_or_else(|| "style".into()),
                get(other, "url").map(js_str).unwrap_or_default(),
            ),
        })
        .collect();
    let urls: Vec<Value> = normalized.iter().map(|(_, u)| json!(u)).collect();
    let key = match normalized[0].0.as_str() {
        "character" => "character_ref_identity_lock",
        "product" => "product_ref_appearance_lock",
        "layout" => "layout_ref_composition_lock",
        _ => "style_ref_locked_aesthetic",
    };
    let clause = fragment(pick_item(lib("references"), Some(key)))
        .map(|f| format!(" {f}."))
        .unwrap_or_default();
    let markers = if model == "nano-banana-pro" {
        format!(
            " {}",
            (1..=urls.len())
                .map(|i| format!("@img{i}"))
                .collect::<Vec<_>>()
                .join(" ")
        )
    } else {
        String::new()
    };
    let mut param_fields = Map::new();
    param_fields.insert(field, Value::Array(urls.clone()));
    Ok(Refs {
        prompt_clause: format!("{clause}{markers}"),
        param_fields,
        used: urls.len(),
        key: Some(key),
    })
}

// ── composer ──────────────────────────────────────────────────────────────

/// `{prompt, params, meta}`.
#[derive(Debug, Clone)]
pub struct Composed {
    pub prompt: String,
    pub params: Map<String, Value>,
    pub meta: Value,
}

impl Composed {
    pub fn to_json(&self) -> Value {
        json!({"ok": true, "prompt": self.prompt, "params": self.params, "meta": self.meta})
    }
}

fn tidy(prompt: &str) -> String {
    static DOT: OnceLock<Regex> = OnceLock::new();
    let dot = DOT.get_or_init(|| Regex::new(r"\s+\.").unwrap());
    let collapsed = dot.replace_all(prompt, ".");
    collapsed.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Turn a structured brief into the per-model `{prompt, params, meta}`.
pub fn compose(opts: &Value) -> Result<Composed, ComposeError> {
    let o = |k: &str| get(opts, k);
    let brand = o("brand").and_then(Value::as_str);
    let models = lib("models");
    let model = match first_present(&[o("model")]) {
        Some(m) => js_str(m),
        None => brand_default(models, brand).ok_or_else(|| {
            ComposeError::Message(
                "compose() requires either ctx.model or ctx.brand with a brand default".into(),
            )
        })?,
    };
    let spec = model_spec(&model)?;

    let slots: Vec<(&str, &str)> = vec![
        ("aesthetics", "aesthetic"),
        ("composition", "composition"),
        ("lens", "lens"),
        ("lighting", "lighting"),
        ("depth", "depth"),
        ("color", "color"),
        ("skin", "skin"),
    ];
    let mut keys: Vec<Option<String>> = Vec::new();
    let mut items: Vec<Option<&'static Value>> = Vec::new();
    for (lib_name, opt_name) in &slots {
        let library = lib(lib_name);
        let key = resolve_key(library, brand, o(opt_name));
        items.push(pick_item(library, key.as_deref()));
        keys.push(key);
    }
    let (aesthetic, composition, lens, lighting, depth, color, skin) = (
        items[0], items[1], items[2], items[3], items[4], items[5], items[6],
    );

    let subject = o("subject")
        .map(js_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    if subject.is_empty() {
        return Err(ComposeError::Message(
            "compose() requires opts.subject".into(),
        ));
    }

    // aesthetic LEAD + subject + composition + lens + lighting + depth + colour + skin
    let mut fragments: Vec<String> = Vec::new();
    fragments.extend(fragment(aesthetic));
    fragments.push(subject);
    for item in [composition, lens, lighting, depth, color] {
        fragments.extend(fragment(item));
    }
    if o("has_humans") != Some(&Value::Bool(false)) {
        fragments.extend(fragment(skin));
    }

    let mut negatives: Vec<String> = Vec::new();
    let mut push_neg = |n: String| {
        if !negatives.contains(&n) {
            negatives.push(n);
        }
    };
    for item in [aesthetic, composition, lens, lighting, depth, color, skin]
        .into_iter()
        .flatten()
    {
        for n in get(item, "negative_constraints")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            push_neg(js_str(n));
        }
    }
    if let Some(extra) = o("negative").and_then(Value::as_array) {
        for n in extra {
            push_neg(js_str(n));
        }
    }

    let strategy = get(spec, "negative_compile_strategy")
        .map(js_str)
        .unwrap_or_default();
    let neg = compile_negatives(&strategy, &negatives)?;
    let refs = compile_references(o("references"), &model)?;

    let mut prompt = fragments
        .join(", ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    prompt.push_str(&refs.prompt_clause);
    prompt.push_str(&neg.prompt_clause);
    let prompt = tidy(&prompt);
    let throws_on_negatives = contracts::load_contract(&model)
        .ok()
        .and_then(|c| get(c, "negative_prompt_policy"))
        .and_then(|p| get(p, "throw_on_negative_phrases"))
        .is_some_and(truthy);
    let prompt = if throws_on_negatives {
        positivize(&prompt, &model)
    } else {
        prompt
    };

    // params by body shape
    let mut params = Map::new();
    let aspect = first_truthy(&[o("aspect")])
        .map(js_str)
        .unwrap_or_else(|| "1:1".into());
    let body_shape = get(spec, "body_shape")
        .and_then(Value::as_str)
        .unwrap_or("");
    let spec_default = |k: &str| get(spec, k).filter(|v| !v.is_null());
    if body_shape == "size_string" {
        params.insert("size".into(), json!(aspect_to_qwen_size(&aspect)));
    } else if model.starts_with("flux-2") && model != "flux-2-pro" {
        // flux-2-pro is served by a route that declares no size parameter (prompt and megapixels
        // only), and the engine refuses undeclared parameters.
        let size = first_present(&[o("size")])
            .cloned()
            .unwrap_or_else(|| aspect_to_fal_image_size(&aspect));
        params.insert("image_size".into(), size);
    } else if model == "gpt-image-2" {
        params.insert(
            "size".into(),
            first_present(&[o("size")])
                .cloned()
                .unwrap_or_else(|| json!(aspect)),
        );
        params.insert(
            "resolution".into(),
            first_present(&[o("resolution"), spec_default("default_resolution")])
                .cloned()
                .unwrap_or(json!("1K")),
        );
        params.insert(
            "quality".into(),
            first_present(&[o("quality"), spec_default("default_quality")])
                .cloned()
                .unwrap_or(json!("medium")),
        );
    } else {
        params.insert(
            "aspect_ratio".into(),
            json!(snap_aspect(
                &aspect,
                get(spec, "allowed_aspects").and_then(Value::as_array)
            )),
        );
    }
    if model == "nano-banana-pro" {
        params.insert(
            "resolution".into(),
            first_present(&[o("resolution"), spec_default("default_resolution")])
                .cloned()
                .unwrap_or(json!("2K")),
        );
    }
    if let (Some(style_param), Some(style)) = (
        get(spec, "style_param").filter(|v| truthy(v)),
        o("style").filter(|v| truthy(v)),
    ) {
        params.insert(js_str(style_param), style.clone());
    }
    if get(spec, "supports_seed").is_some_and(truthy) {
        if let Some(seed) = o("seed") {
            params.insert("seed".into(), seed.clone());
        }
    }
    if get(spec, "supports_steps").is_some_and(truthy) {
        if let Some(steps) = o("num_inference_steps").filter(|v| truthy(v)) {
            params.insert("num_inference_steps".into(), steps.clone());
        }
    }
    if get(spec, "supports_guidance").is_some_and(truthy) {
        if let Some(g) = o("guidance_scale") {
            params.insert("guidance_scale".into(), g.clone());
        }
    }
    params.extend(neg.param_fields.clone());
    params.extend(refs.param_fields.clone());

    let ref_types: Vec<Value> = o("references")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|r| match r {
                    Value::String(_) => json!("style"),
                    other => json!(get(other, "type")
                        .filter(|v| truthy(v))
                        .cloned()
                        .unwrap_or(json!("style"))),
                })
                .collect()
        })
        .unwrap_or_default();
    let mut meta = json!({
        "schema_version": SCHEMA_VERSION,
        "brand": brand,
        "model": model,
        "backend": get(spec, "backend"),
        "aesthetic_key": keys[0],
        "composition_key": keys[1],
        "lens_key": keys[2],
        "lighting_key": keys[3],
        "depth_key": keys[4],
        "color_key": keys[5],
        "skin_key": keys[6],
        "references": if refs.used > 0 { json!({"count": refs.used, "key": refs.key, "types": ref_types}) } else { Value::Null },
        "negative_compile_strategy": strategy,
        "negative_count": negatives.len(),
        "composed_at": now_iso(),
    });
    if let Some(w) = neg.warning {
        meta["composer_warning"] = json!(w);
    }

    // Defence in depth: validate the composed payload against the contract.
    if !o("skip_contract").is_some_and(truthy) && contracts::has_contract(&model) {
        let mut payload = Map::new();
        payload.insert("prompt".into(), json!(prompt));
        if let Some(r) = o("references") {
            payload.insert("references".into(), r.clone());
        }
        payload.extend(params.clone());
        contracts::validate_contract(&model, &Value::Object(payload), opts)?;
        meta["contract_validated"] = json!(true);
    }
    Ok(Composed {
        prompt,
        params,
        meta,
    })
}

/// Edit-mode composer: `editing.json` templates instead of generation.
pub fn compose_edit(opts: &Value) -> Result<Composed, ComposeError> {
    let o = |k: &str| get(opts, k);
    let brand = o("brand").and_then(Value::as_str);
    let editing = lib("editing");
    let model = match first_present(&[o("model")]) {
        Some(m) => js_str(m),
        None => brand_default(lib("models"), brand).ok_or_else(|| {
            ComposeError::Message("composeEdit() requires either ctx.model or ctx.brand".into())
        })?,
    };
    let spec = model_spec(&model)?;
    let edit_key = match first_present(&[o("edit_mode")]) {
        Some(k) => Some(js_str(k)),
        None => brand_default(editing, brand),
    };
    let Some(item) = pick_item(editing, edit_key.as_deref()) else {
        let modes = get(editing, "items")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        return Err(ComposeError::Message(format!(
            "composeEdit() requires opts.edit_mode (one of: {modes})"
        )));
    };
    let mut prompt = fragment(Some(item)).unwrap_or_default();
    for (opt, pattern) in [
        ("preserve", r"\[list what must NOT change[^\]]*\]"),
        ("change", r"\[list the single specific change[^\]]*\]"),
        ("new_background", r"\[new background description\]"),
        ("target_palette", r"\[target palette description\]"),
        ("element_to_remove", r"\[specific element being deleted\]"),
    ] {
        if let Some(v) = o(opt).filter(|v| truthy(v)) {
            let re = Regex::new(pattern).expect("static pattern");
            prompt = re
                .replace(&prompt, regex::NoExpand(&js_str(v)))
                .into_owned();
        }
    }
    let negatives: Vec<String> = get(item, "negative_constraints")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(js_str).collect())
        .unwrap_or_default();
    let strategy = get(spec, "negative_compile_strategy")
        .map(js_str)
        .unwrap_or_default();
    let neg = compile_negatives(&strategy, &negatives)?;
    prompt.push_str(&neg.prompt_clause);
    let refs = compile_references(o("references"), &model)?;
    let mut params = refs.param_fields.clone();
    params.extend(neg.param_fields.clone());
    if model == "gpt-image-2" {
        params.insert(
            "resolution".into(),
            first_present(&[o("resolution")])
                .cloned()
                .unwrap_or(json!("1K")),
        );
        params.insert(
            "quality".into(),
            first_present(&[o("quality")])
                .cloned()
                .unwrap_or(json!("medium")),
        );
    }
    let meta = json!({
        "schema_version": SCHEMA_VERSION,
        "brand": brand,
        "model": model,
        "backend": get(spec, "backend"),
        "edit_mode": edit_key,
        "edit_endpoint_hint": get(item, "model_threading").and_then(|t| t.get(model.as_str())).and_then(|m| m.get("endpoint")),
        "references": if refs.used > 0 { json!({"count": refs.used, "key": refs.key}) } else { Value::Null },
        "composed_at": now_iso(),
    });
    Ok(Composed {
        prompt,
        params,
        meta,
    })
}

// ── libraries (the pickers) ───────────────────────────────────────────────

pub fn libraries() -> Value {
    let mut out = Vec::new();
    for name in PICKER_LIBS {
        let l = lib(name);
        let items: Vec<Value> = get(l, "items")
            .and_then(Value::as_object)
            .map(|m| {
                m.iter()
                    .map(|(key, item)| {
                        json!({
                            "key": key,
                            "name": get(item, "name"),
                            "use": get(item, "use"),
                            "fragment": get(item, "fragment"),
                            "tags": get(item, "tags"),
                            "negative_constraints": get(item, "negative_constraints"),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        out.push(json!({
            "id": name,
            "slot": slot_for_library(name),
            "subcategory": get(l, "_meta").and_then(|m| m.get("subcategory")),
            "principle": get(l, "_meta").and_then(|m| m.get("principle")),
            "brand_defaults": get(l, "_brand_defaults").and_then(Value::as_object).map(|m| {
                m.iter().map(|(b, d)| (b.clone(), get(d, "item_key").cloned().unwrap_or(Value::Null))).collect::<Map<String, Value>>()
            }),
            "items": items,
        }));
    }
    let models: Vec<Value> = get(lib("models"), "items")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(slug, item)| {
                    let spec = get(item, "model_spec");
                    json!({
                        "slug": slug,
                        "backend": spec.and_then(|s| get(s, "backend")),
                        "negative_compile_strategy": spec.and_then(|s| get(s, "negative_compile_strategy")),
                        "reference_max": spec.and_then(|s| get(s, "reference_max")),
                        "max_prompt_length": spec.and_then(|s| get(s, "max_prompt_length")),
                        "has_contract": contracts::has_contract(slug),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "libraries": out,
        "models": models,
        "model_brand_defaults": get(lib("models"), "_brand_defaults").and_then(Value::as_object).map(|m| {
            m.iter().map(|(b, d)| (b.clone(), get(d, "item_key").cloned().unwrap_or(Value::Null))).collect::<Map<String, Value>>()
        }),
    })
}

fn slot_for_library(name: &str) -> &'static str {
    match name {
        "aesthetics" => "aesthetic",
        "composition" => "composition",
        "lens" => "lens",
        "lighting" => "lighting",
        "depth" => "depth",
        "color" => "color",
        "skin" => "skin",
        _ => "edit_mode",
    }
}

// ── preflight request normalisation (studio-image-request.mjs) ────────────

fn slugify(value: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut dash = false;
    for c in value.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            dash = false;
        } else if !dash && !out.is_empty() {
            out.push('-');
            dash = true;
        }
    }
    let trimmed: String = out.trim_matches('-').chars().take(60).collect();
    let trimmed = trimmed.trim_matches('-').to_string();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        trimmed
    }
}

/// `normalizeAspect` from the Media Console: `portrait|landscape|square`,
/// `W:H`, or `WxH`; anything else is `None`.
pub fn normalize_aspect(value: &str) -> Option<String> {
    let t = value.trim().to_lowercase();
    if t.is_empty() {
        return None;
    }
    if ["portrait", "landscape", "square"].contains(&t.as_str()) {
        return Some(t);
    }
    static RATIO: OnceLock<Regex> = OnceLock::new();
    static DIMS: OnceLock<Regex> = OnceLock::new();
    let ratio = RATIO.get_or_init(|| Regex::new(r"^\d{1,5}\s*:\s*\d{1,5}$").unwrap());
    let dims = DIMS.get_or_init(|| Regex::new(r"^\d{2,5}\s*[x×]\s*\d{2,5}$").unwrap());
    if ratio.is_match(&t) {
        return Some(t.split_whitespace().collect());
    }
    if dims.is_match(&t) {
        return Some(t.split_whitespace().collect::<String>().replace('×', "x"));
    }
    None
}

fn image_ext(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| {
        [
            "png", "jpg", "jpeg", "webp", "gif", "avif", "heic", "bmp", "tif", "tiff",
        ]
        .contains(&e.to_ascii_lowercase().as_str())
    })
}

fn normalize_references(refs: Option<&Value>) -> GenResult<Vec<String>> {
    let bad = |m: String| GenError::new("invalid_input", m);
    let Some(refs) = refs.filter(|v| !v.is_null()) else {
        return Ok(vec![]);
    };
    let Some(list) = refs.as_array() else {
        return Err(bad(
            "Image references must be an array of path strings".into()
        ));
    };
    let limit = catalog::reference_limit();
    if list.len() > limit {
        return Err(bad(format!(
            "Image Studio supports at most {limit} references"
        )));
    }
    let mut out = Vec::new();
    for r in list {
        let Some(s) = r.as_str().filter(|s| !s.trim().is_empty()) else {
            return Err(bad("Image references must be non-empty path strings".into()));
        };
        let p = Path::new(s);
        if !p.exists() {
            return Err(bad(format!("Image reference does not exist: {s}")));
        }
        if !image_ext(p) {
            return Err(bad(format!("Image reference must be an image file: {s}")));
        }
        out.push(s.to_string());
    }
    Ok(out)
}

fn opt_string(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn is_param_value(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => a.iter().all(is_param_value),
        Value::Object(m) => m.values().all(is_param_value),
        _ => true,
    }
}

/// A normalized, validated image request: what the operator asked for.
struct ImageRequest {
    json: Value,
    model: String,
    prompt: Option<String>,
    fields: Map<String, Value>,
    recipes: Map<String, Value>,
    compose: bool,
    generation_params: Map<String, Value>,
}

fn normalize_request(input: &Value) -> GenResult<ImageRequest> {
    let bad = |m: &str| GenError::new("invalid_input", m.to_string());
    let g = |k: &str| get(input, k);
    let prompt = opt_string(g("prompt"));
    let fields: Map<String, Value> = g("fields")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let recipes: Map<String, Value> = g("recipes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let recipe_keys = [
        "aesthetic",
        "composition",
        "lens",
        "lighting",
        "depth",
        "color",
        "skin",
    ];
    let compose = recipes.get("compose") == Some(&Value::Bool(true))
        || recipe_keys
            .iter()
            .any(|k| opt_string(recipes.get(*k)).is_some());
    let has_fields = fields.values().any(is_param_value);
    if prompt.is_none() && !has_fields {
        return Err(bad("Prompt is required for Image Studio preflight"));
    }

    // Contract: explicit slug, else the intent's route, else `mid`.
    let explicit = opt_string(g("contract")).or_else(|| opt_string(g("model")));
    let intent = match opt_string(g("intent")) {
        Some(i) => i,
        None => explicit
            .as_deref()
            .and_then(|m| {
                catalog::intent_names()
                    .into_iter()
                    .find(|n| catalog::intent_model(n) == Some(m))
            })
            .unwrap_or_else(|| "mid".into()),
    };
    let Some(route_model) = catalog::intent_model(&intent) else {
        return Err(GenError::new(
            "invalid_input",
            format!("Unsupported image intent \"{intent}\""),
        ));
    };
    let model = explicit.unwrap_or_else(|| route_model.to_string());
    if !contracts::has_contract(&model) {
        return Err(GenError::new(
            "contract_not_found",
            format!("Image model contract not found: {model}"),
        ));
    }

    let aspect_raw = opt_string(g("aspect")).unwrap_or_else(|| "1:1".into());
    let aspect = normalize_aspect(&aspect_raw)
        .ok_or_else(|| bad("Aspect must be a ratio, dimensions, portrait, landscape, or square"))?;
    let brand = opt_string(g("brand"))
        .filter(|b| catalog::brands().contains(b))
        .unwrap_or_else(|| "NA".into());
    let references = normalize_references(g("references"))?;
    let mode = match opt_string(g("mode")).as_deref() {
        Some(m @ ("text_to_image" | "image_to_image")) => m.to_string(),
        _ if !references.is_empty() => "image_to_image".into(),
        _ => "text_to_image".into(),
    };

    // generation_params: only fields the model's contract accepts.
    let accepted = contracts::model_param_fields(&model);
    let mut generation_params = Map::new();
    if let Some(gp) = g("generation_params").and_then(Value::as_object) {
        for (k, v) in gp {
            if !accepted.contains(k) {
                return Err(bad(&format!(
                    "Image model {model} does not accept generation_params.{k}"
                )));
            }
            if is_param_value(v) {
                generation_params.insert(k.clone(), v.clone());
            }
        }
    }
    // Top-level contract fields (resolution, quality, ...) and the legacy aliases.
    for k in &accepted {
        if let Some(v) = g(k).filter(|v| is_param_value(v)) {
            generation_params.insert(k.clone(), v.clone());
        }
    }
    for (field, value) in [
        ("resolution", opt_string(g("resolution"))),
        ("quality", opt_string(g("quality"))),
        ("style", opt_string(g("style"))),
        ("size", opt_string(g("size"))),
        (
            "negative_prompt",
            opt_string(g("negativePrompt")).or_else(|| opt_string(g("negative_prompt"))),
        ),
    ] {
        if let Some(v) = value {
            if accepted.iter().any(|a| a == field) {
                generation_params
                    .entry(field.to_string())
                    .or_insert(json!(v));
            }
        }
    }

    let request = json!({
        "mode": mode,
        "brand": brand,
        "project_slug": slugify(opt_string(g("project_slug")).as_deref().unwrap_or(""), "studio-session"),
        "intent": intent,
        "contract": model,
        "aspect": aspect,
        "prompt": prompt,
        "fields": fields,
        "recipes": recipes,
        "references": references,
        "generation_params": generation_params,
        "workflow": g("workflow").filter(|w| w.is_object()),
        "engine": g("engine").filter(|e| e.is_object()),
        "notes": opt_string(g("notes")),
    });
    Ok(ImageRequest {
        json: request,
        model,
        prompt,
        fields,
        recipes,
        compose,
        generation_params,
    })
}

// ── building a draft ──────────────────────────────────────────────────────

/// Slot text for the composer's `subject` when the operator filled contract
/// fields instead of typing a subject.
fn subject_from(req: &ImageRequest) -> String {
    if let Some(s) = req.fields.get("subject").and_then(|v| slot(Some(v))) {
        let rest: Vec<String> = [
            "action",
            "context",
            "scene",
            "environment",
            "context_and_background",
            "location_context",
        ]
        .iter()
        .filter_map(|k| slot(req.fields.get(*k)))
        .collect();
        if rest.is_empty() {
            return s;
        }
        return format!("{s}, {}", rest.join(", "));
    }
    req.prompt.clone().unwrap_or_default()
}

fn recipe_opts(req: &ImageRequest, subject: &str) -> Value {
    let mut o = Map::new();
    o.insert("brand".into(), req.json["brand"].clone());
    o.insert("model".into(), json!(req.model));
    o.insert("subject".into(), json!(subject));
    o.insert("aspect".into(), req.json["aspect"].clone());
    for k in [
        "aesthetic",
        "composition",
        "lens",
        "lighting",
        "depth",
        "color",
        "skin",
    ] {
        if let Some(v) = opt_string(req.recipes.get(k)) {
            o.insert(k.into(), json!(v));
        }
    }
    if let Some(b) = req.recipes.get("has_humans").filter(|v| v.is_boolean()) {
        o.insert("has_humans".into(), b.clone());
    }
    for k in ["resolution", "quality", "style", "size"] {
        if let Some(v) = req.generation_params.get(k) {
            o.insert(k.into(), v.clone());
        }
    }
    if let Some(s) = req.generation_params.get("image_size") {
        o.insert("size".into(), s.clone());
    }
    if let Some(n) = req
        .generation_params
        .get("negative_prompt")
        .and_then(Value::as_str)
    {
        o.insert("negative".into(), json!([n]));
    }
    if let Some(r) = req
        .json
        .get("references")
        .filter(|r| r.as_array().is_some_and(|a| !a.is_empty()))
    {
        // The composer takes `{type,url}`; plain paths are style references.
        let refs: Vec<Value> = r
            .as_array()
            .unwrap()
            .iter()
            .map(|p| json!({"type": "style", "url": p}))
            .collect();
        o.insert("references".into(), Value::Array(refs));
    }
    for k in ["seed", "num_inference_steps", "guidance_scale"] {
        if let Some(v) = req.generation_params.get(k) {
            o.insert(k.into(), v.clone());
        }
    }
    Value::Object(o)
}

/// The internal brief the contract renderer takes: slot fields plus every
/// operator-chosen param, with the request aspect carried for renderers that use it.
fn render_internal(req: &ImageRequest) -> Value {
    let mut internal = Map::new();
    for (k, v) in &req.fields {
        if is_param_value(v) {
            internal.insert(k.clone(), v.clone());
        }
    }
    for (k, v) in &req.generation_params {
        internal.entry(k.clone()).or_insert(v.clone());
    }
    if let Some(r) = req
        .json
        .get("references")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        internal
            .entry("references".to_string())
            .or_insert(Value::Array(r.clone()));
    }
    Value::Object(internal)
}

struct Built {
    prompt: String,
    params: Map<String, Value>,
    meta: Value,
    pipeline: &'static str,
}

fn build(req: &ImageRequest) -> Result<Built, ComposeError> {
    if req.compose {
        let subject = subject_from(req);
        let c = compose(&recipe_opts(req, &subject))?;
        let mut params = c.params;
        // Explicit operator params win over composed defaults.
        for (k, v) in &req.generation_params {
            params.insert(k.clone(), v.clone());
        }
        return Ok(Built {
            prompt: c.prompt,
            params,
            meta: c.meta,
            pipeline: "recipe-composer",
        });
    }
    if req.fields.values().any(is_param_value) {
        let internal = render_internal(req);
        let r = contracts::render_contract(&req.model, &internal, &json!({}))?;
        return Ok(Built {
            prompt: r.prompt,
            params: r.params,
            meta: r.meta,
            pipeline: "contract-renderer",
        });
    }
    // Raw prompt: validated as written against the contract.
    let prompt = req.prompt.clone().unwrap_or_default();
    let mut payload = Map::new();
    payload.insert("prompt".into(), json!(prompt));
    for (k, v) in &req.generation_params {
        payload.insert(k.clone(), v.clone());
    }
    if let Some(r) = req
        .json
        .get("references")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    {
        payload.insert("references".into(), Value::Array(r.clone()));
    }
    contracts::validate_contract(&req.model, &Value::Object(payload), &json!({}))?;
    // flux-2-pro's served route takes megapixels only. The raw path applies the render path's filter
    // and reports the same warnings in meta.warnings, so it never sends a param the route refuses.
    let (params, route_warnings) = if req.model == "flux-2-pro" {
        contracts::flux_pro_route_params(&Value::Object(req.generation_params.clone()))
    } else {
        (req.generation_params.clone(), Vec::new())
    };
    Ok(Built {
        prompt,
        params,
        meta: json!({"contract_validated": true, "warnings": route_warnings}),
        pipeline: "raw-prompt",
    })
}

fn sha_hex(text: &str) -> String {
    rightkit_fs::digest::sha256_bytes(text.as_bytes())
}

/// Hash of exactly what a run would send: model, prompt, params, references.
fn request_hash(
    model: &str,
    prompt: &str,
    params: &Map<String, Value>,
    references: &Value,
) -> String {
    // serde_json keeps insertion order, so sort for a stable canonical form.
    fn canon(v: &Value) -> Value {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                Value::Object(
                    keys.into_iter()
                        .map(|k| (k.clone(), canon(&m[k])))
                        .collect(),
                )
            }
            Value::Array(a) => Value::Array(a.iter().map(canon).collect()),
            other => other.clone(),
        }
    }
    let doc = canon(
        &json!({"model": model, "prompt": prompt, "params": params, "references": references}),
    );
    sha_hex(&doc.to_string())
}

/// What the run costs: the engine's per-image estimate when the caller names
/// the engine model, else the contract's invoice-backed price.
fn cost_of(ctx: &Context, req: &ImageRequest, params: &Map<String, Value>) -> Value {
    let count = ["n", "num_images", "max_images"]
        .iter()
        .find_map(|k| params.get(*k).and_then(Value::as_u64))
        .unwrap_or(1)
        .max(1);
    let engine_model = req
        .json
        .get("engine")
        .and_then(|e| e.get("model"))
        .and_then(Value::as_str);
    if let Some(m) = engine_model {
        if let Ok(Value::Array(models)) = rightkit_media_gen::api::models(ctx, &json!({})) {
            if let Some(est) = models
                .iter()
                .find(|x| x.get("model").and_then(Value::as_str) == Some(m))
                .and_then(|x| x.get("est_usd"))
                .and_then(Value::as_f64)
            {
                return json!({"est_usd": est * count as f64, "per_image_usd": est, "count": count, "source": "engine_model"});
            }
        }
    }
    let tier = contracts::load_contract(&req.model)
        .ok()
        .and_then(|c| {
            c.get("_meta")
                .and_then(|m| m.get("pricing_tier_id"))
                .and_then(Value::as_str)
        })
        .and_then(contracts::get_invoice_status);
    match tier
        .and_then(|t| t.get("cost_per_image"))
        .and_then(Value::as_f64)
    {
        Some(per) => json!({
            "est_usd": per * count as f64, "per_image_usd": per, "count": count, "source": "contract_invoice",
            "invoice_verified": tier.and_then(|t| t.get("invoice_verified")),
            "tier": tier.and_then(|t| t.get("tier_label")),
        }),
        None => {
            json!({"est_usd": null, "per_image_usd": null, "count": count, "source": "unknown"})
        }
    }
}

fn filter_engine_params(model: &str, params: &Map<String, Value>) -> Map<String, Value> {
    let accepted = contracts::model_param_fields(model);
    params
        .iter()
        .filter(|(k, v)| {
            accepted.iter().any(|a| a == *k) && !v.is_null() && !matches!(k.as_str(), "prompt")
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn draft_dir(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("studio-requests").join("image")
}

fn draft_path(ctx: &Context, id: &str) -> GenResult<PathBuf> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(GenError::new("invalid_input", "draft_id is not a valid id"));
    }
    Ok(draft_dir(ctx).join(format!("{id}.json")))
}

fn make_draft(ctx: &Context, input: &Value, persist: bool) -> GenResult<Value> {
    let req = normalize_request(input)?;
    let built = match build(&req) {
        Ok(b) => b,
        Err(e) => return Ok(e.to_json()),
    };
    let references = req.json.get("references").cloned().unwrap_or(json!([]));
    let hash = request_hash(&req.model, &built.prompt, &built.params, &references);
    let contract = contracts::load_contract(&req.model).ok();
    let intent = req.json["intent"].as_str().unwrap_or("mid").to_string();
    let mut route = catalog::intent(&intent)
        .cloned()
        .unwrap_or_else(|| json!({}));
    route["intent"] = json!(intent);
    route["contract"] = json!(req.model);
    route["contract_version"] = contract
        .and_then(|c| c.get("_meta"))
        .and_then(|m| m.get("version"))
        .cloned()
        .unwrap_or(Value::Null);
    let engine_model = req
        .json
        .get("engine")
        .and_then(|e| e.get("model"))
        .and_then(Value::as_str);
    let mut warnings: Vec<String> = Vec::new();
    if let Some(m) = engine_model {
        match catalog::contract_for_engine_model(m) {
            Some(c) if c == req.model => {}
            Some(c) => warnings.push(format!(
                "The selected model matches the {c} contract, but this draft follows {}.",
                req.model
            )),
            None => warnings.push(format!(
                "The selected model ({m}) has no prompt contract; params are not sent to it."
            )),
        }
    }
    let created = now_iso();
    let stamp: String = created
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect();
    let nonce = sha_hex(&format!(
        "{hash}{created}{}",
        rightkit_logs::identity::random_uuid()
    ));
    let id = format!(
        "image-{stamp}-{}-{}",
        slugify(req.json["project_slug"].as_str().unwrap_or(""), "studio"),
        &nonce[..8]
    );
    let engine_params = filter_engine_params(&req.model, &built.params);
    let mut draft = json!({
        "schema_version": SCHEMA_VERSION,
        "draft_id": id,
        "status": "draft",
        "execution_mode": "preflight_only",
        "created_at": created,
        "request": req.json,
        "route": route,
        "rendered": {
            "prompt": built.prompt,
            "params": built.params,
            "engine_params": engine_params,
            "meta": built.meta,
            "pipeline": built.pipeline,
        },
        "cost": cost_of(ctx, &req, &built.params),
        "warnings": warnings,
        "request_hash": hash,
        "pipeline": {
            "type": "guarded_image_run",
            "execution_note": "This draft does not call a provider. Approve it, then run it through the engine's generate command.",
        },
    });
    if persist {
        let dir = draft_dir(ctx);
        rightkit_fs::ensure_private_dir(&dir)
            .map_err(|e| GenError::new("draft_write", format!("{}: {e}", dir.display())))?;
        let path = draft_path(ctx, &id)?;
        draft["manifest_path"] = json!(path.to_string_lossy());
        write_json(&path, &draft)?;
    }
    draft["ok"] = json!(true);
    Ok(draft)
}

fn write_json(path: &Path, value: &Value) -> GenResult<()> {
    let body = serde_json::to_string_pretty(value)
        .map_err(|e| GenError::new("draft_write", e.to_string()))?;
    rightkit_fs::atomic_write(path, body.as_bytes())
        .map_err(|e| GenError::new("draft_write", format!("{}: {e}", path.display())))
}

fn read_draft(ctx: &Context, id: &str) -> GenResult<Value> {
    let path = draft_path(ctx, id)?;
    let text = fs::read_to_string(&path).map_err(|_| {
        GenError::new(
            "draft_not_found",
            format!("Image run draft not found: {id}"),
        )
    })?;
    serde_json::from_str(&text).map_err(|e| GenError::new("draft_invalid", format!("{id}: {e}")))
}

/// The draft's hash recomputed from what it now says; a hand-edited draft
/// no longer matches the hash recorded at preflight.
fn draft_matches_its_hash(draft: &Value) -> bool {
    let model = draft["request"]["contract"].as_str().unwrap_or("");
    let prompt = draft["rendered"]["prompt"].as_str().unwrap_or("");
    let params = draft["rendered"]["params"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    let refs = draft["request"]
        .get("references")
        .cloned()
        .unwrap_or(json!([]));
    draft["request_hash"].as_str() == Some(request_hash(model, prompt, &params, &refs).as_str())
}

/// The draft fields an approval binds (the SHA-256 in `approvals` covers all of them), plus the
/// draft's existing audit hash for the UI to compare.
fn approval_inputs_of(draft: &Value) -> (Value, String) {
    let inputs = json!({
        "request": draft["request"],
        "route": draft["route"],
        "prompt": draft["rendered"]["prompt"],
        "params": draft["rendered"]["params"],
        "engine_params": draft["rendered"]["engine_params"],
        "request_hash": draft["request_hash"],
    });
    (
        inputs,
        draft["request_hash"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// Preflight plus the single-use approval challenge (returned with the response, not stored in the draft).
fn preflight_with_challenge(ctx: &Context, input: &Value) -> GenResult<Value> {
    let mut draft = make_draft(ctx, input, true)?;
    if let Some(id) = draft["draft_id"].as_str().map(str::to_string) {
        let (inputs, legacy) = approval_inputs_of(&draft);
        draft["approval_challenge"] =
            rightkit_approvals::issue(ctx, "recipes", &id, &inputs, &legacy)?;
    }
    Ok(draft)
}

/// Record the user's in-app approval. Needs an approval token only the app shell can mint
/// (`approvals`); `operator_approved` alone is refused.
fn approve(ctx: &Context, input: &Value) -> GenResult<Value> {
    let gate = rightkit_approvals::gate(input)?;
    let id = get(input, "draft_id")
        .or_else(|| get(input, "id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut draft = read_draft(ctx, id)?;
    let status = draft["status"].as_str().unwrap_or("");
    if status != "draft" && status != "approved" {
        return Err(GenError::new(
            "draft_invalid",
            "Image run can only execute a preflight draft manifest",
        ));
    }
    if draft["rendered"]["prompt"]
        .as_str()
        .unwrap_or("")
        .is_empty()
        || draft["request"]["intent"].as_str().is_none()
    {
        return Err(GenError::new(
            "draft_invalid",
            "Image run draft is missing prompt or intent",
        ));
    }
    if !draft_matches_its_hash(&draft) {
        return Err(GenError::new(
            "draft_tampered",
            "The draft changed after preflight; preflight again",
        ));
    }
    if let Some(expected) = get(input, "request_hash").and_then(Value::as_str) {
        if draft["request_hash"].as_str() != Some(expected) {
            return Err(GenError::new(
                "draft_stale",
                "The draft is not the one that was shown; preflight again",
            ));
        }
    }
    let (inputs, legacy) = approval_inputs_of(&draft);
    let record = rightkit_approvals::consume(ctx, &gate, "recipes", id, &inputs, &legacy)?;
    if status == "draft" {
        draft["status"] = json!("approved");
        draft["approved_at"] = record["approved_at"].clone();
        draft["approved_by"] = record["approved_by"].clone();
        draft["approval"] = Value::Object(rightkit_approvals::approval_fields(&record));
        write_json(&draft_path(ctx, id)?, &draft)?;
    }
    // Only engine request fields: `generate` refuses any other top-level key.
    let generate = json!({
        "kind": "image",
        "prompt": draft["rendered"]["prompt"],
        "params": draft["rendered"]["engine_params"],
    });
    Ok(json!({"ok": true, "draft": draft, "generate": generate}))
}

// ── commands ──────────────────────────────────────────────────────────────

fn opts_of(input: &Value) -> Value {
    get(input, "opts")
        .filter(|o| o.is_object())
        .cloned()
        .unwrap_or_else(|| input.clone())
}

pub fn handle(ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.recipes.libraries" => Ok(libraries()),
        "studio.recipes.compose" => Ok(match compose(&opts_of(input)) {
            Ok(c) => c.to_json(),
            Err(e) => e.to_json(),
        }),
        "studio.recipes.compose_edit" => Ok(match compose_edit(&opts_of(input)) {
            Ok(c) => c.to_json(),
            Err(e) => e.to_json(),
        }),
        // The same pipeline as preflight, nothing written: the live prompt preview.
        "studio.recipes.preview" => make_draft(ctx, input, false),
        "studio.recipes.preflight" => preflight_with_challenge(ctx, input),
        "studio.recipes.draft" => {
            let id = get(input, "draft_id")
                .or_else(|| get(input, "id"))
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut d = read_draft(ctx, id)?;
            d["ok"] = json!(true);
            d["matches_hash"] = json!(draft_matches_its_hash(&d));
            Ok(d)
        }
        "studio.recipes.approve" => approve(ctx, input),
        "studio.recipes.templates" => Ok(templates_list()),
        "studio.recipes.template_compose" => Ok(compose_template(ctx, input)),
        "studio.recipes.template_preflight" => template_preflight(ctx, input),
        "studio.recipes.template_approve" => template_approve(ctx, input),
        "studio.recipes.character_batch" => character_batch(ctx, input),
        "studio.recipes.batch_review" => batch_review(ctx, input),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}

// ── recipe templates: ordered typed slots, moderation guide, staged resolution ─────────────────
//
// Data lives in `data/recipes/templates.json`. Nothing here calls a provider: compose and preflight
// use the engine's own dry run (validates against the served catalogue entry and prices, no key,
// no spend, nothing stored).

use rightkit_media_gen::catalogue::{Catalogue, Entry, InputType, Role};

const TEMPLATES_SRC: &str = include_str!("../data/recipes/templates.json");

fn template_data() -> &'static Value {
    static T: OnceLock<Value> = OnceLock::new();
    T.get_or_init(|| {
        serde_json::from_str(TEMPLATES_SRC)
            .unwrap_or_else(|e| panic!("embedded recipe templates: {e}"))
    })
}

/// A refusal as a result body (not a command error), the same shape `compose` uses.
fn tpl_fail(code: &str, message: impl Into<String>, violations: Vec<Value>) -> Value {
    json!({"ok": false, "code": code, "message": message.into(), "violations": violations})
}

fn violation(position: usize, name: &str, code: &str, problem: impl Into<String>) -> Value {
    json!({"position": position, "slot": name, "code": code, "problem": problem.into()})
}

fn kind_name(e: &Entry) -> String {
    serde_json::to_value(e.kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn role_of(name: &str) -> Option<Role> {
    match name {
        "reference" => Some(Role::Reference),
        "video" => Some(Role::Video),
        "start" => Some(Role::Start),
        "end" => Some(Role::End),
        "audio" => Some(Role::Audio),
        "mask" => Some(Role::Mask),
        _ => None,
    }
}

/// `image` or `video` when the URL or path ends in a recognised extension, else `None`.
fn media_type_of(value: &str) -> Option<&'static str> {
    let path = value.split(['?', '#']).next().unwrap_or("");
    let (_, ext) = path.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "png" | "jpg" | "jpeg" | "webp" | "gif" | "avif" | "heic" | "bmp" | "tif" | "tiff" => {
            Some("image")
        }
        "mp4" | "mov" | "webm" | "m4v" | "mkv" | "avi" => Some("video"),
        _ => None,
    }
}

fn template_routes(t: &Value) -> Vec<(String, String)> {
    t["routes"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|r| {
                    (
                        r["provider"].as_str().unwrap_or("").to_string(),
                        r["model"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn moderation_guide(t: &Value) -> Value {
    let generic = template_data()["generic_moderation"].clone();
    json!({
        "notes": t["moderation"].clone(),
        "generic": [generic],
        "note": "Shown before submit. Notes with generic:false come from text already in the repository (source named); generic:true notes are general warnings, not provider facts.",
    })
}

/// `studio.recipes.templates`: every template with its slots, moderation guide and which routes
/// the catalogue serves right now.
fn templates_list() -> Value {
    let cat = Catalogue::get();
    let data = template_data();
    let templates: Vec<Value> = data["templates"]
        .as_object()
        .map(|m| {
            m.iter()
                .map(|(name, t)| {
                    let kind = t["kind"].as_str().unwrap_or("");
                    let routes: Vec<Value> = template_routes(t)
                        .into_iter()
                        .map(|(p, m)| {
                            let served = cat.find(&p, &m).is_some_and(|e| kind_name(e) == kind);
                            json!({"provider": p, "model": m, "served": served})
                        })
                        .collect();
                    let served = routes.iter().any(|r| r["served"] == true);
                    json!({
                        "template": name,
                        "title": t["title"],
                        "kind": kind,
                        "served": served,
                        "routes": routes,
                        "slots": t["slots"],
                        "prompt": t["prompt"],
                        "staged_resolution": t["staged_resolution"],
                        "moderation": moderation_guide(t),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    json!({"ok": true, "templates": templates, "batches": data["batches"], "slot_rules": data["_meta"]["slot_rules"]})
}

/// Check the supplied slots against the template's declared order and types. Returns the
/// `(declared slot, value)` pairs, or every violation by slot name.
fn check_slots<'a>(t: &'a Value, input: &Value) -> Result<Vec<(&'a Value, String)>, Value> {
    let declared: &[Value] = t["slots"].as_array().map(Vec::as_slice).unwrap_or(&[]);
    let order = declared
        .iter()
        .map(|s| format!("{} {}", s["position"], s["name"].as_str().unwrap_or("")))
        .collect::<Vec<_>>()
        .join(", ");
    let Some(given) = get(input, "slots")
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
    else {
        return Err(tpl_fail(
            "slots_required",
            format!("slots must list the template's reference slots in order: {order}"),
            vec![],
        ));
    };
    let mut violations: Vec<Value> = Vec::new();
    if given.len() > declared.len() {
        violations.push(violation(
            declared.len() + 1,
            "",
            "slot_overflow",
            format!(
                "{} slots given but the template declares {}: {order}",
                given.len(),
                declared.len()
            ),
        ));
    }
    let mut out = Vec::new();
    for (i, d) in declared.iter().enumerate() {
        let position = i + 1;
        let name = d["name"].as_str().unwrap_or("");
        let want_type = d["type"].as_str().unwrap_or("");
        let required = d["required"].as_bool().unwrap_or(false);
        let Some(g) = given.get(i) else {
            if required {
                violations.push(violation(position, name, "slot_missing", format!("required slot {position} {name} ({want_type}) is missing; slots go in order: {order}")));
            }
            continue;
        };
        // An entry may name its slot by position or by name; it must name the one it sits in.
        match g.get("slot") {
            Some(Value::Number(n)) if n.as_u64() != Some(position as u64) => violations.push(violation(
                position,
                name,
                "slot_order",
                format!("entry names slot {n} but sits at position {position} ({name}); slots go in order: {order}"),
            )),
            Some(Value::String(s)) if s != name => violations.push(violation(
                position,
                name,
                "slot_order",
                format!("entry names slot \"{s}\" but sits at position {position} ({name}); slots go in order: {order}"),
            )),
            _ => {}
        }
        let given_type = g.get("type").and_then(Value::as_str).unwrap_or("");
        if given_type != want_type {
            violations.push(violation(
                position,
                name,
                "slot_type",
                format!("slot {position} {name} takes {want_type}, got type \"{given_type}\""),
            ));
        }
        let value = opt_string(g.get("value")).unwrap_or_default();
        if value.is_empty() {
            violations.push(violation(
                position,
                name,
                "slot_value",
                format!("slot {position} {name} has no value"),
            ));
            continue;
        }
        if let Some(actual) =
            media_type_of(&value).filter(|a| given_type == want_type && *a != want_type)
        {
            violations.push(violation(position, name, "slot_type", format!("slot {position} {name} takes {want_type}, but the value looks like {actual}: {value}")));
        }
        out.push((d, value));
    }
    if violations.is_empty() {
        return Ok(out);
    }
    let code = violations[0]["code"]
        .as_str()
        .unwrap_or("slot_invalid")
        .to_string();
    let names: Vec<String> = violations
        .iter()
        .map(|v| {
            format!(
                "{} ({})",
                v["slot"].as_str().unwrap_or("?"),
                v["code"].as_str().unwrap_or("")
            )
        })
        .collect();
    Err(tpl_fail(
        &code,
        format!("slot violations: {}", names.join(", ")),
        violations,
    ))
}

/// Pick the first route of the template that the catalogue serves for the template's kind.
fn pick_route(name: &str, t: &Value, input: &Value) -> Result<&'static Entry, Value> {
    let kind = t["kind"].as_str().unwrap_or("");
    let routes = template_routes(t);
    let candidates: Vec<(String, String)> = match (
        opt_string(get(input, "provider")),
        opt_string(get(input, "model")),
    ) {
        (Some(p), Some(m)) => {
            if !routes.contains(&(p.clone(), m.clone())) {
                let list = routes
                    .iter()
                    .map(|(p, m)| format!("{p}/{m}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(tpl_fail(
                    "route_not_in_template",
                    format!("{p}/{m} is not a route of template \"{name}\". Its routes: {list}"),
                    vec![],
                ));
            }
            vec![(p, m)]
        }
        (None, None) => routes,
        _ => {
            return Err(tpl_fail(
                "invalid_input",
                "provider and model go together",
                vec![],
            ))
        }
    };
    let cat = Catalogue::get();
    if let Some(e) = candidates
        .iter()
        .find_map(|(p, m)| cat.find(p, m).filter(|e| kind_name(e) == kind))
    {
        return Ok(e);
    }
    let missing: Vec<Value> = candidates
        .iter()
        .map(|(p, m)| json!({"provider": p, "model": m}))
        .collect();
    let list = candidates
        .iter()
        .map(|(p, m)| format!("{p}/{m}"))
        .collect::<Vec<_>>()
        .join(", ");
    Err(json!({
        "ok": false,
        "status": "unsupported",
        "code": "route_not_served",
        "template": name,
        "kind": kind,
        "missing_routes": missing,
        "message": format!("No {kind} route is served for template \"{name}\": the catalogue has no entry for {list}. Nothing was routed or guessed."),
        "violations": [],
    }))
}

fn resolution_rank(s: &str) -> Option<u64> {
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Draft at the lowest resolution the route serves, final at the requested one.
fn staged_plan(t: &Value, entry: &Entry, params: &Map<String, Value>) -> Value {
    let param = t["staged_resolution"]["param"]
        .as_str()
        .unwrap_or("resolution");
    let unavailable =
        |reason: String| json!({"available": false, "param": param, "reason": reason});
    let Some(input) = entry.inputs.iter().find(|i| i.param == param && i.send) else {
        return unavailable(format!(
            "{}/{} serves no {param} parameter, so there is no lower resolution to draft at",
            entry.provider, entry.model
        ));
    };
    let Some(values) = input.enum_values.as_ref() else {
        return unavailable(format!(
            "{param} on {}/{} is not an enumerated list, so its lowest value is not known",
            entry.provider, entry.model
        ));
    };
    let mut options: Vec<(u64, String)> = Vec::new();
    for v in values {
        match v
            .as_str()
            .and_then(|s| resolution_rank(s).map(|r| (r, s.to_string())))
        {
            Some(o) => options.push(o),
            None => {
                return unavailable(format!(
                    "{param} values on {}/{} cannot be ordered by size",
                    entry.provider, entry.model
                ))
            }
        }
    }
    options.sort();
    let Some((_, lowest)) = options.first().cloned() else {
        return unavailable(format!(
            "{param} on {}/{} lists no values",
            entry.provider, entry.model
        ));
    };
    let Some(requested) = params.get(param).and_then(Value::as_str) else {
        return unavailable(format!(
            "params.{param} (the final resolution) is required for staged resolution"
        ));
    };
    json!({
        "available": true,
        "param": param,
        "options": options.iter().map(|(_, s)| s).collect::<Vec<_>>(),
        "lowest": lowest,
        "requested": requested,
        "single_stage": lowest == requested,
        "identical_prompt": true,
        "second_requires_first_approval": true,
    })
}

fn dry_run(ctx: &Context, request: &Value) -> Result<Value, Value> {
    rightkit_media_gen::api::generate(ctx, request, true)
        .map_err(|e| tpl_fail(&e.code, e.message, vec![]))
}

/// `studio.recipes.template_compose`: validate the slots, map them to the served route's
/// parameters through its catalogue entry, build the prompt, and price it with the engine dry run.
fn compose_template(ctx: &Context, input: &Value) -> Value {
    let name = opt_string(get(input, "template")).unwrap_or_default();
    let Some(t) = get(template_data(), "templates").and_then(|m| m.get(name.as_str())) else {
        let known = get(template_data(), "templates")
            .and_then(Value::as_object)
            .map(|m| m.keys().cloned().collect::<Vec<_>>().join(", "))
            .unwrap_or_default();
        return tpl_fail(
            "unknown_template",
            format!("Unknown template \"{name}\". Known: {known}"),
            vec![],
        );
    };
    let slots = match check_slots(t, input) {
        Ok(s) => s,
        Err(e) => return e,
    };
    let entry = match pick_route(&name, t, input) {
        Ok(e) => e,
        Err(e) => return e,
    };
    let kind = t["kind"].as_str().unwrap_or("");

    // Slot -> served parameter, by the engine role each slot declares.
    let mut mapped = Map::new();
    let mut rows: Vec<Value> = Vec::new();
    let mut violations: Vec<Value> = Vec::new();
    let mut roles: Vec<&str> = Vec::new();
    for (d, _) in &slots {
        let r = d["engine_role"].as_str().unwrap_or("");
        if !roles.contains(&r) {
            roles.push(r);
        }
    }
    for role_name in roles {
        let Some(role) = role_of(role_name) else {
            return tpl_fail(
                "template_invalid",
                format!("template \"{name}\" declares unknown engine_role \"{role_name}\""),
                vec![],
            );
        };
        let inputs: Vec<&rightkit_media_gen::catalogue::Input> = entry
            .inputs
            .iter()
            .filter(|i| {
                i.send
                    && i.role == Some(role)
                    && matches!(i.ty, InputType::Url | InputType::UrlList)
            })
            .collect();
        let list_input = inputs.iter().find(|i| i.ty == InputType::UrlList).copied();
        let singles: Vec<&rightkit_media_gen::catalogue::Input> = inputs
            .iter()
            .filter(|i| i.ty == InputType::Url)
            .copied()
            .collect();
        let mut single_next = 0usize;
        let mut list: Vec<Value> = Vec::new();
        for (d, value) in slots
            .iter()
            .filter(|(d, _)| d["engine_role"].as_str() == Some(role_name))
        {
            let position = d["position"].as_u64().unwrap_or(0) as usize;
            let sname = d["name"].as_str().unwrap_or("");
            let param = if let Some(li) = list_input {
                if li.max_items.is_some_and(|m| list.len() >= m) {
                    violations.push(violation(
                        position,
                        sname,
                        "slot_unservable",
                        format!(
                            "{}/{} takes at most {} {role_name} inputs in {}",
                            entry.provider,
                            entry.model,
                            li.max_items.unwrap_or(0),
                            li.param
                        ),
                    ));
                    continue;
                }
                list.push(json!(value));
                li.param.clone()
            } else if let Some(si) = singles.get(single_next) {
                single_next += 1;
                mapped.insert(si.param.clone(), json!(value));
                si.param.clone()
            } else {
                violations.push(violation(
                    position,
                    sname,
                    "slot_unservable",
                    format!("{}/{} has no free {role_name} input for slot {position} {sname}; it takes {} {role_name} input(s)", entry.provider, entry.model, singles.len()),
                ));
                continue;
            };
            rows.push(json!({
                "position": position, "name": sname, "type": d["type"], "engine_role": role_name,
                "role": d["role"], "param": param, "value": value,
            }));
        }
        if let Some(li) = list_input {
            if !list.is_empty() {
                mapped.insert(li.param.clone(), Value::Array(list));
            }
        }
    }
    if !violations.is_empty() {
        let names: Vec<String> = violations
            .iter()
            .map(|v| v["slot"].as_str().unwrap_or("?").to_string())
            .collect();
        return tpl_fail(
            "slot_unservable",
            format!(
                "{}/{} cannot take slot(s): {}",
                entry.provider,
                entry.model,
                names.join(", ")
            ),
            violations,
        );
    }
    rows.sort_by_key(|r| r["position"].as_u64().unwrap_or(0));

    // Parameters: the caller's, plus the mapped slots. A caller key that is a slot parameter is a conflict.
    let mut params = get(input, "params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(k) = params.keys().find(|k| mapped.contains_key(*k)) {
        return tpl_fail(
            "param_conflict",
            format!("params.{k} is filled by a slot; remove it from params"),
            vec![],
        );
    }
    params.extend(mapped);

    let mut warnings: Vec<String> = Vec::new();
    let mut prompt = format!(
        "{} {}",
        t["prompt"]["lead"].as_str().unwrap_or(""),
        t["prompt"]["lock"].as_str().unwrap_or("")
    );
    if let Some(extra) = opt_string(get(input, "prompt")) {
        prompt = format!("{prompt} {extra}");
    }
    let prompt = tidy(&prompt);
    if !entry.inputs.iter().any(|i| i.param == "prompt" && i.send) {
        warnings.push(format!("{}/{} takes no prompt input; the prompt is part of the request but is not sent to the provider.", entry.provider, entry.model));
    }
    let request = json!({"kind": kind, "provider": entry.provider, "model": entry.model, "prompt": prompt, "params": Value::Object(params.clone())});
    let estimate = match dry_run(ctx, &request) {
        Ok(v) => v,
        Err(mut e) => {
            e["route"] = json!({"provider": entry.provider, "model": entry.model});
            return e;
        }
    };
    json!({
        "ok": true,
        "template": name,
        "kind": kind,
        "route": {"provider": entry.provider, "model": entry.model, "label": entry.label, "endpoint": entry.endpoint},
        "slots": rows,
        "request": request,
        "est_usd": estimate.get("est_usd").cloned().unwrap_or(Value::Null),
        "would_submit": estimate.get("would_submit").cloned().unwrap_or(Value::Null),
        "moderation": moderation_guide(t),
        "staged": staged_plan(t, entry, &params),
        "warnings": warnings,
    })
}

// ── template drafts (a request per draft; the second stage depends on the first's approval) ─────

fn tpl_dir(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("studio-requests").join("template")
}

fn tpl_path(ctx: &Context, id: &str) -> GenResult<PathBuf> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(GenError::new("invalid_input", "draft_id is not a valid id"));
    }
    Ok(tpl_dir(ctx).join(format!("{id}.json")))
}

fn tpl_read(ctx: &Context, id: &str) -> GenResult<Value> {
    let path = tpl_path(ctx, id)?;
    let text = fs::read_to_string(&path)
        .map_err(|_| GenError::new("draft_not_found", format!("Template draft not found: {id}")))?;
    serde_json::from_str(&text).map_err(|e| GenError::new("draft_invalid", format!("{id}: {e}")))
}

fn tpl_write(ctx: &Context, doc: &Value) -> GenResult<()> {
    let id = doc["draft_id"].as_str().unwrap_or("");
    rightkit_fs::ensure_private_dir(&tpl_dir(ctx))
        .map_err(|e| GenError::new("draft_write", format!("{}: {e}", tpl_dir(ctx).display())))?;
    write_json(&tpl_path(ctx, id)?, doc)
}

/// The hash of exactly the engine request a draft would send.
fn tpl_request_hash(request: &Value) -> String {
    let params = request["params"].as_object().cloned().unwrap_or_default();
    request_hash(
        request["model"].as_str().unwrap_or(""),
        request["prompt"].as_str().unwrap_or(""),
        &params,
        &json!({"kind": request["kind"], "provider": request["provider"]}),
    )
}

fn new_tpl_id(prefix: &str, hash: &str, salt: &str) -> String {
    let created = now_iso();
    let stamp: String = created
        .chars()
        .filter(char::is_ascii_digit)
        .take(14)
        .collect();
    let nonce = sha_hex(&format!(
        "{hash}{salt}{created}{}",
        rightkit_logs::identity::random_uuid()
    ));
    format!("{prefix}-{stamp}-{}", &nonce[..8])
}

fn template_preflight(ctx: &Context, input: &Value) -> GenResult<Value> {
    let composed = compose_template(ctx, input);
    if composed["ok"] != true {
        return Ok(composed);
    }
    let staged = input
        .get("staged")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let plan = composed["staged"].clone();
    let mut stages: Vec<(&str, Value)> = Vec::new();
    let mut note: Option<String> = None;
    if staged {
        if plan["available"] != true {
            return Ok(tpl_fail(
                "staged_unavailable",
                plan["reason"]
                    .as_str()
                    .unwrap_or("staged resolution is not available")
                    .to_string(),
                vec![],
            ));
        }
        if plan["single_stage"] == true {
            note = Some("The requested resolution is already the lowest the route serves; one request, no draft stage.".into());
            stages.push(("single", composed["request"].clone()));
        } else {
            let param = plan["param"].as_str().unwrap_or("resolution");
            let mut draft_req = composed["request"].clone();
            draft_req["params"][param] = plan["lowest"].clone();
            stages.push(("draft", draft_req));
            stages.push(("final", composed["request"].clone()));
        }
    } else {
        stages.push(("single", composed["request"].clone()));
    }

    let mut out: Vec<Value> = Vec::new();
    let mut previous: Option<String> = None;
    for (stage, request) in stages {
        let estimate = match dry_run(ctx, &request) {
            Ok(v) => v,
            Err(e) => return Ok(e),
        };
        let hash = tpl_request_hash(&request);
        let id = new_tpl_id(
            &format!(
                "tpl-{}",
                slugify(composed["template"].as_str().unwrap_or(""), "template")
            ),
            &hash,
            stage,
        );
        let doc = json!({
            "schema_version": SCHEMA_VERSION,
            "draft_id": id,
            "status": "draft",
            "execution_mode": "preflight_only",
            "created_at": now_iso(),
            "template": composed["template"],
            "stage": stage,
            "requires_approval_of": previous,
            "request": request,
            "slots": composed["slots"],
            "route": composed["route"],
            "est_usd": estimate.get("est_usd").cloned().unwrap_or(Value::Null),
            "moderation": composed["moderation"],
            "warnings": composed["warnings"],
            "request_hash": hash,
            "pipeline": {"type": "guarded_template_run", "execution_note": "This draft does not call a provider. Approve it, then run the returned request through the engine's generate command."},
        });
        tpl_write(ctx, &doc)?;
        out.push(json!({
            "draft_id": id, "stage": stage, "status": "draft", "requires_approval_of": previous,
            "est_usd": doc["est_usd"], "request_hash": doc["request_hash"], "request": doc["request"],
            "approval_challenge": tpl_challenge(ctx, &doc)?,
        }));
        previous = Some(id);
    }
    Ok(json!({
        "ok": true,
        "template": composed["template"],
        "kind": composed["kind"],
        "route": composed["route"],
        "staged": staged,
        "stage_plan": plan,
        "note": note,
        "drafts": out,
        "slots": composed["slots"],
        "moderation": composed["moderation"],
        "warnings": composed["warnings"],
    }))
}

/// One single-stage template draft for a request built outside `templates.json` (a saved swap
/// template). Same store, hash and challenge as `template_preflight`, so
/// `studio.recipes.template_approve` and the approval gates apply unchanged.
pub(crate) fn single_stage_draft(
    ctx: &Context,
    template: &str,
    request: &Value,
    slots: Value,
    route: Value,
    warnings: Value,
) -> GenResult<Value> {
    let estimate = dry_run(ctx, request).map_err(|e| {
        GenError::new(
            e["code"].as_str().unwrap_or("estimate_failed"),
            e["message"]
                .as_str()
                .unwrap_or("estimate failed")
                .to_string(),
        )
    })?;
    let hash = tpl_request_hash(request);
    let id = new_tpl_id(
        &format!("tpl-{}", slugify(template, "template")),
        &hash,
        "single",
    );
    let doc = json!({
        "schema_version": SCHEMA_VERSION, "draft_id": id, "status": "draft", "execution_mode": "preflight_only",
        "created_at": now_iso(), "template": template, "stage": "single", "requires_approval_of": Value::Null,
        "request": request, "slots": slots, "route": route,
        "est_usd": estimate.get("est_usd").cloned().unwrap_or(Value::Null),
        "moderation": Value::Null, "warnings": warnings, "request_hash": hash,
        "pipeline": {"type": "guarded_template_run", "execution_note": "This draft does not call a provider. Approve it, then run the returned request through the engine's generate command."},
    });
    tpl_write(ctx, &doc)?;
    Ok(json!({
        "draft_id": id, "stage": "single", "status": "draft", "est_usd": doc["est_usd"],
        "request_hash": doc["request_hash"], "request": doc["request"],
        "approval_challenge": tpl_challenge(ctx, &doc)?,
    }))
}

/// The template draft fields an approval binds (the SHA-256 in `approvals` covers all of them),
/// plus the draft's request hash for the UI to compare.
fn tpl_approval_inputs_of(doc: &Value) -> (Value, String) {
    let inputs = json!({
        "template": doc["template"],
        "stage": doc["stage"],
        "requires_approval_of": doc["requires_approval_of"],
        "request": doc["request"],
        "request_hash": doc["request_hash"],
    });
    (
        inputs,
        doc["request_hash"].as_str().unwrap_or_default().to_string(),
    )
}

/// The single-use approval challenge for one template draft (returned with the response, never stored in the draft).
fn tpl_challenge(ctx: &Context, doc: &Value) -> GenResult<Value> {
    let id = doc["draft_id"].as_str().unwrap_or_default();
    let (inputs, legacy) = tpl_approval_inputs_of(doc);
    rightkit_approvals::issue(ctx, "recipe_templates", id, &inputs, &legacy)
}

/// Approve one template draft: hash-bound, token-bound, and a final stage needs its draft stage
/// approved first. `gate` is the caller's already-checked approval presentation; the token is
/// consumed only after every integrity check passes.
fn approve_template_draft(
    ctx: &Context,
    id: &str,
    gate: &rightkit_approvals::Gate,
) -> GenResult<Value> {
    let mut doc = tpl_read(ctx, id)?;
    match doc["status"].as_str().unwrap_or("") {
        "draft" | "approved" => {}
        "rejected" => {
            return Err(GenError::new(
                "draft_rejected",
                format!("Draft {id} was rejected; preflight again"),
            ))
        }
        _ => {
            return Err(GenError::new(
                "draft_invalid",
                "Only a preflight draft can be approved",
            ))
        }
    }
    if doc["request_hash"].as_str() != Some(tpl_request_hash(&doc["request"]).as_str()) {
        return Err(GenError::new(
            "draft_tampered",
            "The draft changed after preflight; preflight again",
        ));
    }
    if doc["identity_attached"] == false {
        return Err(GenError::new(
            "identity_not_attached",
            format!("Draft {id} has no character identity attached (local references are not uploaded); rebuild the batch with https references or upload:true"),
        ));
    }
    if let Some(dep) = doc["requires_approval_of"].as_str() {
        let dep_doc = tpl_read(ctx, dep)?;
        if dep_doc["status"] != "approved" {
            return Err(GenError::new(
                "dependency_unapproved",
                format!(
                    "Draft {id} is the {} stage; approve its draft stage {dep} first",
                    doc["stage"].as_str().unwrap_or("final")
                ),
            ));
        }
    }
    let (inputs, legacy) = tpl_approval_inputs_of(&doc);
    let record = rightkit_approvals::consume(ctx, gate, "recipe_templates", id, &inputs, &legacy)?;
    if doc["status"] == "draft" {
        doc["status"] = json!("approved");
        doc["approved_at"] = record["approved_at"].clone();
        doc["approved_by"] = record["approved_by"].clone();
        doc["approval"] = Value::Object(rightkit_approvals::approval_fields(&record));
        tpl_write(ctx, &doc)?;
    }
    Ok(doc)
}

/// Needs an approval token only the app shell can mint (`approvals`, kind `recipe_templates`);
/// `operator_approved` is ignored. Input: `{draft_id, approval_token, approval_nonce}`.
fn template_approve(ctx: &Context, input: &Value) -> GenResult<Value> {
    let gate = rightkit_approvals::gate(input)?;
    let id = get(input, "draft_id")
        .or_else(|| get(input, "id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let doc = approve_template_draft(ctx, id, &gate)?;
    let generate = doc["request"].clone();
    Ok(json!({"ok": true, "draft": doc, "generate": generate}))
}

// ── character reference batch ───────────────────────────────────────────────────────────────────
//
// One character plus N scenario lines expand to N image requests that share one character lock.
// The review is data: each item's state is its draft's status (pending, approved, rejected), kept
// through the same draft store and hash-bound approval as every template draft. No image
// compositing happens in this slice.

fn batch_dir(ctx: &Context) -> PathBuf {
    ctx.data_dir.join("studio-requests").join("character-batch")
}

fn batch_path(ctx: &Context, id: &str) -> GenResult<PathBuf> {
    if id.is_empty()
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(GenError::new("invalid_input", "batch_id is not a valid id"));
    }
    Ok(batch_dir(ctx).join(format!("{id}.json")))
}

fn batch_read(ctx: &Context, id: &str) -> GenResult<Value> {
    let text = fs::read_to_string(batch_path(ctx, id)?).map_err(|_| {
        GenError::new(
            "batch_not_found",
            format!("Character batch not found: {id}"),
        )
    })?;
    serde_json::from_str(&text).map_err(|e| GenError::new("batch_invalid", format!("{id}: {e}")))
}

fn character_batch(ctx: &Context, input: &Value) -> GenResult<Value> {
    let bad = |m: &str| GenError::new("invalid_input", m.to_string());
    let b = &template_data()["batches"]["character_reference_batch"];
    let character_id =
        opt_string(get(input, "character_id")).ok_or_else(|| bad("character_id is required"))?;
    let provider = opt_string(get(input, "provider")).ok_or_else(|| bad("provider is required"))?;
    let model = opt_string(get(input, "model")).ok_or_else(|| bad("model is required"))?;
    let scenarios: Vec<String> = get(input, "scenarios")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("scenarios must be an array of scenario lines"))?
        .iter()
        .map(|s| {
            opt_string(Some(s)).ok_or_else(|| bad("every scenario line must be non-empty text"))
        })
        .collect::<Result<Vec<String>, GenError>>()?;
    let max = b["max_items"].as_u64().unwrap_or(24) as usize;
    if scenarios.is_empty() || scenarios.len() > max {
        return Err(bad(&format!("scenarios must hold 1 to {max} lines")));
    }

    // One lock for the whole batch, by the existing character lock. A refusal is carried per item.
    let mut lock_input = json!({"character_id": character_id, "provider": provider, "model": model, "kind": "image"});
    for k in [
        "brand",
        "references",
        "aesthetic_lora_id",
        "fallback_to_references",
        "upload",
    ] {
        if let Some(v) = get(input, k).filter(|v| !v.is_null()) {
            lock_input[k] = v.clone();
        }
    }
    let lock = crate::characters::handle(ctx, "studio.characters.lock", &lock_input);
    let lock_summary = match &lock {
        Ok(l) => json!({
            "mode": l["mode"], "model": l["model"], "references": l["references"],
            "pending_upload": l["pending_upload"], "diagnostics": l["diagnostics"],
        }),
        Err(e) => json!({"refused": {"code": e.code, "message": e.message}}),
    };

    let extra_params = get(input, "params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let batch_id = new_tpl_id(
        &format!("chrbatch-{}", slugify(&character_id, "character")),
        &sha_hex(&scenarios.join("\n")),
        &model,
    );
    let mut items: Vec<Value> = Vec::new();
    let mut challenges: std::collections::HashMap<String, Value> = std::collections::HashMap::new();
    for (i, scenario) in scenarios.iter().enumerate() {
        let index = i + 1;
        let l = match &lock {
            Err(e) => {
                items.push(json!({"index": index, "scenario": scenario, "draft_id": null, "refusal": {"code": e.code, "message": e.message}}));
                continue;
            }
            Ok(l) => l,
        };
        let mode = l["mode"].as_str().unwrap_or("none");
        let clause = b["prompt"]["lock_clauses"][mode].as_str().unwrap_or("");
        let prompt = tidy(&format!(
            "{} {clause}",
            b["prompt"]["scenario"]
                .as_str()
                .unwrap_or("{scenario}")
                .replace("{scenario}", scenario)
        ));
        let mut params = l["params"].as_object().cloned().unwrap_or_default();
        params.extend(extra_params.clone());
        let request = json!({"kind": "image", "provider": provider, "model": model, "prompt": prompt, "params": Value::Object(params)});
        let pending_empty = l["pending_upload"].as_array().is_none_or(|a| a.is_empty());
        let identity_attached = mode == "lora" || (mode == "reference" && pending_empty);
        let mut warnings: Vec<String> = Vec::new();
        if !identity_attached {
            warnings.push("No character identity is attached to this request (no LoRA, and no uploaded or https reference). Approval is refused until it is.".into());
        }
        let estimate = match dry_run(ctx, &request) {
            Ok(v) => v,
            Err(e) => {
                items.push(json!({"index": index, "scenario": scenario, "draft_id": null, "refusal": {"code": e["code"], "message": e["message"]}}));
                continue;
            }
        };
        let hash = tpl_request_hash(&request);
        let draft_id = new_tpl_id(&format!("tpl-character-batch-{index}"), &hash, &batch_id);
        let doc = json!({
            "schema_version": SCHEMA_VERSION,
            "draft_id": draft_id,
            "status": "draft",
            "execution_mode": "preflight_only",
            "created_at": now_iso(),
            "template": "character_reference_batch",
            "stage": "single",
            "requires_approval_of": null,
            "batch_id": batch_id,
            "item_index": index,
            "scenario": scenario,
            "identity_attached": identity_attached,
            "asset": null,
            "request": request,
            "est_usd": estimate.get("est_usd").cloned().unwrap_or(Value::Null),
            "warnings": warnings,
            "request_hash": hash,
        });
        tpl_write(ctx, &doc)?;
        items.push(
            json!({"index": index, "scenario": scenario, "draft_id": draft_id, "refusal": null}),
        );
        challenges.insert(draft_id.clone(), tpl_challenge(ctx, &doc)?);
    }
    let manifest = json!({
        "schema_version": SCHEMA_VERSION,
        "batch_id": batch_id,
        "template": "character_reference_batch",
        "character_id": character_id,
        "created_at": now_iso(),
        "lock": lock_summary,
        "items": items,
        "review": b["review"],
    });
    rightkit_fs::ensure_private_dir(&batch_dir(ctx))
        .map_err(|e| GenError::new("draft_write", format!("{}: {e}", batch_dir(ctx).display())))?;
    write_json(&batch_path(ctx, &batch_id)?, &manifest)?;
    let mut view = batch_view(ctx, &manifest)?;
    // Each item's single-use approval challenge rides with the creation response only.
    if let Some(rows) = view["items"].as_array_mut() {
        for row in rows {
            if let Some(challenge) = row["draft_id"].as_str().and_then(|d| challenges.get(d)) {
                row["approval_challenge"] = challenge.clone();
            }
        }
    }
    Ok(view)
}

/// The contact-sheet review as data: each item with its state read back from its draft.
fn batch_view(ctx: &Context, manifest: &Value) -> GenResult<Value> {
    let mut rows: Vec<Value> = Vec::new();
    let (mut pending, mut approved, mut rejected, mut refused) = (0, 0, 0, 0);
    for item in manifest["items"].as_array().cloned().unwrap_or_default() {
        let Some(id) = item["draft_id"].as_str() else {
            refused += 1;
            rows.push(json!({"index": item["index"], "scenario": item["scenario"], "state": "refused", "draft_id": null, "refusal": item["refusal"], "asset": null}));
            continue;
        };
        let d = tpl_read(ctx, id)?;
        let state = match d["status"].as_str().unwrap_or("") {
            "approved" => {
                approved += 1;
                "approved"
            }
            "rejected" => {
                rejected += 1;
                "rejected"
            }
            _ => {
                pending += 1;
                "pending"
            }
        };
        rows.push(json!({
            "index": item["index"], "scenario": item["scenario"], "state": state, "draft_id": id,
            "prompt": d["request"]["prompt"], "est_usd": d["est_usd"], "identity_attached": d["identity_attached"],
            "asset": d["asset"], "refusal": null,
        }));
    }
    Ok(json!({
        "ok": true,
        "batch_id": manifest["batch_id"],
        "template": manifest["template"],
        "character_id": manifest["character_id"],
        "lock": manifest["lock"],
        "review": manifest["review"],
        "counts": {"items": rows.len(), "pending": pending, "approved": approved, "rejected": rejected, "refused": refused},
        "items": rows,
    }))
}

/// `studio.recipes.batch_review {batch_id, decisions?: [{item, decision: approve|reject, asset?, approval_token?, approval_nonce?}]}`.
/// Without decisions it reads the review back. A reject needs no token and is final (an item
/// rejected here can never be approved: `decision_final`). An approve decision needs the approval
/// token and nonce of that item's draft challenge (kind `recipe_templates`) inside the decision;
/// `operator_approved` is ignored.
fn batch_review(ctx: &Context, input: &Value) -> GenResult<Value> {
    let id = get(input, "batch_id")
        .or_else(|| get(input, "id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let manifest = batch_read(ctx, id)?;
    let decisions = get(input, "decisions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    // Validate every decision before writing any.
    let mut plan: Vec<(
        String,
        String,
        Option<String>,
        Option<rightkit_approvals::Gate>,
    )> = Vec::new();
    for d in &decisions {
        let index = d.get("item").and_then(Value::as_u64).unwrap_or(0);
        let item = manifest["items"]
            .as_array()
            .and_then(|a| a.iter().find(|i| i["index"].as_u64() == Some(index)));
        let Some(item) = item else {
            return Err(GenError::new(
                "item_not_found",
                format!("Batch {id} has no item {index}"),
            ));
        };
        let Some(draft_id) = item["draft_id"].as_str() else {
            return Err(GenError::new(
                "item_refused",
                format!("Item {index} was refused by the character lock and cannot be reviewed"),
            ));
        };
        let decision = d.get("decision").and_then(Value::as_str).unwrap_or("");
        if !["approve", "reject"].contains(&decision) {
            return Err(GenError::new(
                "invalid_input",
                "decision must be approve or reject",
            ));
        }
        let gate = if decision == "approve" {
            Some(rightkit_approvals::gate(d)?)
        } else {
            None
        };
        let status = tpl_read(ctx, draft_id)?["status"]
            .as_str()
            .unwrap_or("")
            .to_string();
        if (decision == "approve" && status == "rejected")
            || (decision == "reject" && status == "approved")
        {
            return Err(GenError::new(
                "decision_final",
                format!("Item {index} is already {status}; preflight a new batch to change it"),
            ));
        }
        plan.push((
            draft_id.to_string(),
            decision.to_string(),
            opt_string(d.get("asset")),
            gate,
        ));
    }
    for (draft_id, decision, asset, gate) in plan {
        if let Some(gate) = gate.as_ref() {
            approve_template_draft(ctx, &draft_id, gate)?;
        }
        let mut doc = tpl_read(ctx, &draft_id)?;
        if decision == "reject" {
            doc["status"] = json!("rejected");
            doc["rejected_at"] = json!(now_iso());
        }
        if let Some(a) = asset {
            doc["asset"] = json!(a);
        }
        tpl_write(ctx, &doc)?;
    }
    batch_view(ctx, &manifest)
}
