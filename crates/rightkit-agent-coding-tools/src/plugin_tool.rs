use crate::{Tool, ToolContext, ToolResult};
use rightkit_agent_plugins::{
    run_plugin_entry_with_policy_and_adapter, PluginDataAccess, PluginInvocation,
    PluginSandboxPolicy, PluginToolMetadata,
};
use serde_json::Value;
use std::path::PathBuf;

pub struct PluginTool {
    name: String,
    description: String,
    input_schema: Value,
    read_only: bool,
    network_access: bool,
    data_access: Vec<PluginDataAccess>,
    output_media_type: String,
    sandbox: PluginSandboxPolicy,
    plugin_id: String,
    plugin_root: PathBuf,
    entry: PathBuf,
}

impl PluginTool {
    pub fn new(
        name: impl Into<String>,
        plugin_id: impl Into<String>,
        plugin_root: impl Into<PathBuf>,
        entry: impl Into<PathBuf>,
        metadata: Option<PluginToolMetadata>,
        sandbox: PluginSandboxPolicy,
    ) -> Self {
        let metadata = metadata.unwrap_or_else(|| PluginToolMetadata {
            description: String::new(),
            input_schema: serde_json::json!({
                "type": "object",
                "additionalProperties": true
            }),
            read_only: false,
            network_access: false,
            data_access: Vec::new(),
            output_media_type: "text/plain; charset=utf-8".into(),
        });
        let name = name.into();
        let plugin_id = plugin_id.into();
        Self {
            description: if metadata.description.trim().is_empty() {
                format!("Tool contributed by Agent plugin {plugin_id}")
            } else {
                metadata.description
            },
            input_schema: metadata.input_schema,
            read_only: metadata.read_only,
            network_access: metadata.network_access,
            data_access: metadata.data_access,
            output_media_type: metadata.output_media_type,
            sandbox,
            name,
            plugin_id,
            plugin_root: plugin_root.into(),
            entry: entry.into(),
        }
    }
}

#[async_trait::async_trait]
impl Tool for PluginTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        self.input_schema.clone()
    }

    fn defer_schema_by_default(&self) -> bool {
        true
    }

    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        let launch_adapter = ctx
            .process_launch_adapter
            .clone()
            .unwrap_or_else(plugin_launch_adapter);
        match run_plugin_entry_with_policy_and_adapter(
            &self.entry,
            &self.plugin_root,
            PluginInvocation {
                kind: "tool".into(),
                name: self.name.clone(),
                args,
                session_id: ctx.session_id.clone(),
                workspace_root: ctx.workspace_root.clone(),
            },
            self.sandbox.clone(),
            launch_adapter,
        )
        .await
        {
            Ok(result) if result.is_error => {
                ToolResult::error(result.content).with_media_type(&self.output_media_type)
            }
            Ok(result) => ToolResult::ok(result.content).with_media_type(&self.output_media_type),
            Err(err) => ToolResult::error(format!(
                "plugin '{}' failed to run tool '{}': {err}",
                self.plugin_id, self.name
            )),
        }
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(rightkit_agent_engine_runtime_resources::ResourceKind::ChildProcess)
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        if self.data_access.contains(&PluginDataAccess::Secrets) {
            Some(rightkit_agent_policy::CapabilityClass::Secrets)
        } else if self.network_access {
            Some(rightkit_agent_policy::CapabilityClass::Network)
        } else {
            // Every plugin tool executes a subprocess, including read-only
            // tools, so name inference must never classify it as harmless.
            Some(rightkit_agent_policy::CapabilityClass::Shell)
        }
    }

    fn durable_write_kind(&self, _args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        (!self.read_only || self.data_access.contains(&PluginDataAccess::WorkspaceWrite))
            .then_some(rightkit_agent_policy::DurableWriteKind::ShellCommand)
    }
}

/// Production launch adapter for plugin subprocesses: the real, enforced
/// sandbox seam (CG-B02). Tests that exercise plugin dispatch/loading logic
/// rather than confinement itself may override this via
/// [`set_test_launch_adapter`] since a confined child cannot always reach a
/// plain `tempdir()` fixture plugin script on every platform.
fn plugin_launch_adapter() -> std::sync::Arc<dyn rightkit_sandbox::execution::ProcessLaunchAdapter>
{
    std::sync::Arc::new(crate::shell::NativeCommandSandboxAdapter)
}
