//! studio.contracts — the 12 image prompt contracts, their validator, the
//! deterministic renderers, and the form schema. Ported from
//! `tools/lib/image-prompt-contract.mjs`, `image-param-normalizer.mjs`,
//! `image-model-base-registry.mjs` and the Media Console form schema
//! (a7271cf `server/studio-image-form-schema.mjs`).
//!
//! The JSON contracts under `data/contracts/` are the originals, byte for
//! byte. The validator keeps the JS semantics (truthiness, `String(value)`
//! formatting, UTF-16 lengths) so the same fixtures give the same verdicts.
//!
//! Commands: `studio.contracts.{list,schema,validate,render,invoice,normalize,health}`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use rightkit_media_gen::{Context, GenError, GenResult};

pub const COMMANDS: &[&str] = &[
    "studio.contracts.list",
    "studio.contracts.schema",
    "studio.contracts.validate",
    "studio.contracts.render",
    "studio.contracts.invoice",
    "studio.contracts.normalize",
    "studio.contracts.health",
];

const CONTRACT_SOURCES: &[(&str, &str)] = &[
    (
        "flux-2-dev",
        include_str!("../data/contracts/flux-2-dev.json"),
    ),
    (
        "flux-2-pro",
        include_str!("../data/contracts/flux-2-pro.json"),
    ),
    (
        "gpt-image-2",
        include_str!("../data/contracts/gpt-image-2.json"),
    ),
    (
        "imagen-4-standard",
        include_str!("../data/contracts/imagen-4-standard.json"),
    ),
    (
        "imagen-4-ultra",
        include_str!("../data/contracts/imagen-4-ultra.json"),
    ),
    (
        "mj-v7-evolink",
        include_str!("../data/contracts/mj-v7-evolink.json"),
    ),
    (
        "nano-banana-pro",
        include_str!("../data/contracts/nano-banana-pro.json"),
    ),
    (
        "qwen-image",
        include_str!("../data/contracts/qwen-image.json"),
    ),
    (
        "recraft-v4-pro-vector",
        include_str!("../data/contracts/recraft-v4-pro-vector.json"),
    ),
    (
        "recraft-v4-raster",
        include_str!("../data/contracts/recraft-v4-raster.json"),
    ),
    (
        "recraft-v4-vector",
        include_str!("../data/contracts/recraft-v4-vector.json"),
    ),
    (
        "seedream-v4",
        include_str!("../data/contracts/seedream-v4.json"),
    ),
];
const INVOICE_SOURCE: &str = include_str!("../data/contracts/invoice-status.json");

// ── JS semantics helpers (shared with recipes.rs) ─────────────────────────

pub(crate) fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map_or(true, |f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `a || b || c`: the first truthy candidate.
pub(crate) fn first_truthy<'a>(candidates: &[Option<&'a Value>]) -> Option<&'a Value> {
    candidates.iter().flatten().copied().find(|v| truthy(v))
}

/// `a ?? b ?? c`: the first candidate that is neither absent nor null.
pub(crate) fn first_present<'a>(candidates: &[Option<&'a Value>]) -> Option<&'a Value> {
    candidates.iter().flatten().copied().find(|v| !v.is_null())
}

pub(crate) fn is_nullish(v: Option<&Value>) -> bool {
    v.map_or(true, Value::is_null)
}

fn js_number(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(f64::NAN);
    if f.fract() == 0.0 && f.abs() < 1e21 {
        format!("{}", f as i128)
    } else {
        format!("{f}")
    }
}

/// JS `String(value)`.
pub(crate) fn js_str(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => js_number(n),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                if x.is_null() {
                    String::new()
                } else {
                    js_str(x)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// A truthy value as the string `filter(Boolean).join` would see.
pub(crate) fn slot(v: Option<&Value>) -> Option<String> {
    v.filter(|x| truthy(x)).map(js_str)
}

pub(crate) fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

fn js_len(v: &Value) -> Option<usize> {
    match v {
        Value::String(s) => Some(utf16_len(s)),
        Value::Array(a) => Some(a.len()),
        _ => None,
    }
}

fn js_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

fn num_of(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

fn obj_get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|m| m.get(key))
}

fn str_of<'a>(v: &'a Value, key: &str) -> &'a str {
    obj_get(v, key).and_then(Value::as_str).unwrap_or("")
}

// ── Contract loading ──────────────────────────────────────────────────────

static CONTRACTS: OnceLock<BTreeMap<String, Value>> = OnceLock::new();
static INVOICE: OnceLock<Value> = OnceLock::new();

fn contracts_map() -> &'static BTreeMap<String, Value> {
    CONTRACTS.get_or_init(|| {
        CONTRACT_SOURCES
            .iter()
            .map(|(slug, text)| {
                let v = serde_json::from_str(text)
                    .unwrap_or_else(|e| panic!("embedded contract {slug} is invalid JSON: {e}"));
                (slug.to_string(), v)
            })
            .collect()
    })
}

fn invoice() -> &'static Value {
    INVOICE
        .get_or_init(|| serde_json::from_str(INVOICE_SOURCE).expect("embedded invoice-status.json"))
}

/// Model slugs that have a contract (single source of truth, sorted).
pub fn list_contracts() -> Vec<String> {
    contracts_map().keys().cloned().collect()
}

pub fn has_contract(slug: &str) -> bool {
    contracts_map().contains_key(slug)
}

pub fn load_contract(slug: &str) -> Result<&'static Value, ContractError> {
    contracts_map().get(slug).ok_or_else(|| {
        ContractError::single(
            slug,
            Violation::new(
                "contract_missing",
                None,
                Value::Null,
                format!("image-prompt-contract: no contract found for model_slug='{slug}'."),
            ),
        )
    })
}

pub fn get_invoice_status(tier_id: &str) -> Option<&'static Value> {
    obj_get(invoice(), "tiers").and_then(|t| t.get(tier_id))
}

// ── Violations ────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct Violation {
    pub rule_id: String,
    pub field: Option<String>,
    pub value: Value,
    pub message: String,
    pub context: Option<Value>,
}

impl Violation {
    fn new(rule_id: &str, field: Option<&str>, value: Value, message: String) -> Self {
        Self {
            rule_id: rule_id.to_string(),
            field: field.map(str::to_string),
            value,
            message,
            context: None,
        }
    }
    fn with_context(mut self, ctx: Value) -> Self {
        self.context = Some(ctx);
        self
    }
    pub fn to_json(&self) -> Value {
        json!({
            "rule_id": self.rule_id,
            "field": self.field,
            "value": self.value,
            "message": self.message,
            "context": self.context,
        })
    }
}

/// The JS `ContractViolation`: the first violation plus every other one.
#[derive(Debug, Clone)]
pub struct ContractError {
    pub model_slug: String,
    pub first: Violation,
    pub all: Vec<Violation>,
}

impl ContractError {
    fn single(model_slug: &str, v: Violation) -> Self {
        Self {
            model_slug: model_slug.to_string(),
            all: vec![v.clone()],
            first: v,
        }
    }
    pub fn message(&self) -> &str {
        &self.first.message
    }
    pub fn rule_id(&self) -> &str {
        &self.first.rule_id
    }
    pub fn to_json(&self) -> Value {
        json!({
            "ok": false,
            "model": self.model_slug,
            "rule_id": self.first.rule_id,
            "field": self.first.field,
            "message": self.first.message,
            "violations": self.all.iter().map(Violation::to_json).collect::<Vec<_>>(),
        })
    }
}

impl std::fmt::Display for ContractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.first.message)
    }
}

impl std::error::Error for ContractError {}

// ── Declarative rules ─────────────────────────────────────────────────────

/// `'references.length'` style path lookup.
fn resolve_path(payload: &Value, path: &str) -> Option<Value> {
    if path.is_empty() {
        return None;
    }
    let mut cur = payload.clone();
    for key in path.split('.') {
        if cur.is_null() {
            return None;
        }
        if key == "length" {
            if let Some(n) = js_len(&cur) {
                cur = json!(n);
                continue;
            }
        }
        cur = cur.get(key).cloned()?;
    }
    Some(cur)
}

fn throws_text(rule: &Value) -> &str {
    str_of(rule, "throws")
}

fn slug_of(contract: &Value) -> &str {
    obj_get(contract, "_meta")
        .map(|m| str_of(m, "model_slug"))
        .unwrap_or("")
}

fn rule_id(rule: &Value) -> &str {
    str_of(rule, "id")
}

fn rule_field(rule: &Value) -> &str {
    str_of(rule, "field")
}

fn run_enum_rule(rule: &Value, payload: &Value) -> Option<Violation> {
    let value = resolve_path(payload, rule_field(rule))?;
    if value.is_null() {
        return None;
    }
    let allowed = obj_get(rule, "enum").and_then(Value::as_array)?;
    if !allowed.iter().any(|a| js_eq(a, &value)) {
        let msg = throws_text(rule).replacen("{value}", &js_str(&value), 1);
        return Some(Violation::new(
            rule_id(rule),
            Some(rule_field(rule)),
            value,
            msg,
        ));
    }
    None
}

fn run_range_rule(rule: &Value, payload: &Value, contract: &Value) -> Option<Violation> {
    let value = resolve_path(payload, rule_field(rule))?;
    if value.is_null() {
        return None;
    }
    let field = rule_field(rule);
    let Some(n) = value.as_f64() else {
        let kind = match &value {
            Value::String(_) => "string",
            Value::Bool(_) => "boolean",
            _ => "object",
        };
        return Some(Violation::new(
            rule_id(rule),
            Some(field),
            value.clone(),
            format!(
                "{}: {field} must be numeric (got {kind})",
                slug_of(contract)
            ),
        ));
    };
    let below = obj_get(rule, "min")
        .and_then(Value::as_f64)
        .is_some_and(|m| n < m);
    let above = obj_get(rule, "max")
        .and_then(Value::as_f64)
        .is_some_and(|m| n > m);
    if below || above {
        let msg = throws_text(rule).replacen("{value}", &js_str(&value), 1);
        return Some(Violation::new(rule_id(rule), Some(field), value, msg));
    }
    None
}

fn run_length_rule(rule: &Value, payload: &Value, contract: &Value) -> Option<Violation> {
    let field = rule_field(rule);
    let min = obj_get(rule, "min").and_then(Value::as_f64);
    let max = obj_get(rule, "max").and_then(Value::as_f64);
    let value = resolve_path(payload, field).filter(|v| !v.is_null());
    let Some(value) = value else {
        if min.is_some_and(|m| m > 0.0) {
            let msg = throws_text(rule).replacen("{len}", "0", 1);
            return Some(Violation::new(rule_id(rule), Some(field), Value::Null, msg));
        }
        return None;
    };
    let Some(len) = js_len(&value) else {
        return Some(Violation::new(
            rule_id(rule),
            Some(field),
            value,
            format!("{}: {field} has no length", slug_of(contract)),
        ));
    };
    let len_f = len as f64;
    if min.is_some_and(|m| len_f < m) || max.is_some_and(|m| len_f > m) {
        let msg = throws_text(rule).replacen("{len}", &len.to_string(), 1);
        return Some(Violation::new(rule_id(rule), Some(field), value, msg));
    }
    None
}

fn run_regex_rule(rule: &Value, payload: &Value) -> Option<Violation> {
    let value = resolve_path(payload, rule_field(rule)).filter(|v| !v.is_null())?;
    let pattern = str_of(rule, "pattern");
    let re = regex::Regex::new(pattern).ok()?;
    if !re.is_match(&js_str(&value)) {
        let msg = throws_text(rule).replacen("{value}", &js_str(&value), 1);
        return Some(Violation::new(
            rule_id(rule),
            Some(rule_field(rule)),
            value,
            msg,
        ));
    }
    None
}

// ── Named handlers ────────────────────────────────────────────────────────

/// Handler names registered in the validator (for health checks).
pub const HANDLER_NAMES: &[&str] = &[
    "validateNbProReferenceArity",
    "validateFluxNegativePhrases",
    "validateSeedreamDeclaredCount",
    "validateMjV7Flags",
    "validateImagenAspectSnap",
    "validateImagenPromptRewriting",
    "validateGptImage2QualityRequired",
    "validateGptImage2CompressionFormat",
    "validateRecraftStyleEnum",
    "validateQwenSizeFormat",
];

fn ctx_flag(ctx: &Value, key: &str) -> bool {
    obj_get(ctx, key).is_some_and(truthy)
}

fn run_handler(name: &str, payload: &Value, contract: &Value, ctx: &Value) -> Option<Violation> {
    match name {
        "validateNbProReferenceArity" => h_reference_arity(payload, contract, ctx),
        "validateFluxNegativePhrases" => h_flux_negative(payload, contract, ctx),
        "validateSeedreamDeclaredCount" => h_seedream_count(payload, contract, ctx),
        "validateMjV7Flags" => h_mj_flags(payload, contract, ctx),
        "validateImagenAspectSnap" => h_imagen_snap(payload, contract),
        "validateImagenPromptRewriting" => h_imagen_rewriting(payload, contract, ctx),
        "validateGptImage2QualityRequired" => h_gpt_quality(payload, contract, ctx),
        "validateGptImage2CompressionFormat" => h_gpt_compression(payload, contract),
        "validateRecraftStyleEnum" => h_recraft_style(payload, contract),
        "validateQwenSizeFormat" => h_qwen_size(payload, contract),
        _ => None,
    }
}

fn h_reference_arity(payload: &Value, contract: &Value, ctx: &Value) -> Option<Violation> {
    let candidates = [
        obj_get(payload, "references"),
        obj_get(payload, "image_urls"),
        obj_get(payload, "imageUrls"),
        obj_get(ctx, "references"),
        obj_get(ctx, "image_urls"),
        obj_get(ctx, "imageUrls"),
        obj_get(ctx, "styleRefs"),
    ];
    let refs_len = first_truthy(&candidates).and_then(js_len).unwrap_or(0);
    let max_refs = obj_get(contract, "reference_syntax")
        .and_then(|r| obj_get(r, "max_references"))
        .and_then(Value::as_u64)
        .unwrap_or(14) as usize;
    let slug = slug_of(contract);
    if refs_len > max_refs {
        return Some(Violation::new(
            "reference_count_exceeds_max",
            Some("references"),
            json!(refs_len),
            format!("{slug}: {refs_len} references exceeds max {max_refs}"),
        ));
    }
    let prompt = obj_get(payload, "prompt")
        .filter(|v| truthy(v))
        .map(js_str)
        .unwrap_or_default();
    // `/@image(\d+)/gi` then `/@img(\d+)/gi`; `@image2` is not matched by the second.
    let lower = prompt.to_lowercase();
    let mut cited: Vec<u64> = Vec::new();
    for tag in ["@image", "@img"] {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(tag) {
            let start = from + pos + tag.len();
            let digits: String = lower[start..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if !digits.is_empty() {
                if let Ok(n) = digits.parse::<u64>() {
                    if !cited.contains(&n) {
                        cited.push(n);
                    }
                }
            }
            from = start;
        }
    }
    for n in cited {
        if n < 1 || n as usize > refs_len {
            return Some(Violation::new(
                "reference_arity_match",
                Some("prompt"),
                json!(format!("@image/@img{n}")),
                format!(
                    "{slug}: prompt cites @image{n} or @img{n} but references[{}] is missing (have {refs_len} references)",
                    n as i64 - 1
                ),
            ));
        }
    }
    None
}

fn utf16_offset(haystack: &str, byte_idx: usize) -> usize {
    utf16_len(&haystack[..byte_idx])
}

fn h_flux_negative(payload: &Value, contract: &Value, ctx: &Value) -> Option<Violation> {
    if ctx_flag(ctx, "allow_negative_phrases") {
        return None;
    }
    let policy = obj_get(contract, "negative_prompt_policy");
    let blocked = policy
        .and_then(|p| obj_get(p, "blocked_phrases"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let prompt = obj_get(payload, "prompt")
        .filter(|v| truthy(v))
        .map(js_str)
        .unwrap_or_default()
        .to_lowercase();
    for phrase in blocked.iter().filter_map(Value::as_str) {
        let needle = phrase.to_lowercase();
        if let Some(idx) = prompt.find(&needle) {
            let offset = utf16_offset(&prompt, idx);
            let table = policy.and_then(|p| obj_get(p, "rewrite_table"));
            let hint = table
                .and_then(|t| {
                    first_truthy(&[t.get(phrase.trim()), t.get(phrase.trim().to_lowercase())])
                })
                .map(js_str)
                .map(|h| format!(" Suggested rewrite: '{h}'."))
                .unwrap_or_default();
            return Some(Violation::new(
                "no_negative_phrases",
                Some("prompt"),
                json!(phrase),
                format!(
                    "{}: BFL requires positive_rewrite. Found '{phrase}' at offset {offset}.{hint}",
                    slug_of(contract)
                ),
            ));
        }
    }
    None
}

/// First `\ba series of (\d+)\b` (case-insensitive; input already lowercased).
fn find_series_count(lower: &str) -> Option<f64> {
    const NEEDLE: &str = "a series of ";
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let mut from = 0;
    while let Some(pos) = lower[from..].find(NEEDLE) {
        let at = from + pos;
        let boundary_before = lower[..at]
            .chars()
            .next_back()
            .map_or(true, |c| !is_word(c));
        if boundary_before {
            let rest = &lower[at + NEEDLE.len()..];
            let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
            let after = rest[digits.len()..].chars().next();
            if !digits.is_empty() && after.map_or(true, |c| !is_word(c)) {
                return digits.parse().ok();
            }
        }
        from = at + 1;
    }
    None
}

fn h_seedream_count(payload: &Value, contract: &Value, ctx: &Value) -> Option<Violation> {
    // The series-language requirement binds the rendered prompt; the pre-render
    // pass has only raw slots, so the renderer's own clause isn't there yet.
    // max_images itself is still checked by the explicit `range` rule.
    if ctx_flag(ctx, "_skip_post_render_rules") {
        return None;
    }
    let slug = slug_of(contract);
    let max_images = first_truthy(&[obj_get(payload, "max_images")])
        .cloned()
        .unwrap_or(json!(1));
    let max_n = num_of(&max_images).unwrap_or(f64::NAN);
    let endpoint = first_truthy(&[obj_get(payload, "endpoint_variant")])
        .map(js_str)
        .unwrap_or_else(|| "default".into());
    if max_n > 9.0 {
        return Some(Violation::new(
            "max_images_range",
            Some("max_images"),
            max_images.clone(),
            format!(
                "{slug}: max_images must be 1..9 (got {})",
                js_str(&max_images)
            ),
        ));
    }
    if endpoint == "sequential" || endpoint == "edit-sequential" {
        let prompt = obj_get(payload, "prompt")
            .filter(|v| truthy(v))
            .map(js_str)
            .unwrap_or_default()
            .to_lowercase();
        let Some(declared) = find_series_count(&prompt) else {
            let head: String = prompt.chars().take(80).collect();
            return Some(Violation::new(
                "sequential_endpoint_requires_series_lang",
                Some("prompt"),
                json!(head),
                format!("{slug}: sequential endpoint requires 'a series of N images' in prompt; not found"),
            ));
        };
        if declared != max_n {
            return Some(
                Violation::new(
                    "seedream_count_match",
                    Some("max_images"),
                    max_images.clone(),
                    format!(
                        "{slug}: max_images ({}) does not match prompt-declared count ({}). Sequential generation requires both to agree.",
                        js_str(&max_images),
                        js_number(&serde_json::Number::from_f64(declared).unwrap_or_else(|| 0.into()))
                    ),
                )
                .with_context(json!({"declared": declared, "max_images": max_images})),
            );
        }
    }
    None
}

fn is_ratio(s: &str) -> bool {
    match s.split_once(':') {
        Some((w, h)) => {
            !w.is_empty()
                && !h.is_empty()
                && w.chars().all(|c| c.is_ascii_digit())
                && h.chars().all(|c| c.is_ascii_digit())
        }
        None => false,
    }
}

fn h_mj_flags(payload: &Value, contract: &Value, ctx: &Value) -> Option<Violation> {
    let slug = slug_of(contract);
    let ar = obj_get(payload, "ar").filter(|v| truthy(v));
    let Some(ar) = ar else {
        return Some(Violation::new(
            "ar_required_well_formed",
            Some("ar"),
            obj_get(payload, "ar").cloned().unwrap_or(Value::Null),
            format!("{slug}: --ar flag is required"),
        ));
    };
    if !is_ratio(&js_str(ar)) {
        return Some(Violation::new(
            "ar_required_well_formed",
            Some("ar"),
            ar.clone(),
            format!("{slug}: --ar must match W:H pattern (got '{}')", js_str(ar)),
        ));
    }
    if let Some(sw) = obj_get(payload, "sw").filter(|v| !v.is_null()) {
        if num_of(sw).is_some_and(|n| !(0.0..=1000.0).contains(&n)) {
            return Some(Violation::new(
                "sw_range_when_sref",
                Some("sw"),
                sw.clone(),
                format!(
                    "{slug}: --sw must be 0..1000 when --sref is set (got {})",
                    js_str(sw)
                ),
            ));
        }
    }
    if let Some(cref) = obj_get(payload, "cref").filter(|v| truthy(v)) {
        if !ctx_flag(ctx, "allow_legacy_cref") {
            return Some(Violation::new(
                "v7_prefers_omni_over_cref",
                Some("cref"),
                cref.clone(),
                format!("{slug}: V7 character refs should use --omni, not --cref. Set ctx.allow_legacy_cref=true to override."),
            ));
        }
    }
    None
}

fn h_imagen_snap(payload: &Value, contract: &Value) -> Option<Violation> {
    let ar = obj_get(payload, "aspectRatio").filter(|v| !v.is_null())?;
    let text = js_str(ar);
    if ["1:1", "3:4", "4:3", "9:16", "16:9"].contains(&text.as_str()) {
        return None;
    }
    let snap = match text.as_str() {
        "2:3" => "3:4",
        "3:2" => "4:3",
        "4:5" => "3:4",
        "5:4" => "4:3",
        "21:9" => "16:9",
        _ => "1:1",
    };
    Some(
        Violation::new(
            "aspect_snap_handler",
            Some("aspectRatio"),
            ar.clone(),
            format!(
                "{}: aspect '{text}' not supported. Suggested snap: '{snap}'",
                slug_of(contract)
            ),
        )
        .with_context(json!({"snap": snap})),
    )
}

fn h_imagen_rewriting(payload: &Value, contract: &Value, ctx: &Value) -> Option<Violation> {
    if obj_get(payload, "promptRewriting") == Some(&Value::Bool(false))
        && !ctx_flag(ctx, "allow_quality_degrade")
    {
        return Some(Violation::new(
            "prompt_rewriting_explicit_disable",
            Some("promptRewriting"),
            json!(false),
            format!(
                "{}: promptRewriting=false requires ctx.allow_quality_degrade=true (disabling impacts quality per Vertex AI docs)",
                slug_of(contract)
            ),
        ));
    }
    None
}

fn h_gpt_quality(payload: &Value, contract: &Value, ctx: &Value) -> Option<Violation> {
    let slug = slug_of(contract);
    let q = obj_get(payload, "quality").filter(|v| !v.is_null());
    let allow_auto = obj_get(ctx, "allow_auto_quality") == Some(&Value::Bool(true));
    match q {
        None => Some(Violation::new(
            "quality_required_explicit",
            Some("quality"),
            Value::Null,
            format!("{slug}: quality must be explicitly low|medium|high. Auto requires ctx.allow_auto_quality=true (prevents surprise spend on high tier)."),
        )),
        Some(q) if q.as_str() == Some("auto") && !allow_auto => Some(Violation::new(
            "quality_required_explicit",
            Some("quality"),
            q.clone(),
            format!("{slug}: quality='auto' requires ctx.allow_auto_quality=true (cost varies by resolved tier; default to explicit low/medium/high in production)."),
        )),
        _ => None,
    }
}

fn h_gpt_compression(payload: &Value, contract: &Value) -> Option<Violation> {
    let comp = obj_get(payload, "output_compression").filter(|v| !v.is_null())?;
    let fmt = first_truthy(&[obj_get(payload, "output_format")])
        .map(js_str)
        .unwrap_or_else(|| "png".into());
    if fmt == "png" {
        return Some(Violation::new(
            "compression_only_with_lossy",
            Some("output_compression"),
            comp.clone(),
            format!(
                "{}: output_compression only valid when output_format is jpeg or webp (current: png)",
                slug_of(contract)
            ),
        ));
    }
    None
}

fn h_recraft_style(payload: &Value, contract: &Value) -> Option<Violation> {
    const ALLOWED: [&str; 4] = [
        "realistic_image",
        "digital_illustration",
        "vector_illustration",
        "icon",
    ];
    let slug = slug_of(contract);
    let allowed = ALLOWED.join("|");
    let style = obj_get(payload, "style").filter(|v| truthy(v));
    let Some(style) = style else {
        return Some(Violation::new(
            "style_required_in_enum",
            Some("style"),
            obj_get(payload, "style").cloned().unwrap_or(Value::Null),
            format!("{slug}: style is required (one of {allowed})"),
        ));
    };
    let text = js_str(style);
    let base = text.split('/').next().unwrap_or("");
    if !ALLOWED.contains(&base) {
        return Some(Violation::new(
            "style_required_in_enum",
            Some("style"),
            style.clone(),
            format!(
                "{slug}: style '{text}' must be one of {allowed} (substyle after / is allowed)"
            ),
        ));
    }
    if slug == "recraft-v4-raster" && base == "vector_illustration" {
        return Some(Violation::new(
            "vector_style_routes_correctly",
            Some("style"),
            style.clone(),
            format!("{slug}: vector_illustration style requested against raster backend; route to recraft-v4-vector contract instead"),
        ));
    }
    if let Some(colors) = obj_get(payload, "colors").and_then(Value::as_array) {
        for (i, c) in colors.iter().enumerate() {
            let ok = obj_get(c, "rgb")
                .and_then(Value::as_array)
                .is_some_and(|rgb| {
                    rgb.len() == 3
                        && rgb.iter().all(|v| {
                            v.as_f64()
                                .is_some_and(|n| (0.0..=255.0).contains(&n) && n.fract() == 0.0)
                        })
                });
            if !ok {
                return Some(Violation::new(
                    "colors_rgb_format",
                    Some(&format!("colors[{i}]")),
                    c.clone(),
                    format!(
                        "{slug}: colors[{i}] must be {{rgb: [R,G,B]}} with R/G/B integers 0..255"
                    ),
                ));
            }
        }
    }
    None
}

fn h_qwen_size(payload: &Value, contract: &Value) -> Option<Violation> {
    let size = obj_get(payload, "size").filter(|v| !v.is_null())?;
    let s = js_str(size);
    let slug = slug_of(contract);
    if s.contains('x') && !s.contains('*') {
        return Some(Violation::new(
            "size_format_asterisk",
            Some("size"),
            size.clone(),
            format!("{slug}: size '{s}' must use asterisk (e.g. 1024*1024), not 'x' (per WaveSpeed Qwen quickstart)"),
        ));
    }
    let well_formed = s.split_once('*').is_some_and(|(w, h)| {
        !w.is_empty()
            && !h.is_empty()
            && w.chars().all(|c| c.is_ascii_digit())
            && h.chars().all(|c| c.is_ascii_digit())
    });
    if !well_formed {
        return Some(Violation::new(
            "size_format_asterisk",
            Some("size"),
            size.clone(),
            format!("{slug}: size '{s}' must match W*H pattern with asterisk separator"),
        ));
    }
    None
}

// ── Synthesized metadata rules ────────────────────────────────────────────

fn synthesize_meta_rules(contract: &Value) -> Vec<Value> {
    let slug = slug_of(contract);
    let mut out = Vec::new();
    for (block, block_required) in [("required_fields", true), ("optional_fields", false)] {
        let Some(fields) = obj_get(contract, block).and_then(Value::as_object) else {
            continue;
        };
        for (name, spec) in fields {
            let Some(spec_obj) = spec.as_object() else {
                continue;
            };
            let required = match spec_obj.get("required") {
                Some(Value::Bool(false)) => false,
                Some(Value::Bool(true)) => true,
                _ => block_required,
            };
            let ty = spec_obj.get("type").and_then(Value::as_str).unwrap_or("");
            let max = spec_obj
                .get("max_length")
                .or_else(|| spec_obj.get("max_items"))
                .filter(|v| !v.is_null())
                .cloned();
            let min = spec_obj
                .get("min_length")
                .filter(|v| !v.is_null())
                .cloned()
                .or_else(|| (required && ty != "boolean").then(|| json!(1)));
            if max.is_some() || min.is_some() {
                let mut rule = Map::new();
                rule.insert("id".into(), json!(format!("meta_{name}_length")));
                rule.insert("kind".into(), json!("length"));
                rule.insert("field".into(), json!(name));
                if let Some(m) = &min {
                    rule.insert("min".into(), m.clone());
                }
                if let Some(m) = &max {
                    rule.insert("max".into(), m.clone());
                }
                let lo = min.as_ref().map(js_str).unwrap_or_else(|| "0".into());
                let hi = max.as_ref().map(js_str).unwrap_or_else(|| "∞".into());
                rule.insert(
                    "throws".into(),
                    json!(format!("{slug}: {name} length {{len}} not in [{lo}, {hi}]")),
                );
                out.push(Value::Object(rule));
            }
            let smin = spec_obj.get("min").filter(|v| !v.is_null());
            let smax = spec_obj.get("max").filter(|v| !v.is_null());
            if (ty == "integer" || ty == "number") && (smin.is_some() || smax.is_some()) {
                let mut rule = Map::new();
                rule.insert("id".into(), json!(format!("meta_{name}_range")));
                rule.insert("kind".into(), json!("range"));
                rule.insert("field".into(), json!(name));
                if let Some(m) = smin {
                    rule.insert("min".into(), m.clone());
                }
                if let Some(m) = smax {
                    rule.insert("max".into(), m.clone());
                }
                let lo = smin.map(js_str).unwrap_or_else(|| "-∞".into());
                let hi = smax.map(js_str).unwrap_or_else(|| "∞".into());
                rule.insert(
                    "throws".into(),
                    json!(format!(
                        "{slug}: {name} value {{value}} not in [{lo}, {hi}]"
                    )),
                );
                out.push(Value::Object(rule));
            }
            if ty == "enum" {
                if let Some(en) = spec_obj.get("enum").and_then(Value::as_array) {
                    let joined = en.iter().map(js_str).collect::<Vec<_>>().join("|");
                    out.push(json!({
                        "id": format!("meta_{name}_enum"),
                        "kind": "enum",
                        "field": name,
                        "enum": en,
                        "throws": format!("{slug}: {name} '{{value}}' not in [{joined}]"),
                    }));
                }
            }
        }
    }
    out
}

const INTERNAL_PAYLOAD_FIELDS: &[&str] = &[
    "apiKey",
    "aspect",
    "aspect_ratio",
    "base_model",
    "character_id",
    "intent",
    "lora_policy",
    "megapixels",
    "model",
    "outPath",
    "skip_contract",
    "verbose",
];

fn canonical_input_field(field: &str) -> &str {
    match field {
        "callbackUrl" => "callback_url",
        "callBackUrl" => "callBackUrl",
        "google_search" => "googleSearch",
        "imageUrls" => "image_urls",
        "negativePrompt" => "negative_prompt",
        "numInferenceSteps" => "num_inference_steps",
        "output_format" => "output_format",
        "outputFormat" => "outputFormat",
        "size" => "image_size",
        "styleRef" | "styleRefs" => "image_urls",
        other => other,
    }
}

fn contract_field_names(contract: &Value) -> Vec<&str> {
    let mut names = Vec::new();
    for block in ["required_fields", "optional_fields"] {
        if let Some(m) = obj_get(contract, block).and_then(Value::as_object) {
            names.extend(m.keys().map(String::as_str));
        }
    }
    names
}

fn has_reference_support(contract: &Value) -> bool {
    let fields = contract_field_names(contract);
    fields.contains(&"references")
        || fields.contains(&"image_urls")
        || obj_get(contract, "reference_syntax")
            .and_then(|r| obj_get(r, "max_references"))
            .and_then(Value::as_f64)
            .is_some_and(|n| n > 0.0)
}

fn is_meaningful(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        _ => true,
    }
}

fn run_unknown_field_rule(payload: &Value, contract: &Value) -> Option<Violation> {
    let fields = contract_field_names(contract);
    let supports_refs = has_reference_support(contract);
    let model = slug_of(contract);
    for (field, value) in payload.as_object()? {
        if !is_meaningful(value) || INTERNAL_PAYLOAD_FIELDS.contains(&field.as_str()) {
            continue;
        }
        let is_ref_field = [
            "references",
            "image_urls",
            "imageUrls",
            "styleRefs",
            "styleRef",
        ]
        .contains(&field.as_str());
        if is_ref_field {
            if supports_refs {
                continue;
            }
        } else {
            if field == "loras" && model == "flux-2-dev" {
                continue;
            }
            if fields.contains(&field.as_str()) || fields.contains(&canonical_input_field(field)) {
                continue;
            }
        }
        return Some(Violation::new(
            "unknown_provider_field",
            Some(field),
            value.clone(),
            format!("{model}: field '{field}' is not accepted by this model route"),
        ));
    }
    None
}

// ── Validator core ────────────────────────────────────────────────────────

/// Validate `payload` against the model's contract. `ctx` carries the JS
/// override flags (`allow_negative_phrases`, `allow_legacy_cref`, ...).
pub fn validate_contract(slug: &str, payload: &Value, ctx: &Value) -> Result<(), ContractError> {
    let contract = load_contract(slug)?;
    let mut violations: Vec<Violation> = Vec::new();
    if !ctx_flag(ctx, "_skip_post_render_rules") && !ctx_flag(ctx, "_skip_unknown_fields") {
        violations.extend(run_unknown_field_rule(payload, contract));
    }
    let explicit: Vec<Value> = obj_get(contract, "validator_rules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let explicit_fields: Vec<&str> = explicit
        .iter()
        .filter_map(|r| obj_get(r, "field").and_then(Value::as_str))
        .collect();
    let meta = synthesize_meta_rules(contract);
    let merged = meta
        .iter()
        .filter(|r| !explicit_fields.contains(&rule_field(r)))
        .chain(explicit.iter());
    for rule in merged {
        let result = match str_of(rule, "kind") {
            "handler" => {
                let handler = str_of(rule, "handler");
                if !HANDLER_NAMES.contains(&handler) {
                    return Err(ContractError::single(
                        slug,
                        Violation::new(
                            "handler_missing",
                            None,
                            Value::Null,
                            format!(
                                "image-prompt-contract: handler '{handler}' referenced by rule '{}' is not registered",
                                rule_id(rule)
                            ),
                        ),
                    ));
                }
                run_handler(handler, payload, contract, ctx)
            }
            "enum" => run_enum_rule(rule, payload),
            "range" => run_range_rule(rule, payload, contract),
            "length" => run_length_rule(rule, payload, contract),
            "regex" => run_regex_rule(rule, payload),
            other => {
                return Err(ContractError::single(
                    slug,
                    Violation::new(
                        "rule_kind_unknown",
                        None,
                        Value::Null,
                        format!(
                            "image-prompt-contract: unknown rule kind '{other}' in rule '{}'",
                            rule_id(rule)
                        ),
                    ),
                ))
            }
        };
        violations.extend(result);
    }
    match violations.first().cloned() {
        None => Ok(()),
        Some(first) => Err(ContractError {
            model_slug: slug.to_string(),
            first,
            all: violations,
        }),
    }
}

// ── Renderers ─────────────────────────────────────────────────────────────

/// `{prompt, params, meta}` as the backend adapter consumes it.
#[derive(Debug, Clone)]
pub struct Rendered {
    pub prompt: String,
    pub params: Map<String, Value>,
    pub meta: Value,
}

impl Rendered {
    pub fn to_json(&self) -> Value {
        json!({"prompt": self.prompt, "params": self.params, "meta": self.meta})
    }
}

fn join_slots(parts: &[Option<String>], sep: &str) -> String {
    parts
        .iter()
        .flatten()
        .cloned()
        .collect::<Vec<_>>()
        .join(sep)
}

fn put_if_not_null(m: &mut Map<String, Value>, key: &str, v: Option<&Value>) {
    if let Some(v) = v.filter(|v| !v.is_null()) {
        m.insert(key.into(), v.clone());
    }
}

fn put_if_truthy(m: &mut Map<String, Value>, key: &str, v: Option<&Value>) {
    if let Some(v) = v.filter(|v| truthy(v)) {
        m.insert(key.into(), v.clone());
    }
}

fn or_default(v: Option<&Value>, default: Value) -> Value {
    v.filter(|x| truthy(x)).cloned().unwrap_or(default)
}

fn render_nb_pro(i: &Value) -> Rendered {
    let g = |k: &str| obj_get(i, k);
    let lead =
        slot(first_truthy(&[g("subject_with_adjectives"), g("subject")])).unwrap_or_default();
    let action = slot(g("action")).map(|a| format!("doing {a}"));
    let place = slot(g("location_context"))
        .map(|l| format!("in {l}"))
        .or_else(|| slot(g("scene")).map(|s| format!("in {s}")));
    let slots = join_slots(&[Some(lead).filter(|s| !s.is_empty()), action, place], " ");
    let tail = join_slots(
        &[
            slot(first_truthy(&[
                g("composition_or_camera_angle"),
                g("composition"),
            ])),
            slot(first_truthy(&[g("lighting_or_atmosphere"), g("lighting")])),
            slot(first_truthy(&[g("style_or_media"), g("style")])),
        ],
        ". ",
    );
    let prompt = join_slots(
        &[
            Some(slots).filter(|s| !s.is_empty()),
            Some(tail.clone()).filter(|s| !s.is_empty()),
        ],
        ". ",
    );
    let mut params = Map::new();
    params.insert(
        "resolution".into(),
        or_default(g("resolution"), json!("2K")),
    );
    params.insert("aspect".into(), or_default(g("aspect"), json!("1:1")));
    Rendered {
        prompt: if tail.is_empty() {
            prompt
        } else {
            format!("{prompt}.")
        },
        params,
        meta: json!({"renderer": "nb_pro_default", "references": g("references").cloned().filter(truthy).unwrap_or(json!([]))}),
    }
}

/// The params the served flux-2-pro route (fal-ai/flux-2-max) accepts, and the warnings for the
/// rest. The route declares only `prompt` and `megapixels`, and the engine refuses any other param
/// key, null or not. So only megapixels (when the caller gives it) is sent: no image_size, no seed,
/// no image_urls. A supplied seed or image_size is dropped by the route, so it is reported as a
/// warning rather than dropped silently. Shared by the contract renderer and the raw-prompt recipe
/// path, so both send the same params and report the same warnings. `i` is the caller's params or
/// internal brief (an object).
pub(crate) fn flux_pro_route_params(i: &Value) -> (Map<String, Value>, Vec<String>) {
    let g = |k: &str| obj_get(i, k);
    let mut params = Map::new();
    let mut warnings: Vec<String> = Vec::new();
    if let Some(mp) = g("megapixels").filter(|v| !v.is_null()) {
        params.insert("megapixels".into(), mp.clone());
        warnings.push(
            "flux-2-pro: megapixels sets the price estimate only; the served route marks it send:false, so fal receives no size".to_string(),
        );
    }
    if g("seed").is_some_and(|v| !v.is_null()) {
        warnings.push("flux-2-pro: seed was supplied but not sent; the served fal-ai/flux-2-max route takes no seed, so this render is not seeded".to_string());
    }
    if g("image_size").is_some_and(|v| !v.is_null()) {
        warnings.push("flux-2-pro: image_size was supplied but not sent; the served fal-ai/flux-2-max route takes megapixels only".to_string());
    }
    (params, warnings)
}

fn render_flux(i: &Value, contract: &Value) -> Result<Rendered, ContractError> {
    let g = |k: &str| obj_get(i, k);
    let policy = obj_get(contract, "negative_prompt_policy");
    let table = policy.and_then(|p| obj_get(p, "rewrite_table"));
    let mut positivized: Vec<String> = Vec::new();
    for phrase in g("negative_intent")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let text = js_str(&phrase);
        let lower = text.to_lowercase();
        let lower = lower.trim();
        let hit = table.and_then(|t| first_truthy(&[t.get(lower), t.get(text.trim())]));
        match hit {
            Some(h) => positivized.push(js_str(h)),
            None => {
                return Err(ContractError::single(
                    "flux-2-pro",
                    Violation::new(
                        "negative_rewrite_missing",
                        None,
                        phrase,
                        format!("flux-2-pro: negative_intent '{text}' has no positive_rewrite in contract.rewrite_table. Add it or rewrite caller-side."),
                    ),
                ))
            }
        }
    }
    let slug = slug_of(contract);
    let is_dev = slug == "flux-2-dev";
    let is_pro = slug == "flux-2-pro";
    let mut parts = vec![
        slot(g("subject")),
        slot(g("action")),
        slot(g("style")),
        slot(first_truthy(&[g("context"), g("scene")])),
    ];
    parts.extend(
        positivized
            .iter()
            .map(|p| Some(p.clone()).filter(|s| !s.is_empty())),
    );
    let prompt = join_slots(&parts, ", ");
    let (mut params, warnings) = if is_pro {
        flux_pro_route_params(i)
    } else {
        let mut params = Map::new();
        params.insert(
            "image_size".into(),
            or_default(g("image_size"), json!("portrait_16_9")),
        );
        put_if_not_null(&mut params, "seed", g("seed"));
        (params, Vec::new())
    };
    if is_dev {
        put_if_not_null(&mut params, "num_inference_steps", g("num_inference_steps"));
        put_if_not_null(&mut params, "guidance_scale", g("guidance_scale"));
        put_if_not_null(&mut params, "num_images", g("num_images"));
        put_if_truthy(&mut params, "acceleration", g("acceleration"));
        put_if_not_null(
            &mut params,
            "enable_prompt_expansion",
            g("enable_prompt_expansion"),
        );
    }
    Ok(Rendered {
        prompt,
        params,
        meta: json!({"renderer": "flux_default", "positivized_count": positivized.len(), "warnings": warnings}),
    })
}

fn render_qwen(i: &Value) -> Rendered {
    let g = |k: &str| obj_get(i, k);
    let prompt = join_slots(
        &[
            slot(g("subject")),
            slot(g("action")),
            slot(first_truthy(&[g("style_modifiers"), g("style")])),
            slot(g("orientation_hint")),
        ],
        ", ",
    );
    let mut params = Map::new();
    params.insert("size".into(), or_default(g("size"), json!("1024*1024")));
    put_if_not_null(&mut params, "seed", g("seed"));
    put_if_truthy(&mut params, "output_format", g("output_format"));
    put_if_not_null(&mut params, "enable_sync_mode", g("enable_sync_mode"));
    put_if_not_null(
        &mut params,
        "enable_base64_output",
        g("enable_base64_output"),
    );
    Rendered {
        prompt,
        params,
        meta: json!({"renderer": "qwen_default"}),
    }
}

fn render_gpt_image_2(i: &Value) -> Rendered {
    let g = |k: &str| obj_get(i, k);
    let prompt = join_slots(
        &[slot(g("subject")), slot(g("context")), slot(g("style"))],
        ". ",
    );
    let mut params = Map::new();
    // `quality: internal.quality` is always present, undefined serializes away.
    put_if_not_null(&mut params, "quality", g("quality"));
    params.insert("size".into(), or_default(g("size"), json!("1024x1024")));
    put_if_truthy(&mut params, "resolution", g("resolution"));
    params.insert("n".into(), or_default(g("n"), json!(1)));
    put_if_truthy(&mut params, "callback_url", g("callback_url"));
    if g("image_urls")
        .and_then(Value::as_array)
        .is_some_and(|a| !a.is_empty())
    {
        params.insert(
            "image_urls".into(),
            g("image_urls").cloned().unwrap_or(Value::Null),
        );
    }
    Rendered {
        prompt,
        params,
        meta: json!({"renderer": "gpt_image_2_default"}),
    }
}

fn render_recraft(i: &Value) -> Rendered {
    let g = |k: &str| obj_get(i, k);
    let prompt = join_slots(
        &[
            slot(g("subject")),
            slot(g("composition")),
            slot(first_truthy(&[g("style_descriptor"), g("style")])),
        ],
        ", ",
    );
    let mut params = Map::new();
    params.insert("size".into(), or_default(g("size"), json!("1024*1024")));
    Rendered {
        prompt,
        params,
        meta: json!({"renderer": "recraft_default"}),
    }
}

fn render_imagen(i: &Value) -> Rendered {
    let g = |k: &str| obj_get(i, k);
    let prompt = join_slots(
        &[
            slot(g("subject")),
            slot(first_truthy(&[g("context_and_background"), g("scene")])),
            slot(g("style")),
        ],
        ". ",
    );
    let mut params = Map::new();
    if let Some(neg) = first_truthy(&[g("negative_prompt"), g("negativePrompt")]) {
        params.insert("negative_prompt".into(), neg.clone());
    }
    put_if_not_null(&mut params, "seed", g("seed"));
    put_if_truthy(&mut params, "resolution", g("resolution"));
    put_if_not_null(&mut params, "num_images", g("num_images"));
    put_if_not_null(
        &mut params,
        "enable_base64_output",
        g("enable_base64_output"),
    );
    Rendered {
        prompt,
        params,
        meta: json!({"renderer": "imagen_default"}),
    }
}

fn render_seedream(i: &Value) -> Rendered {
    let g = |k: &str| obj_get(i, k);
    let series = first_truthy(&[g("series_count"), g("max_images")])
        .cloned()
        .unwrap_or(json!(1));
    let endpoint = first_truthy(&[g("endpoint_variant")])
        .map(js_str)
        .unwrap_or_else(|| "default".into());
    let mut prompt = join_slots(
        &[
            slot(g("action")),
            slot(g("object")),
            slot(g("target_feature")),
        ],
        " ",
    );
    if endpoint == "sequential" || endpoint == "edit-sequential" {
        let marker = format!("a series of {}", js_str(&series)).to_lowercase();
        if !prompt.to_lowercase().contains(&marker) {
            prompt = format!("{prompt}, a series of {} images", js_str(&series));
            if let Some(locks) = g("style_locks")
                .and_then(Value::as_array)
                .filter(|a| !a.is_empty())
            {
                let joined = locks.iter().map(js_str).collect::<Vec<_>>().join(", ");
                prompt = format!("{prompt}, {joined}");
            }
        }
    }
    let mut params = Map::new();
    params.insert("size".into(), or_default(g("size"), json!("2048*2048")));
    params.insert("endpoint_variant".into(), json!(endpoint));
    params.insert("max_images".into(), series.clone());
    params.insert(
        "enable_base64_output".into(),
        or_default(g("enable_base64_output"), json!(false)),
    );
    params.insert(
        "enable_sync_mode".into(),
        or_default(g("enable_sync_mode"), json!(false)),
    );
    Rendered {
        prompt,
        params,
        meta: json!({"renderer": "seedream_v4", "endpoint_variant": endpoint, "declared_series_count": series}),
    }
}

fn render_mj(i: &Value, ctx: &Value) -> Result<Rendered, ContractError> {
    let g = |k: &str| obj_get(i, k);
    let body = join_slots(
        &[
            slot(g("subject")),
            slot(g("environment")),
            slot(g("lighting")),
            slot(g("style")),
        ],
        ", ",
    );
    let ar = or_default(g("ar"), json!("1:1"));
    let mut flags = vec![format!("--ar {}", js_str(&ar))];
    if let Some(s) = g("stylize").filter(|v| !v.is_null()) {
        flags.push(format!("--stylize {}", js_str(s)));
    }
    for r in g("references")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let ty = obj_get(&r, "type").map(js_str).unwrap_or_default();
        let value = obj_get(&r, "value").map(js_str).unwrap_or_default();
        match ty.as_str() {
            "style" => {
                flags.push(format!("--sref {value}"));
                if let Some(w) = obj_get(&r, "weight").filter(|v| !v.is_null()) {
                    flags.push(format!("--sw {}", js_str(w)));
                }
            }
            "character_v7" | "omni" => flags.push(format!("--omni {value}")),
            "character_v6" if ctx_flag(ctx, "allow_legacy_cref") => flags.push(format!("--cref {value}")),
            "image" => {
                let w = or_default(obj_get(&r, "weight"), json!(1));
                flags.push(format!("--iw {}", js_str(&w)));
            }
            _ => {
                return Err(ContractError::single(
                    "mj-v7-evolink",
                    Violation::new(
                        "reference_type_ambiguous",
                        None,
                        r.clone(),
                        format!("mj-v7: ambiguous reference type '{}'. Must be one of style|character_v7|omni|image|character_v6 (with allow_legacy_cref).", js_str(obj_get(&r, "type").unwrap_or(&Value::Null))),
                    ),
                ))
            }
        }
    }
    if let Some(no) = g("no").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        flags.push(format!(
            "--no {}",
            no.iter().map(js_str).collect::<Vec<_>>().join(", ")
        ));
    }
    let mut params = Map::new();
    params.insert("ar".into(), ar);
    put_if_not_null(&mut params, "stylize", g("stylize"));
    params.insert("speed".into(), or_default(g("speed"), json!("fast")));
    Ok(Rendered {
        prompt: format!("{body} {}", flags.join(" ")).trim().to_string(),
        params,
        meta: json!({"renderer": "mj_v7_flag_suffix", "flag_count": flags.len()}),
    })
}

/// Validate the internal brief, render it with the contract's renderer, then
/// re-validate the rendered payload (the adapter-boundary safety net).
pub fn render_contract(
    slug: &str,
    internal: &Value,
    ctx: &Value,
) -> Result<Rendered, ContractError> {
    let contract = load_contract(slug)?;
    let mut for_validate = internal.clone();
    if !obj_get(&for_validate, "prompt").is_some_and(truthy) {
        let keys = [
            "subject",
            "action",
            "style",
            "context",
            "scene",
            "environment",
            "lighting",
            "subject_with_adjectives",
            "location_context",
            "composition_or_camera_angle",
            "lighting_or_atmosphere",
            "style_or_media",
            "composition",
            "style_descriptor",
            "style_modifiers",
            "orientation_hint",
            "context_and_background",
            "object",
            "target_feature",
        ];
        let synthetic = keys
            .iter()
            .filter_map(|k| slot(obj_get(internal, k)))
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        if let Some(m) = for_validate.as_object_mut() {
            m.insert("prompt".into(), json!(synthetic));
        }
    }
    let mut pre_ctx = ctx.as_object().cloned().unwrap_or_default();
    pre_ctx.insert("_skip_post_render_rules".into(), json!(true));
    validate_contract(slug, &for_validate, &Value::Object(pre_ctx))?;

    let renderer_id = obj_get(contract, "official_prompt_template")
        .map(|t| str_of(t, "_renderer_id"))
        .unwrap_or("");
    let out = match renderer_id {
        "nb_pro_default" => render_nb_pro(internal),
        "flux_default" => render_flux(internal, contract)?,
        "qwen_default" => render_qwen(internal),
        "gpt_image_2_default" => render_gpt_image_2(internal),
        "recraft_default" => render_recraft(internal),
        "imagen_default" => render_imagen(internal),
        "seedream_v4" => render_seedream(internal),
        "mj_v7_flag_suffix" => render_mj(internal, ctx)?,
        other => {
            return Err(ContractError::single(
                slug,
                Violation::new(
                    "renderer_missing",
                    None,
                    Value::Null,
                    format!("image-prompt-contract: renderer '{other}' for {slug} not registered"),
                ),
            ))
        }
    };
    let mut post = out.params.clone();
    post.insert("prompt".into(), json!(out.prompt));
    if let Some(r) = obj_get(internal, "references") {
        post.insert("references".into(), r.clone());
    }
    let mut post_ctx = ctx.as_object().cloned().unwrap_or_default();
    post_ctx.insert("_post_render".into(), json!(true));
    validate_contract(slug, &Value::Object(post), &Value::Object(post_ctx))?;
    Ok(out)
}

// ── Health ────────────────────────────────────────────────────────────────

pub fn contract_health() -> Value {
    let mut contracts = Map::new();
    let mut missing_handlers = Vec::new();
    let mut invoice_unverified = Vec::new();
    for slug in list_contracts() {
        match load_contract(&slug) {
            Ok(c) => {
                let tier_id = obj_get(c, "_meta")
                    .and_then(|m| obj_get(m, "pricing_tier_id"))
                    .and_then(Value::as_str);
                if let Some(tier) = tier_id.and_then(get_invoice_status) {
                    if !obj_get(tier, "invoice_verified").is_some_and(truthy) {
                        invoice_unverified.push(format!("{slug} → {}", tier_id.unwrap_or("")));
                    }
                }
                for rule in obj_get(c, "validator_rules")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if str_of(rule, "kind") == "handler"
                        && !HANDLER_NAMES.contains(&str_of(rule, "handler"))
                    {
                        missing_handlers.push(format!(
                            "{slug}.{} → {}",
                            rule_id(rule),
                            str_of(rule, "handler")
                        ));
                    }
                }
                contracts.insert(
                    slug.clone(),
                    json!({"ok": true, "version": obj_get(c, "_meta").and_then(|m| obj_get(m, "version")), "tier_id": tier_id}),
                );
            }
            Err(e) => {
                contracts.insert(slug.clone(), json!({"ok": false, "error": e.message()}));
            }
        }
    }
    json!({"contracts": contracts, "missing_handlers": missing_handlers, "invoice_unverified": invoice_unverified})
}

// ── Param normalizer (image-param-normalizer.mjs) ─────────────────────────

fn reference_to_url(r: &Value) -> Option<String> {
    match r {
        Value::Null => None,
        Value::String(s) => Some(s.clone()).filter(|s| !s.is_empty()),
        Value::Object(_) => ["url", "href", "src"]
            .iter()
            .filter_map(|k| obj_get(r, k).and_then(Value::as_str))
            .find(|s| !s.is_empty())
            .map(str::to_string),
        _ => None,
    }
}

fn as_url_array(v: Option<&Value>) -> Vec<String> {
    match v {
        None => vec![],
        Some(x) if !truthy(x) => vec![],
        Some(Value::Array(a)) => a.iter().filter_map(reference_to_url).collect(),
        Some(x) => reference_to_url(x).into_iter().collect(),
    }
}

/// Canonicalise image param aliases so references, negatives and tuning
/// cannot silently disappear before spend.
pub fn normalize_image_params(ctx: &Value) -> Value {
    let mut refs: Vec<String> = Vec::new();
    for key in [
        "references",
        "image_urls",
        "imageUrls",
        "styleRefs",
        "styleRef",
    ] {
        for url in as_url_array(obj_get(ctx, key)) {
            if !refs.contains(&url) {
                refs.push(url);
            }
        }
    }
    let mut out = ctx.as_object().cloned().unwrap_or_default();
    let refs_v = json!(refs);
    for key in ["references", "styleRefs", "image_urls", "imageUrls"] {
        out.insert(key.into(), refs_v.clone());
    }
    let pair = |out: &mut Map<String, Value>, a: &str, b: &str| {
        let v = first_present(&[obj_get(ctx, a), obj_get(ctx, b)]).cloned();
        let w = first_present(&[obj_get(ctx, b), obj_get(ctx, a)]).cloned();
        if let Some(v) = v {
            out.insert(a.into(), v);
        }
        if let Some(w) = w {
            out.insert(b.into(), w);
        }
    };
    pair(&mut out, "negativePrompt", "negative_prompt");
    pair(&mut out, "numInferenceSteps", "num_inference_steps");
    pair(&mut out, "guidanceScale", "guidance_scale");
    pair(&mut out, "size", "image_size");
    pair(&mut out, "googleSearch", "google_search");
    pair(&mut out, "outputFormat", "output_format");
    let cb = first_present(&[
        obj_get(ctx, "callbackUrl"),
        obj_get(ctx, "callBackUrl"),
        obj_get(ctx, "callback_url"),
    ])
    .cloned();
    if let Some(cb) = cb {
        for key in ["callbackUrl", "callBackUrl", "callback_url"] {
            out.insert(key.into(), cb.clone());
        }
    }
    Value::Object(out)
}

// ── LoRA base-model registry (image-model-base-registry.mjs) ──────────────

/// The LoRA base model a contract slug is locked to; only `flux-2-dev`.
pub fn base_model_for_image_model(slug: &str) -> Option<&'static str> {
    match slug.trim() {
        "flux-2-dev" => Some("FLUX.2-dev"),
        _ => None,
    }
}

pub fn assert_image_model_supports_loras(slug: &str) -> GenResult<&'static str> {
    base_model_for_image_model(slug).ok_or_else(|| {
        GenError::new(
            "lora_unsupported",
            format!("Image model \"{slug}\" does not support LoRA weights in the locked router"),
        )
    })
}

// ── Form schema (a7271cf server/studio-image-form-schema.mjs) ─────────────

fn deep_merge(base: &Value, over: &Value) -> Value {
    match (base, over) {
        (Value::Object(b), Value::Object(o)) => {
            let mut out = b.clone();
            for (k, v) in o {
                let merged = match b.get(k) {
                    Some(bv) => deep_merge(bv, v),
                    None => v.clone(),
                };
                out.insert(k.clone(), merged);
            }
            Value::Object(out)
        }
        _ => over.clone(),
    }
}

fn resolve_contract(slug: &str, seen: &mut Vec<String>) -> GenResult<(Value, Vec<String>)> {
    if seen.iter().any(|s| s == slug) {
        return Err(GenError::new(
            "contract_cycle",
            format!("Image contract inheritance cycle at {slug}"),
        ));
    }
    let contract = contracts_map().get(slug).ok_or_else(|| {
        GenError::new(
            "contract_not_found",
            format!("Image model contract not found: {slug}"),
        )
    })?;
    let parent = obj_get(contract, "_meta")
        .and_then(|m| obj_get(m, "_extends_parent"))
        .or_else(|| obj_get(contract, "_extends_parent"))
        .and_then(Value::as_str)
        .map(str::to_string);
    match parent {
        None => Ok((contract.clone(), vec![slug.to_string()])),
        Some(p) => {
            seen.push(slug.to_string());
            let (parent_contract, mut chain) = resolve_contract(&p, seen)?;
            chain.push(slug.to_string());
            Ok((deep_merge(&parent_contract, contract), chain))
        }
    }
}

fn is_rgb_list_format(f: Option<&str>) -> bool {
    f.is_some_and(|f| {
        let l = f.to_lowercase().replace(' ', "");
        l.contains("[{rgb:[r,g,b]}")
    })
}

fn is_rgb_object_format(f: Option<&str>) -> bool {
    f.is_some_and(|f| f.to_lowercase().replace(' ', "").contains("{rgb:[r,g,b]}"))
}

fn derive_form_field(field: &str, spec: &Value, block_required: bool) -> GenResult<Value> {
    let fail = |why: &str| GenError::new("form_schema", format!("{field}: {why}"));
    let Some(obj) = spec.as_object() else {
        return Err(fail("field metadata must be an object"));
    };
    let Some(ty) = obj
        .get("type")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
    else {
        return Err(fail("missing type"));
    };
    if ![
        "string", "integer", "number", "boolean", "enum", "array", "object",
    ]
    .contains(&ty)
    {
        return Err(fail(&format!("unknown type {ty}")));
    }
    let format = obj.get("_format").and_then(Value::as_str);
    let supported = obj.get("supported") == Some(&Value::Bool(true));
    if ty == "enum"
        && !obj
            .get("enum")
            .and_then(Value::as_array)
            .is_some_and(|a| !a.is_empty())
    {
        return Err(fail("enum field has no enum values"));
    }
    if ty == "array"
        && !is_rgb_list_format(format)
        && is_nullish(obj.get("max_items"))
        && !supported
        && !obj.contains_key("default")
    {
        return Err(fail(
            "array field lacks max_items, supported marker, RGB format, or default",
        ));
    }
    if ty == "object" && !is_rgb_object_format(format) && !supported && !obj.contains_key("default")
    {
        return Err(fail(
            "object field lacks supported marker, RGB format, or default",
        ));
    }
    let input_type = match ty {
        "enum" => "dropdown",
        "integer" | "number" => {
            if !is_nullish(obj.get("min")) || !is_nullish(obj.get("max")) {
                "bounded_number"
            } else {
                "number"
            }
        }
        "boolean" => "toggle",
        "string" => {
            if !is_nullish(obj.get("max_length")) {
                "limited_text"
            } else {
                "text"
            }
        }
        "array" => {
            if is_rgb_list_format(format) {
                "color_list"
            } else {
                "string_list"
            }
        }
        _ => {
            if is_rgb_object_format(format) {
                "color"
            } else {
                "json_object"
            }
        }
    };
    let mut constraints = Map::new();
    for key in [
        "enum",
        "min",
        "max",
        "min_length",
        "max_length",
        "max_items",
        "supported",
    ] {
        if let Some(v) = obj.get(key) {
            constraints.insert(key.into(), v.clone());
        }
    }
    if let Some(ex) = obj.get("_examples").filter(|v| v.is_array()) {
        constraints.insert("examples".into(), ex.clone());
    }
    let required = match obj.get("required") {
        Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        _ => block_required,
    };
    Ok(json!({
        "field": field,
        "type": ty,
        "input_type": input_type,
        "constraints": constraints,
        "default": obj.get("default").cloned().unwrap_or(Value::Null),
        "required": required,
        "format": format,
        "note": obj.get("_note").and_then(Value::as_str),
    }))
}

/// Form fields for a model; `negative_prompt` is exposed only for models whose
/// policy has an explicit negative field.
pub fn model_form_schema(model: &str) -> GenResult<Value> {
    let slug = model.trim();
    if slug.is_empty() {
        return Err(GenError::new(
            "invalid_input",
            "Image model slug is required",
        ));
    }
    let (contract, chain) = resolve_contract(slug, &mut Vec::new())?;
    let policy = obj_get(&contract, "negative_prompt_policy").cloned();
    let strategy = policy
        .as_ref()
        .map(|p| str_of(p, "strategy").to_string())
        .unwrap_or_default();
    let explicit_negative =
        strategy == "imagen_field" || strategy == "natural_language_negative_prompt";
    let mut fields: Vec<Value> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (block, required) in [("required_fields", true), ("optional_fields", false)] {
        let Some(map) = obj_get(&contract, block).and_then(Value::as_object) else {
            continue;
        };
        for (field, spec) in map {
            if seen.contains(field) {
                continue;
            }
            if (field == "negative_prompt" || field == "negativePrompt") && !explicit_negative {
                continue;
            }
            seen.push(field.clone());
            fields.push(derive_form_field(field, spec, required)?);
        }
    }
    Ok(json!({
        "schema_version": "1.0.0",
        "model": slug,
        "contract_version": obj_get(&contract, "_meta").and_then(|m| obj_get(m, "version")),
        "inheritance_chain": chain,
        "negative_prompt_policy": policy,
        "fields": fields,
    }))
}

/// Generation param names a model accepts (everything except `prompt`).
pub fn model_param_fields(model: &str) -> Vec<String> {
    model_form_schema(model)
        .ok()
        .and_then(|s| s.get("fields").and_then(Value::as_array).cloned())
        .map(|fields| {
            fields
                .iter()
                .filter_map(|f| f.get("field").and_then(Value::as_str))
                .filter(|f| *f != "prompt")
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

// ── Commands ──────────────────────────────────────────────────────────────

fn need_model(input: &Value) -> GenResult<String> {
    input
        .get("model")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| GenError::new("invalid_input", "model is required"))
}

fn ctx_of(input: &Value) -> Value {
    input
        .get("ctx")
        .filter(|c| c.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}))
}

pub fn handle(_ctx: &Context, name: &str, input: &Value) -> GenResult<Value> {
    match name {
        "studio.contracts.list" => {
            let rows: Vec<Value> = list_contracts()
                .into_iter()
                .map(|slug| {
                    let c = contracts_map().get(&slug).cloned().unwrap_or(Value::Null);
                    let meta = obj_get(&c, "_meta").cloned().unwrap_or(Value::Null);
                    let tier_id = meta.get("pricing_tier_id").and_then(Value::as_str).map(str::to_string);
                    let tier = tier_id.as_deref().and_then(get_invoice_status).cloned();
                    json!({
                        "model": slug,
                        "version": meta.get("version"),
                        "pricing_tier_id": tier_id,
                        "cost_per_image": tier.as_ref().and_then(|t| t.get("cost_per_image")),
                        "invoice_verified": tier.as_ref().and_then(|t| t.get("invoice_verified")),
                        "renderer": obj_get(&c, "official_prompt_template").map(|t| str_of(t, "_renderer_id")),
                        "slots": obj_get(&c, "official_prompt_template").and_then(|t| t.get("slots")),
                        "negative_strategy": obj_get(&c, "negative_prompt_policy").map(|p| str_of(p, "strategy")),
                        "max_references": obj_get(&c, "reference_syntax").and_then(|r| r.get("max_references")),
                    })
                })
                .collect();
            Ok(json!({"contracts": rows}))
        }
        "studio.contracts.schema" => model_form_schema(&need_model(input)?),
        "studio.contracts.validate" => {
            let model = need_model(input)?;
            let payload = input.get("payload").cloned().unwrap_or_else(|| json!({}));
            match validate_contract(&model, &payload, &ctx_of(input)) {
                Ok(()) => Ok(json!({"ok": true, "model": model, "violations": []})),
                Err(e) if e.rule_id() == "contract_missing" => {
                    Err(GenError::new("contract_not_found", e.message()))
                }
                Err(e) => Ok(e.to_json()),
            }
        }
        "studio.contracts.render" => {
            let model = need_model(input)?;
            let internal = input.get("internal").cloned().unwrap_or_else(|| json!({}));
            match render_contract(&model, &internal, &ctx_of(input)) {
                Ok(r) => {
                    let mut v = r.to_json();
                    v["ok"] = json!(true);
                    v["model"] = json!(model);
                    Ok(v)
                }
                Err(e) if e.rule_id() == "contract_missing" => {
                    Err(GenError::new("contract_not_found", e.message()))
                }
                Err(e) => Ok(e.to_json()),
            }
        }
        "studio.contracts.invoice" => {
            let tier = input
                .get("tier")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(get_invoice_status(tier).cloned().unwrap_or(Value::Null))
        }
        "studio.contracts.normalize" => Ok(normalize_image_params(
            input.get("params").unwrap_or(&json!({})),
        )),
        "studio.contracts.health" => Ok(contract_health()),
        _ => Err(GenError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}
