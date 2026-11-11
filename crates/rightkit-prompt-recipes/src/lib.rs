//! Prompt contracts and their validator, the recipe composer, the image workflow catalogue, the
//! character and LoRA lock data, and the video shot-contract templates: a library over JSON data
//! embedded at compile time (`data/<area>/`). Brands are the host's ([`brands::set_brands`]); the
//! crate ships none.
//!
//! The default build is the pure library: no engine, no database, no network. Feature
//! `commands` adds the `studio.<area>.*` command handlers ([`commands`], [`dispatch`]), a thin
//! adapter that runs them against a `rightkit_media_gen::Context` (drafts under its data folder,
//! dry runs and model lists through its API) and the approval gate of
//! `rightkit-media-gen-approvals`. Swap templates and the recipe catalogue join the engine's model
//! registry, so they exist only with that feature.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub mod brands;
pub mod catalog;
pub mod characters;
pub mod contracts;
#[cfg(feature = "commands")]
pub mod recipe_catalogue;
pub mod recipes;
#[cfg(feature = "commands")]
pub mod swap_templates;
pub mod video_templates;

#[cfg(feature = "commands")]
pub use rightkit_media_gen::Context;

/// Typed error of every fallible call; serialises as `{code, message}` plus `details` when the
/// error carries structured data. With feature `commands` it converts to and from
/// `rightkit_media_gen::GenError` (same shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RecipeError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl RecipeError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl std::fmt::Display for RecipeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RecipeError {}

pub type RecipeResult<T> = Result<T, RecipeError>;

#[cfg(feature = "commands")]
impl From<rightkit_media_gen::GenError> for RecipeError {
    fn from(e: rightkit_media_gen::GenError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            details: e.details,
        }
    }
}

#[cfg(feature = "commands")]
impl From<RecipeError> for rightkit_media_gen::GenError {
    fn from(e: RecipeError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            details: e.details,
        }
    }
}

/// Every command name this crate owns, for an engine CLI and doctor capabilities.
#[cfg(feature = "commands")]
pub fn commands() -> Vec<&'static str> {
    let mut all = Vec::new();
    all.extend_from_slice(catalog::COMMANDS);
    all.extend_from_slice(contracts::COMMANDS);
    all.extend_from_slice(recipes::COMMANDS);
    all.extend_from_slice(video_templates::COMMANDS);
    all.extend_from_slice(characters::COMMANDS);
    all.extend_from_slice(recipe_catalogue::COMMANDS);
    all.extend_from_slice(swap_templates::COMMANDS);
    all
}

/// Route a `studio.*` command to the module that owns it; `None` when no module here does.
#[cfg(feature = "commands")]
pub fn dispatch(
    ctx: &Context,
    name: &str,
    input: &Value,
) -> Option<rightkit_media_gen::GenResult<Value>> {
    let handled = if catalog::COMMANDS.contains(&name) {
        catalog::handle(ctx, name, input)
    } else if contracts::COMMANDS.contains(&name) {
        contracts::handle(ctx, name, input)
    } else if recipes::COMMANDS.contains(&name) {
        recipes::handle(ctx, name, input)
    } else if video_templates::COMMANDS.contains(&name) {
        video_templates::handle(ctx, name, input)
    } else if characters::COMMANDS.contains(&name) {
        characters::handle(ctx, name, input)
    } else if recipe_catalogue::COMMANDS.contains(&name) {
        recipe_catalogue::handle(ctx, name, input)
    } else if swap_templates::COMMANDS.contains(&name) {
        swap_templates::handle(ctx, name, input)
    } else {
        return None;
    };
    Some(handled.map_err(Into::into))
}
