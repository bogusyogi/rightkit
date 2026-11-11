//! studio.catalog — the image workflow catalog: groups with their models,
//! best-for / avoid-for notes and prompt rules, plus the intent routes.
//! Ported from the Media Console (a7271cf `server/workflow-catalog-data.mjs`
//! IMAGE_GROUPS and `server/studio-image-intents.mjs`). The JSON under
//! `data/catalog/` is the original data; this module adds the join to the
//! prompt contracts so the UI can show each model's contract state.
//!
//! Commands: `studio.catalog.{image,group,intents,contract_for}`.

use std::sync::OnceLock;

use serde_json::{json, Value};

use crate::contracts;
#[cfg(feature = "commands")]
use crate::{RecipeError, RecipeResult};
#[cfg(feature = "commands")]
use rightkit_media_gen::Context;

#[cfg(feature = "commands")]
pub const COMMANDS: &[&str] = &[
    "studio.catalog.image",
    "studio.catalog.group",
    "studio.catalog.intents",
    "studio.catalog.contract_for",
];

const GROUPS_SOURCE: &str = include_str!("../data/catalog/image-groups.json");
const INTENTS_SOURCE: &str = include_str!("../data/catalog/image-intents.json");

static GROUPS: OnceLock<Value> = OnceLock::new();
static INTENTS: OnceLock<Value> = OnceLock::new();

fn groups_doc() -> &'static Value {
    GROUPS.get_or_init(|| serde_json::from_str(GROUPS_SOURCE).expect("embedded image-groups.json"))
}

fn intents_doc() -> &'static Value {
    INTENTS
        .get_or_init(|| serde_json::from_str(INTENTS_SOURCE).expect("embedded image-intents.json"))
}

/// Max references an Image Studio request may carry.
pub fn reference_limit() -> usize {
    intents_doc()
        .get("reference_limit")
        .and_then(Value::as_u64)
        .unwrap_or(14) as usize
}

/// Brand codes a request may carry (anything else becomes `NA`): the host's brands
/// ([`crate::brands::set_brands`]) and `NA`.
pub fn brands() -> Vec<String> {
    crate::brands::codes()
        .iter()
        .cloned()
        .chain(std::iter::once(crate::brands::NO_BRAND.to_string()))
        .collect()
}

/// Intent route (`bottom|mid|premium|cartoon|vector|...`) or `None`.
pub fn intent(name: &str) -> Option<&'static Value> {
    intents_doc().get("intents").and_then(|i| i.get(name))
}

pub fn intent_names() -> Vec<String> {
    intents_doc()
        .get("intents")
        .and_then(Value::as_object)
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

/// Contract slug an intent routes to.
pub fn intent_model(name: &str) -> Option<&'static str> {
    intent(name)
        .and_then(|i| i.get("model"))
        .and_then(Value::as_str)
}

/// The engine model a catalog prompt-contract slug resolves to. Catalog slugs
/// name prompt rules (the contracts); the engine serves model ids. The FLUX.2
/// Pro/Dev contracts resolve to the served FLUX.2 Max / Klein 9B routes. The
/// Recraft V4 raster contract resolves to `recraft-v4`, the WaveSpeed
/// text-to-image route, which returns raster output. The two Recraft vector
/// contracts resolve to nothing: no served route produces SVG.
/// `None` means the slug has no served engine model and the catalog reports it.
pub fn engine_model_for_contract(slug: &str) -> Option<&'static str> {
    Some(match slug {
        "flux-2-pro" => "fal-ai/flux-2-max",
        "flux-2-dev" => "fal-ai/flux-2/klein/9b",
        "nano-banana-pro" => "nano-banana-2",
        "recraft-v4-raster" => "recraft-v4",
        "recraft-v4-vector" | "recraft-v4-pro-vector" => return None,
        "qwen-image" => "qwen-image",
        "seedream-v4" => "seedream-v4",
        "gpt-image-2" => "gpt-image-2",
        "imagen-4-ultra" => "imagen-4-ultra",
        _ => return None,
    })
}

#[cfg(feature = "commands")]
/// The engine route that runs a contract's LoRAs, when the engine serves one. `flux-2-dev` resolves
/// to `fal-ai/flux-2/klein/9b` for plain text-to-image (its contract fields fit that route), but Klein
/// is a different base model (`FLUX.2-Klein-9B`) and takes no `loras`; FLUX.2-dev LoRAs run on
/// `fal-ai/flux-2/lora` (and `/lora/edit` with references) once the engine lists it. `None` means no
/// served LoRA route for this slug.
pub fn engine_lora_model_for_contract(slug: &str) -> Option<&'static str> {
    match slug {
        "flux-2-dev"
            if crate::recipe_catalogue::registry_served("fal", "fal-ai/flux-2/lora", "image") =>
        {
            Some("fal-ai/flux-2/lora")
        }
        _ => None,
    }
}

/// Why a contract slug has no served engine model; `None` when it resolves.
/// The catalog reports this next to `engine_model` so the panel can say why.
pub fn engine_unavailable_reason(slug: &str) -> Option<&'static str> {
    if engine_model_for_contract(slug).is_some() {
        return None;
    }
    Some(match slug {
        "recraft-v4-vector" | "recraft-v4-pro-vector" => "no served route produces SVG",
        _ => "no served engine model for this prompt contract",
    })
}

/// The prompt contract that fits an engine model id (`fal-ai/flux-2-pro`,
/// `nano-banana-pro`, ...). The engine catalog and the contracts name models
/// differently, so this is a pattern join; `None` means the model has no
/// contract and the panel leaves it ungated by contract rules.
pub fn contract_for_engine_model(model: &str) -> Option<&'static str> {
    let m = model.to_ascii_lowercase();
    let has = |s: &str| m.contains(s);
    let slug = if has("nano-banana") || has("nanobanana") {
        "nano-banana-pro"
    } else if has("flux-2") && has("klein") {
        return None;
    } else if has("flux-2") && (has("pro") || has("max")) {
        "flux-2-pro"
    } else if has("flux-2") && has("dev") {
        "flux-2-dev"
    } else if has("qwen-image") {
        "qwen-image"
    } else if has("gpt-image") {
        "gpt-image-2"
    } else if has("recraft") && has("pro") && has("vector") {
        "recraft-v4-pro-vector"
    } else if has("recraft") && has("vector") {
        "recraft-v4-vector"
    } else if has("recraft") {
        "recraft-v4-raster"
    } else if has("imagen") && has("ultra") {
        "imagen-4-ultra"
    } else if has("imagen") {
        "imagen-4-standard"
    } else if has("seedream") {
        "seedream-v4"
    } else if has("midjourney") || has("mj-v7") || m == "mj" {
        "mj-v7-evolink"
    } else {
        return None;
    };
    contracts::has_contract(slug).then_some(slug)
}

/// The served LoRA route, which only the engine's registry knows (feature `commands`).
#[cfg(feature = "commands")]
fn engine_lora_model(slug: &str) -> Option<&'static str> {
    engine_lora_model_for_contract(slug)
}

#[cfg(not(feature = "commands"))]
fn engine_lora_model(_slug: &str) -> Option<&'static str> {
    None
}

/// A model slug joined with its contract: version, negative-prompt strategy,
/// reference cap, invoice-backed cost, and the intents that route to it.
fn model_ref(slug: &str) -> Value {
    let contract = contracts::load_contract(slug).ok();
    let meta = contract.and_then(|c| c.get("_meta"));
    let tier = meta
        .and_then(|m| m.get("pricing_tier_id"))
        .and_then(Value::as_str)
        .and_then(contracts::get_invoice_status);
    let routed: Vec<String> = intent_names()
        .into_iter()
        .filter(|name| intent_model(name) == Some(slug))
        .collect();
    json!({
        "slug": slug,
        "engine_model": engine_model_for_contract(slug),
        "engine_model_reason": engine_unavailable_reason(slug),
        "engine_lora_model": engine_lora_model(slug),
        "has_contract": contract.is_some(),
        "contract_version": meta.and_then(|m| m.get("version")),
        "negative_strategy": contract.and_then(|c| c.get("negative_prompt_policy")).and_then(|p| p.get("strategy")),
        "max_references": contract.and_then(|c| c.get("reference_syntax")).and_then(|r| r.get("max_references")),
        "cost_per_image": tier.and_then(|t| t.get("cost_per_image")),
        "invoice_verified": tier.and_then(|t| t.get("invoice_verified")),
        "lora_base_model": contracts::base_model_for_image_model(slug),
        "intents": routed,
        "est_cost": routed_cost(slug),
    })
}

fn routed_cost(slug: &str) -> Value {
    intent_names()
        .into_iter()
        .find(|name| intent_model(name) == Some(slug))
        .and_then(|name| intent(&name).and_then(|i| i.get("estimated_cost")).cloned())
        .unwrap_or(Value::Null)
}

fn annotated_group(group: &Value) -> Value {
    let mut out = group.clone();
    let mut slugs: Vec<String> = group
        .get("model_slugs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    // A workflow's route options (Recraft's Imagen fallback) are models too.
    for wf in group
        .get("workflows")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for opt in wf
            .get("route_options")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(s) = opt.get("model_slug").and_then(Value::as_str) {
                if !slugs.iter().any(|x| x == s) {
                    slugs.push(s.to_string());
                }
            }
        }
    }
    out["models"] = Value::Array(slugs.iter().map(|s| model_ref(s)).collect());
    out
}

/// The intent routes, each with the engine model its contract slug resolves to.
fn intents_with_engine() -> Value {
    let mut intents = intents_doc()
        .get("intents")
        .cloned()
        .unwrap_or_else(|| json!({}));
    if let Some(map) = intents.as_object_mut() {
        for route in map.values_mut() {
            let slug = route.get("model").and_then(Value::as_str);
            let engine = slug.and_then(engine_model_for_contract);
            let reason = slug.and_then(engine_unavailable_reason);
            route["engine_model"] = json!(engine);
            route["engine_model_reason"] = json!(reason);
        }
    }
    intents
}

pub fn image_catalog() -> Value {
    let groups: Vec<Value> = groups_doc()
        .get("groups")
        .and_then(Value::as_array)
        .map(|g| g.iter().map(annotated_group).collect())
        .unwrap_or_default();
    json!({
        "schema_version": groups_doc().get("schema_version"),
        "section": {"id": "image", "label": "Image"},
        "groups": groups,
        "intents": intents_with_engine(),
        "reference_limit": reference_limit(),
        "brands": brands(),
    })
}

#[cfg(feature = "commands")]
pub fn handle(_ctx: &Context, name: &str, input: &Value) -> RecipeResult<Value> {
    match name {
        "studio.catalog.image" => Ok(image_catalog()),
        "studio.catalog.group" => {
            let id = input.get("id").and_then(Value::as_str).unwrap_or_default();
            groups_doc()
                .get("groups")
                .and_then(Value::as_array)
                .and_then(|g| {
                    g.iter()
                        .find(|x| x.get("id").and_then(Value::as_str) == Some(id))
                })
                .map(annotated_group)
                .ok_or_else(|| {
                    RecipeError::new("group_not_found", format!("no image workflow group: {id}"))
                })
        }
        "studio.catalog.intents" => Ok(json!({
            "intents": intents_with_engine(),
            "reference_limit": reference_limit(),
            "brands": brands(),
        })),
        "studio.catalog.contract_for" => {
            let model = input
                .get("model")
                .and_then(Value::as_str)
                .unwrap_or_default();
            Ok(json!({"model": model, "contract": contract_for_engine_model(model)}))
        }
        _ => Err(RecipeError::new(
            "unknown_command",
            format!("unknown command: {name}"),
        )),
    }
}
