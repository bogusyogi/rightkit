//! A typed, read-only view of a channel recipe body, used to validate it.
//!
//! The view exists only to be checked. It is never serialised back: a recipe's stored
//! `body` stays the untyped `Map<String, Value>` and `body_blake3` is the canonical hash of
//! that map, so typing the body cannot reorder, rename, drop or default a key and cannot
//! change the identity of a recipe already saved. Unknown keys are kept in `extra` (and
//! never refused) so a newer build's keys survive an older one.
//!
//! Allowed values come from the code that consumes them at render time: cut presets (`tight`,
//! `natural`, `punchy`), formats (`16:9`, `9:16`, `1:1`), colour ids from `rightkit_color::preset`,
//! audio ids from `rightkit_audio::Preset::builtin`, caption styles from the
//! `rightkit_captions::StyleCatalog` the host passes (its shipped styles plus any library styles).

use std::collections::BTreeMap;

use rightkit_captions::StyleCatalog;
use serde::Deserialize;
use serde_json::{Map, Value};

const CUT_PRESETS: &[&str] = &["tight", "natural", "punchy"];
const FORMATS: &[&str] = &["16:9", "9:16", "1:1"];

/// Every body key a build reads, each optional and typed; anything else lands in `extra`.
// Fields exist so a wrong type is refused; validation does not read every one of them.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct ChannelRecipe {
    style: Option<String>,
    formats: Option<Vec<String>>,
    captions: Option<Captions>,
    finish: Option<Finish>,
    music: Option<String>,
    music_level: Option<f64>,
    caption_style: Option<String>,
    color_preset: Option<String>,
    audio_preset: Option<String>,
    lut_path: Option<String>,
    lut_intensity: Option<f64>,
    match_shots: Option<bool>,
    burn_captions: Option<bool>,
    grade: Option<Value>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

/// `captions`: a plain on/off flag, or an object with `on` / `enabled` and a `style`.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Captions {
    Flag(bool),
    Spec(CaptionsSpec),
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct CaptionsSpec {
    on: Option<bool>,
    enabled: Option<bool>,
    style: Option<String>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct Finish {
    audio: Option<String>,
    color: Option<String>,
    lut_path: Option<String>,
    lut_intensity: Option<f64>,
    match_shots: Option<bool>,
    music: Option<String>,
    music_level: Option<f64>,
    #[serde(flatten)]
    extra: BTreeMap<String, Value>,
}

const KNOWN_KEYS: &[&str] = &[
    "style",
    "formats",
    "captions",
    "finish",
    "music",
    "music_level",
    "caption_style",
    "color_preset",
    "audio_preset",
    "lut_path",
    "lut_intensity",
    "match_shots",
    "burn_captions",
    "grade",
];

/// The typed view of `body`, or an error naming the first known key with the wrong type.
fn parse(body: &Map<String, Value>) -> Result<ChannelRecipe, String> {
    let e = match serde_json::from_value::<ChannelRecipe>(Value::Object(body.clone())) {
        Ok(recipe) => return Ok(recipe),
        Err(e) => e,
    };
    for key in KNOWN_KEYS {
        let Some(value) = body.get(*key) else {
            continue;
        };
        let mut one = Map::new();
        one.insert((*key).to_string(), value.clone());
        if let Err(e) = serde_json::from_value::<ChannelRecipe>(Value::Object(one)) {
            return Err(format!("body.{key} has the wrong type ({e}): {value}"));
        }
    }
    Err(format!("body has the wrong shape: {e}"))
}

fn list(items: impl IntoIterator<Item = impl AsRef<str>>) -> String {
    items
        .into_iter()
        .map(|i| i.as_ref().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn caption_style(catalog: &StyleCatalog, key: &str, id: &str) -> Result<(), String> {
    if catalog.get(id).is_some() {
        return Ok(());
    }
    Err(format!(
        "{key} {id:?} is not a caption style: expected {}",
        list(catalog.styles.iter().map(|s| s.id.as_str()))
    ))
}

fn colour(key: &str, id: &str) -> Result<(), String> {
    if id.is_empty() || id == "custom" || rightkit_color::preset(id).is_some() {
        return Ok(());
    }
    let ids = rightkit_color::presets().into_iter().map(|p| p.id);
    Err(format!(
        "{key} {id:?} is not a colour preset: expected an empty string (none), custom or {}",
        list(ids)
    ))
}

fn audio(key: &str, id: &str) -> Result<(), String> {
    if id.is_empty() || rightkit_audio::Preset::builtin(id).is_ok() {
        return Ok(());
    }
    Err(format!(
        "{key} {id:?} is not an audio preset: expected an empty string (none) or {}",
        list(rightkit_audio::Preset::builtin_names())
    ))
}

fn intensity(key: &str, value: f64) -> Result<(), String> {
    if (0.0..=100.0).contains(&value) {
        return Ok(());
    }
    Err(format!(
        "{key} {value} is out of range: expected a number from 0 to 100"
    ))
}

fn music_level(key: &str, value: f64) -> Result<(), String> {
    if value <= 0.0 {
        return Ok(());
    }
    Err(format!(
        "{key} {value} is above 0: expected a level in dB under the mix, 0 or less"
    ))
}

/// Checks a body against what a render will accept, with `styles` as the caption styles in force.
/// Unknown keys are not an error.
pub fn validate(body: &Map<String, Value>, styles: &StyleCatalog) -> Result<(), String> {
    let recipe = parse(body)?;
    let finish = recipe.finish.as_ref();

    if let Some(style) = &recipe.style {
        if !CUT_PRESETS.contains(&style.as_str()) {
            return Err(format!(
                "body.style {style:?} is not an edit preset: expected {}",
                list(CUT_PRESETS)
            ));
        }
    }
    for format in recipe.formats.iter().flatten() {
        if !FORMATS.contains(&format.as_str()) {
            return Err(format!(
                "body.formats entry {format:?} is not a supported format: expected {}",
                list(FORMATS)
            ));
        }
    }
    if let Some(Captions::Spec(spec)) = &recipe.captions {
        if let Some(style) = &spec.style {
            caption_style(styles, "body.captions.style", style)?;
        }
    }
    if let Some(id) = &recipe.caption_style {
        caption_style(styles, "body.caption_style", id)?;
    }

    if let Some(id) = &recipe.color_preset {
        colour("body.color_preset", id)?;
    }
    if let Some(id) = finish.and_then(|f| f.color.as_ref()) {
        colour("body.finish.color", id)?;
    }
    // Rendering reads the flat key first and falls back to `finish`, so that order decides
    // which colour id and which LUT path are in force.
    let effective_colour = recipe
        .color_preset
        .as_deref()
        .or_else(|| finish.and_then(|f| f.color.as_deref()));
    let effective_lut = recipe
        .lut_path
        .as_deref()
        .or_else(|| finish.and_then(|f| f.lut_path.as_deref()))
        .filter(|p| !p.is_empty());
    if effective_colour == Some("custom") && effective_lut.is_none() {
        return Err(
            "colour \"custom\" needs a .cube file: set body.lut_path or body.finish.lut_path"
                .to_string(),
        );
    }

    if let Some(id) = &recipe.audio_preset {
        audio("body.audio_preset", id)?;
    }
    if let Some(id) = finish.and_then(|f| f.audio.as_ref()) {
        audio("body.finish.audio", id)?;
    }

    if let Some(v) = recipe.lut_intensity {
        intensity("body.lut_intensity", v)?;
    }
    if let Some(v) = finish.and_then(|f| f.lut_intensity) {
        intensity("body.finish.lut_intensity", v)?;
    }
    if let Some(v) = recipe.music_level {
        music_level("body.music_level", v)?;
    }
    if let Some(v) = finish.and_then(|f| f.music_level) {
        music_level("body.finish.music_level", v)?;
    }
    Ok(())
}
