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

/// The finishing app a host hands a Vox run to: its display name and engine binary, used in the
/// step title, summary, hints and error text of the finishing step ([`pipeline_workflows::finish`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FinishingApp {
    /// Display name ("CutRight").
    pub app: String,
    /// Engine binary name as the host's users know it ("cutright-engine").
    pub engine: String,
}

/// Names and switches specific to this crate that an embedding app may set once at startup with
/// [`configure_pipeline`], beside `rightkit_media_gen::host::configure_media`. Every default is
/// RightKit-neutral and keeps this crate's behaviour; a host with shipped users passes the
/// literals it already uses. Build it with `..Default::default()`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineHostConfig {
    /// The tool name recorded as the provider in the facts of assets a pipeline step derives
    /// (compose, transcode, caption burn). Persisted with the asset. Default
    /// `rightkit-media-pipeline`.
    pub producer: String,
    /// The engine binary named in the `brief run` usage text. Default none: `<engine>`.
    pub engine_name: Option<String>,
    /// The finishing app named in the finishing step's wording. Default none: the neutral
    /// "timeline app" / "the finishing engine" wording.
    pub finishing_app: Option<FinishingApp>,
    /// Whether the QA marker (`<PREFIX>_QA`, then `RIGHTKIT_QA`) guards the user's real data
    /// folder and library in `brief run` ([`brief::qa_marked`]). Default `true`. A host that has
    /// no QA marker sets `false`: the variable is then never read.
    pub qa_guard: bool,
}

impl Default for PipelineHostConfig {
    fn default() -> Self {
        Self {
            producer: "rightkit-media-pipeline".into(),
            engine_name: None,
            finishing_app: None,
            qa_guard: true,
        }
    }
}

static PIPELINE_CONFIG: std::sync::OnceLock<PipelineHostConfig> = std::sync::OnceLock::new();

/// Set the crate-specific host options once, before first use. Returns the rejected value when
/// options are already set (or were already read, which fixes the defaults).
pub fn configure_pipeline(config: PipelineHostConfig) -> Result<(), Box<PipelineHostConfig>> {
    PIPELINE_CONFIG.set(config).map_err(Box::new)
}

/// The options in force (the defaults until [`configure_pipeline`] is called).
pub fn pipeline_config() -> &'static PipelineHostConfig {
    PIPELINE_CONFIG.get_or_init(PipelineHostConfig::default)
}

/// The name of the environment override `suffix` in the host's prefix, `RIGHTKIT_` as the fallback
/// (the one [`rightkit_media_gen::host::env_name`] names first).
pub(crate) fn env_os(suffix: &str) -> Option<std::ffi::OsString> {
    std::env::var_os(rightkit_media_gen::host::env_name(suffix))
        .or_else(|| std::env::var_os(format!("RIGHTKIT_{suffix}")))
}

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
