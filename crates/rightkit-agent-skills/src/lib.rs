//! host_skills — skill manifests and the trigger DSL.
//!
//! Skills carry TOML frontmatter with a `trigger` expression written in a
//! small boolean DSL. This crate provides a hand-written lexer, recursive
//! descent parser, and evaluator for that DSL, plus manifest loading.

pub mod commands;
pub mod custom_commands;
pub mod inline_shell;
pub mod manifest;
pub mod registry_catalog;
pub mod registry_install;
pub mod routing;
pub mod trigger;

pub use commands::{CommandAction, CommandInfo, CommandRegistry, CommandResult, SlashCommand};
pub use manifest::{
    normalize_skill_pack_github_url, HostSkillMetadata, SkillCategory, SkillEffects, SkillError,
    SkillGateMode, SkillLifecycle, SkillManifest, SkillPackageMetadata,
};
pub use routing::PathAffinityRouter;
pub use trigger::{
    evaluate_trigger, evaluate_with_regex, CmpOp, CompiledTrigger, Expr, TriggerBinder,
    TriggerContext, TriggerError,
};
