//! Named production pipelines on top of `rightkit-media-gen`: editable, persistable step DAGs
//! (explainer, product video, dubbing with retiming, translation with terminology, narration-clock
//! presenter, batch variations) with fan-out multi-job steps, native subtitle/compose steps and an
//! optional finishing hand-off to an external timeline engine ([`pipeline_workflows`]), and the
//! deterministic production planning stack (campaign factory, canvas, director shot plan, post
//! plan, brand-kit check, save pack, hand-offs) ([`production`]).
//!
//! Both modules are command handlers: [`handle`] dispatches `studio.pipelines.*` and
//! `studio.production.*` command names to them.

use serde_json::Value;

pub use rightkit_media_gen::{Context, GenError, GenResult};

pub mod brief;
pub mod caller_tag;
pub mod ffmpeg;
#[cfg(feature = "motion")]
pub mod motion;
pub mod pipeline_workflows;
pub mod production;

/// Every command this crate handles.
pub fn commands() -> Vec<&'static str> {
    let mut all = Vec::new();
    all.extend_from_slice(pipeline_workflows::COMMANDS);
    all.extend_from_slice(production::COMMANDS);
    all
}

/// Run `name` when this crate handles it, else `None`.
pub fn handle(ctx: &Context, name: &str, input: &Value) -> Option<GenResult<Value>> {
    if pipeline_workflows::COMMANDS.contains(&name) {
        return Some(pipeline_workflows::handle(ctx, name, input));
    }
    if production::COMMANDS.contains(&name) {
        return Some(production::handle(ctx, name, input));
    }
    None
}
