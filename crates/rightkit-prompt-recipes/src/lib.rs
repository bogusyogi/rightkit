//! Prompt contracts and validator, recipe composer and catalogue, character and LoRA lock,
//! video quality templates, template swap, brand codes and the KDP workflow.
//!
//! Each module owns its `studio.<area>.*` commands and its JSON data under `data/<area>/`
//! (embedded at compile time). The commands run against a `rightkit_media_gen::Context` and the
//! approval gate of `rightkit-media-gen-approvals`.

use serde_json::Value;

pub use rightkit_media_gen::{Context, GenError, GenResult};

pub mod brands;
pub mod catalog;
pub mod characters;
pub mod contracts;
pub mod kdp;
pub mod recipe_catalogue;
pub mod recipes;
pub mod swap_templates;
pub mod video_templates;

/// Every command name this crate owns, for an engine CLI and doctor capabilities.
pub fn commands() -> Vec<&'static str> {
    let mut all = Vec::new();
    all.extend_from_slice(kdp::COMMANDS);
    all.extend_from_slice(catalog::COMMANDS);
    all.extend_from_slice(contracts::COMMANDS);
    all.extend_from_slice(recipes::COMMANDS);
    all.extend_from_slice(video_templates::COMMANDS);
    all.extend_from_slice(characters::COMMANDS);
    all.extend_from_slice(brands::COMMANDS);
    all.extend_from_slice(recipe_catalogue::COMMANDS);
    all.extend_from_slice(swap_templates::COMMANDS);
    all
}

/// Route a `studio.*` command to the module that owns it; `None` when no module here does.
pub fn dispatch(ctx: &Context, name: &str, input: &Value) -> Option<GenResult<Value>> {
    if kdp::COMMANDS.contains(&name) {
        return Some(kdp::handle(ctx, name, input));
    }
    if catalog::COMMANDS.contains(&name) {
        return Some(catalog::handle(ctx, name, input));
    }
    if contracts::COMMANDS.contains(&name) {
        return Some(contracts::handle(ctx, name, input));
    }
    if recipes::COMMANDS.contains(&name) {
        return Some(recipes::handle(ctx, name, input));
    }
    if video_templates::COMMANDS.contains(&name) {
        return Some(video_templates::handle(ctx, name, input));
    }
    if characters::COMMANDS.contains(&name) {
        return Some(characters::handle(ctx, name, input));
    }
    if brands::COMMANDS.contains(&name) {
        return Some(brands::handle(ctx, name, input));
    }
    if recipe_catalogue::COMMANDS.contains(&name) {
        return Some(recipe_catalogue::handle(ctx, name, input));
    }
    if swap_templates::COMMANDS.contains(&name) {
        return Some(swap_templates::handle(ctx, name, input));
    }
    None
}
