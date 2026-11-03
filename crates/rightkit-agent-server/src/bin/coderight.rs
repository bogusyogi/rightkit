//! Test-harness binary for the black-box end-to-end tests under `tests/`.
//!
//! It starts the server with empty skill services; a product ships its own binary that passes
//! real [`rightkit_agent_host_traits::SkillCatalog`] and
//! [`rightkit_agent_host_traits::skill_host::SkillHost`] implementations to
//! `rightkit_agent_server::run`. The binary name is the one the tests look up through
//! `CARGO_BIN_EXE_coderight`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rightkit_agent_host_traits::skill_host::{
    commands::CommandSpec,
    custom_commands::{CommandScope, CustomCommand},
    inline_shell::ShellRunner,
    manifest::{DiscoveredSkill, SkillError, SkillManifest, SkillSourceScope},
    trigger::{TriggerContext, TriggerError},
    SkillHost,
};
use rightkit_agent_host_traits::{
    SkillCatalog, SkillCatalogRoot, SkillManifestSummary, TriggerContext as CatalogContext,
    TriggeredSkill,
};

struct NoSkills;

impl SkillCatalog for NoSkills {
    fn parse_manifest(&self, _markdown: &str) -> Option<SkillManifestSummary> {
        None
    }

    fn triggered_skills(&self, _dirs: &[PathBuf], _ctx: &CatalogContext) -> Vec<TriggeredSkill> {
        Vec::new()
    }

    fn list_catalog(
        &self,
        _roots: &[SkillCatalogRoot],
        _query: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        Ok(serde_json::json!([]))
    }
}

impl SkillHost for NoSkills {
    fn command_specs(&self) -> &'static [CommandSpec] {
        &[]
    }

    fn manifest_from_markdown(&self, _text: &str) -> Result<SkillManifest, SkillError> {
        Err(SkillError::Install("no skills in the test harness".into()))
    }

    fn manifest_validate(&self, _manifest: &SkillManifest) -> Result<(), SkillError> {
        Ok(())
    }

    fn discover_skill_roots(&self, _roots: &[(SkillSourceScope, PathBuf)]) -> Vec<DiscoveredSkill> {
        Vec::new()
    }

    fn normalize_skill_pack_github_url(&self, raw: &str) -> Result<String, SkillError> {
        Ok(raw.trim().to_string())
    }

    fn evaluate_trigger(&self, _expr: &str, _ctx: &TriggerContext) -> Result<bool, TriggerError> {
        Ok(false)
    }

    fn custom_discover(&self, _roots: &[(PathBuf, CommandScope)]) -> Vec<CustomCommand> {
        Vec::new()
    }

    fn custom_parse_command(
        &self,
        name: &str,
        text: &str,
        scope: CommandScope,
        path: &Path,
    ) -> CustomCommand {
        CustomCommand {
            name: name.to_string(),
            description: String::new(),
            argument_hint: None,
            allowed_tools: Vec::new(),
            model: None,
            body: text.to_string(),
            scope,
            path: path.to_path_buf(),
        }
    }

    fn custom_resolve<'a>(
        &self,
        commands: &'a [CustomCommand],
        typed: &str,
    ) -> Option<&'a CustomCommand> {
        commands.iter().find(|command| command.name == typed)
    }

    fn custom_listing<'a>(
        &self,
        commands: &'a [CustomCommand],
    ) -> Vec<(String, &'a CustomCommand)> {
        commands
            .iter()
            .map(|command| (command.name.clone(), command))
            .collect()
    }

    fn custom_substitute_arguments(&self, body: &str, _args: &str) -> String {
        body.to_string()
    }

    fn custom_split_args(&self, args: &str) -> Vec<String> {
        args.split_whitespace().map(str::to_string).collect()
    }

    fn custom_expand_file_references(&self, body: &str, _workspace: &Path) -> String {
        body.to_string()
    }

    fn inline_shell_expand(&self, template: &str, _runner: &mut dyn ShellRunner) -> String {
        template.to_string()
    }
}

fn main() {
    rightkit_agent_server::run(Arc::new(NoSkills), Arc::new(NoSkills));
}
