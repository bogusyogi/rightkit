//! Per-model facts on the rows `models` lists (capability `models.facts`).
//!
//! The UI used to guess these from the provider id and the model name. Each adapter now states them
//! from its own validation tables (`Provider::capabilities`), and declared catalogue entries state
//! them from their `inputs`. Everything here is additive to the existing row.
//!
//! Row fields added by [`model_rows`]:
//! - `needs_key` (bool): `Provider::needs_key`.
//! - `references`: the `ReferenceInput` object, or `null` on an image or video row whose model takes
//!   none (other kinds keep the field absent unless the model has it).
//! - `default` (bool): true on the one row per kind the engine itself uses when a request names no
//!   model. Only voice and avatar have such a rule (see [`default_models`]); every other kind has none.
//! - `inputs`: the input mode (see below) and the params that carry each input.
//! - `settings`: aspects, resolutions, seed, length, loop and sync mode, each with the param name.
//!
//! `inputs.mode` is a closed vocabulary:
//! `prompt` (text only; a video model may still take frames), `image`, `video`, `video_and_audio`,
//! `image_and_audio`, `script` (voice), `script_and_avatar_id` (HeyGen), `model_file` (Blender render)
//! and `none`. A video row also carries `start_image` and `end_image` as `required`, `optional` or
//! `none`, with `start_image_param` and `end_image_param` when not `none`. An input object is
//! `{param, accepts, required}` under `image`, `video`, `audio` (and `model` for a Blender source file), plus
//! `avatar_id` and `voice_id` where a model reads them; `accepts` is `url` (any remote URL), `https_url` (the adapter refuses
//! anything but https) or `path` (a local file path).
//!
//! `settings.aspect` / `settings.resolution` are `{param, values, ...}`. `open: true` on `aspect` means the
//! adapter forwards any value and the listed ones are the suggestions the repo already uses. A
//! `resolution` carries `required` and `default` (null when the adapter has none). `settings.length` is
//! `{param, unit: "s" | "ms", min, max, integer}` for music and sfx.

use crate::catalogue::{Entry, InputType, Role};
use crate::providers::{Kind, Provider, ReferenceInput};
use crate::{credentials, Context};
use serde_json::{json, Map, Value};

/// How far a model needs an optional frame input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Required,
    Optional,
    None,
}

impl Need {
    fn name(self) -> &'static str {
        match self {
            Self::Required => "required",
            Self::Optional => "optional",
            Self::None => "none",
        }
    }
}

/// What an adapter says about one model beyond its `ModelInfo` row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Capabilities {
    pub inputs: Map<String, Value>,
    pub settings: Map<String, Value>,
    /// A reference list the adapter reads that `Provider::references` does not declare
    /// (the fal reference-to-video models). Used only when `Provider::references` gives none.
    pub references: Option<ReferenceInput>,
}

impl Capabilities {
    pub fn mode(mut self, mode: &str) -> Self {
        self.inputs.insert("mode".into(), json!(mode));
        self
    }

    pub fn input(mut self, key: &str, value: Value) -> Self {
        self.inputs.insert(key.into(), value);
        self
    }

    pub fn setting(mut self, key: &str, value: Value) -> Self {
        self.settings.insert(key.into(), value);
        self
    }

    pub fn references(mut self, r: ReferenceInput) -> Self {
        self.references = Some(r);
        self
    }

    /// `start_image` / `end_image` of a video model, with the param that carries each.
    pub fn frames(mut self, start: Need, start_param: &str, end: Need, end_param: &str) -> Self {
        self.inputs
            .insert("start_image".into(), json!(start.name()));
        if start != Need::None {
            self.inputs
                .insert("start_image_param".into(), json!(start_param));
        }
        self.inputs.insert("end_image".into(), json!(end.name()));
        if end != Need::None {
            self.inputs
                .insert("end_image_param".into(), json!(end_param));
        }
        self
    }
}

/// `{param, values}` for an aspect-ratio control.
pub fn aspect(param: &str, values: &[&str], open: bool) -> Value {
    let mut v = json!({ "param": param, "values": values });
    if open {
        v["open"] = json!(true);
    }
    v
}

/// `{param, values, required, default}` for a resolution control.
pub fn resolution(param: &str, values: &[&str], required: bool, default: Option<&str>) -> Value {
    json!({ "param": param, "values": values, "required": required, "default": default })
}

pub fn seed() -> Value {
    json!({ "param": "seed" })
}

/// `{param, unit, min, max, integer}` for a music or sfx length. `min`/`max` are in `unit`.
pub fn length(param: &str, unit: &str, min: f64, max: f64, integer: bool) -> Value {
    json!({ "param": param, "unit": unit, "min": min, "max": max, "integer": integer })
}

/// `{param, accepts, required}` for a media input.
pub fn io(param: &str, accepts: &str, required: bool) -> Value {
    json!({ "param": param, "accepts": accepts, "required": required })
}

fn strings(values: &Option<Vec<Value>>) -> Option<Vec<&str>> {
    let all = values
        .as_ref()?
        .iter()
        .map(Value::as_str)
        .collect::<Option<Vec<_>>>()?;
    (!all.is_empty()).then_some(all)
}

/// Facts for a declared (catalogue) entry, read from its `inputs`. The entry strictly refuses any
/// param it does not declare, so these are the only names a request may use.
pub fn from_entry(e: &Entry) -> Capabilities {
    let mut c = Capabilities::default();
    let single = |role: Role| {
        e.inputs
            .iter()
            .find(|i| i.role == Some(role) && i.ty == InputType::Url)
    };
    let (video, audio, image) = (
        single(Role::Video),
        single(Role::Audio),
        single(Role::Reference),
    );
    let mode = match (video.is_some(), audio.is_some(), image.is_some()) {
        (true, true, _) => "video_and_audio",
        (true, false, _) => "video",
        (false, true, true) => "image_and_audio",
        (false, false, true) if image.is_some_and(|i| i.required) => "image",
        _ => "prompt",
    };
    c = c.mode(mode);
    for (key, found) in [("video", video), ("audio", audio), ("image", image)] {
        if let Some(i) = found {
            c = c.input(key, io(&i.param, "url", i.required));
        }
    }
    // Role -> request param for every declared url / url_list input with a role (e.g. `mask`).
    let mut by_role = Map::new();
    for i in e
        .inputs
        .iter()
        .filter(|i| i.send && matches!(i.ty, InputType::Url | InputType::UrlList))
    {
        if let Some(role) = i.role {
            by_role
                .entry(role.name().to_string())
                .or_insert_with(|| json!({ "param": i.param, "required": i.required }));
        }
    }
    for (role, v) in by_role {
        if matches!(role.as_str(), "mask" | "start" | "end" | "reference") {
            c.inputs.insert(role, v);
        }
    }
    if e.kind == Kind::Video {
        let need = |i: Option<&crate::catalogue::Input>| match i {
            Some(i) if i.required => Need::Required,
            Some(_) => Need::Optional,
            None => Need::None,
        };
        let (start, end) = (single(Role::Start), single(Role::End));
        c = c.frames(
            need(start),
            start.map_or("", |i| i.param.as_str()),
            need(end),
            end.map_or("", |i| i.param.as_str()),
        );
        if need(start) == Need::Required {
            c.inputs.insert("mode".into(), json!("image"));
        }
    }
    let by_name = |names: &[&str]| {
        e.inputs
            .iter()
            .find(|i| i.send && names.contains(&i.param.as_str()))
    };
    if let Some(i) = by_name(&["aspect_ratio", "aspect"]) {
        if let Some(values) = strings(&i.enum_values) {
            c = c.setting("aspect", aspect(&i.param, &values, false));
        }
    }
    if let Some(i) = by_name(&["resolution"]) {
        if let Some(values) = strings(&i.enum_values) {
            let default = i.default.as_ref().and_then(Value::as_str);
            c = c.setting(
                "resolution",
                resolution(&i.param, &values, i.required, default),
            );
        }
    }
    if by_name(&["seed"]).is_some() {
        c = c.setting("seed", seed());
    }
    if by_name(&["duration"]).is_some() && e.durations.is_some() {
        c = c.setting("duration", json!({ "param": "duration" }));
    }
    c
}

/// The `(kind, provider, model)` the engine itself uses when a request names no model.
///
/// - voice: `elevenlabs` / `eleven_v4_turbo`, the fallback in the host voice batch
///   (`str_of(input, "provider").unwrap_or(ELEVENLABS)`, model `eleven_v4_turbo`).
/// - avatar: the host avatar preparation picks the first avatar model, in registry order,
///   whose provider has a stored key, else the first registered one. A persisted route selection (studio
///   state) comes before both there and is not visible here.
///
/// `generate` requires a model, and no other kind has a fallback anywhere in the engine, so no other
/// kind is marked.
fn default_models(
    ctx: &Context,
    providers: &[Box<dyn Provider>],
) -> Vec<(Kind, &'static str, &'static str)> {
    let mut out = Vec::new();
    let all: Vec<(&'static str, Kind, &'static str)> = providers
        .iter()
        .flat_map(|p| p.models().iter().map(move |m| (p.id(), m.kind, m.model)))
        .collect();
    if let Some((id, _, model)) = all.iter().find(|(id, kind, model)| {
        *kind == Kind::Voice && *id == "elevenlabs" && *model == "eleven_v4_turbo"
    }) {
        out.push((Kind::Voice, *id, *model));
    }
    let avatars: Vec<_> = all
        .iter()
        .filter(|(_, kind, _)| *kind == Kind::Avatar)
        .collect();
    let keyed = avatars
        .iter()
        .find(|(id, _, _)| credentials::status_source(ctx, id).is_some());
    if let Some((id, _, model)) = keyed.or(avatars.first()) {
        out.push((Kind::Avatar, *id, *model));
    }
    out
}

/// The `models` rows: each `ModelInfo` as before, plus the facts above.
pub fn model_rows(ctx: &Context, providers: &[Box<dyn Provider>]) -> Vec<Value> {
    let defaults = default_models(ctx, providers);
    let mut rows = Vec::new();
    for p in providers {
        let needs_key = p.needs_key();
        for m in p.models() {
            let caps = p.capabilities(m.model);
            let references = p.references(m.model).or(caps.references);
            let mut row = serde_json::to_value(m).unwrap_or(Value::Null);
            if let Some(o) = row.as_object_mut() {
                match references {
                    Some(r) => {
                        o.insert("references".into(), json!(r));
                    }
                    None if matches!(m.kind, Kind::Image | Kind::Video) => {
                        o.insert("references".into(), Value::Null);
                    }
                    None => {}
                }
                o.insert("needs_key".into(), json!(needs_key));
                o.insert(
                    "default".into(),
                    json!(defaults.contains(&(m.kind, p.id(), m.model))),
                );
                o.insert("inputs".into(), Value::Object(caps.inputs));
                o.insert("settings".into(), Value::Object(caps.settings));
            }
            rows.push(row);
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inpaint() -> Entry {
        let v = json!({
            "provider": "fal", "model": "fal-ai/fill", "kind": "image", "label": "Fill",
            "endpoint": "fal-ai/fill",
            "inputs": [
                {"param": "prompt", "json_key": "prompt", "type": "string", "required": true},
                {"param": "image_url", "json_key": "image_url", "type": "url", "role": "start", "required": true},
                {"param": "mask_url", "json_key": "mask_url", "type": "url", "role": "mask", "required": true},
                {"param": "num_images", "json_key": "num_images", "type": "integer", "default": 1, "min": 1, "max": 4}
            ],
            "pricing": {"kind": "per_item", "usd": 0.05, "count_param": "num_images"},
            "priced_at": "2026-10-17",
            "price_source": "https://example.com/pricing"
        });
        serde_json::from_value(v).expect("entry")
    }

    #[test]
    fn declared_mask_and_start_params_ride_the_row() {
        let c = from_entry(&inpaint());
        assert_eq!(c.inputs["mask"]["param"], "mask_url");
        assert_eq!(c.inputs["mask"]["required"], true);
        assert_eq!(c.inputs["start"]["param"], "image_url");
        assert!(c.inputs.get("reference").is_none());
    }
}
