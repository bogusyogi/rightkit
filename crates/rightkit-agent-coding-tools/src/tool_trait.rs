//! Core `Tool` trait definition.

use crate::context::ToolContext;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkspaceMutationKind {
    Created,
    Modified,
    Deleted,
    Renamed { from: PathBuf },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceMutation {
    pub path: PathBuf,
    pub kind: WorkspaceMutationKind,
    pub previous_content: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ToolResult {
    pub content: String,
    pub is_error: bool,
    pub process_id: Option<String>,
    pub media_type: Option<String>,
    pub mutations: Vec<WorkspaceMutation>,
    /// Evidence-backed sandbox denial consumed by the shared EFF-001
    /// approval path. Kept out of model-controlled tool arguments.
    #[cfg(feature = "host")]
    pub sandbox_denial: Option<crate::sandbox::SandboxDenial>,
}

impl ToolResult {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
            process_id: None,
            media_type: None,
            mutations: Vec::new(),
            #[cfg(feature = "host")]
            sandbox_denial: None,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
            process_id: None,
            media_type: None,
            mutations: Vec::new(),
            #[cfg(feature = "host")]
            sandbox_denial: None,
        }
    }

    pub fn with_process_id(mut self, pid: Option<String>) -> Self {
        self.process_id = pid;
        self
    }

    pub fn with_media_type(mut self, media_type: impl Into<String>) -> Self {
        self.media_type = Some(media_type.into());
        self
    }

    pub fn with_mutations(mut self, mutations: Vec<WorkspaceMutation>) -> Self {
        self.mutations = mutations;
        self
    }

    #[cfg(feature = "host")]
    pub fn with_sandbox_denial(mut self, denial: crate::sandbox::SandboxDenial) -> Self {
        self.sandbox_denial = Some(denial);
        self
    }
}

#[async_trait::async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn input_schema(&self) -> Value;
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult;

    fn defer_schema_by_default(&self) -> bool {
        false
    }

    fn is_read_only(&self) -> bool {
        false
    }

    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        match self.capability_class() {
            Some(rightkit_agent_policy::CapabilityClass::Browser) => {
                Some(rightkit_agent_engine_runtime_resources::ResourceKind::BrowserProcess)
            }
            _ => None,
        }
    }

    fn process_resource_kind_for(
        &self,
        _args: &Value,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        self.process_resource_kind()
    }

    async fn preview(&self, _args: &Value, _ctx: &ToolContext) -> Option<String> {
        None
    }

    /// The workspace files this call may write, resolved the way `execute`
    /// resolves them (sessions rule 41). The engine's write path snapshots each
    /// one before the call runs, and fails the call with `history_unavailable`
    /// when it cannot; a tool never writes snapshots itself. Read-only tools and
    /// tools that write nothing in the workspace return none.
    fn write_targets(&self, _args: &Value, _ctx: &ToolContext) -> Vec<std::path::PathBuf> {
        Vec::new()
    }

    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        None
    }

    /// What a call of this tool does to the world (rule 50a). Derived from the declared
    /// capability class; a tool overrides it only to say something more specific.
    fn effect_class(&self) -> rightkit_agent_policy::EffectClass {
        rightkit_agent_policy::effect_class_for_tool(
            self.name(),
            self.capability_class(),
            self.is_read_only(),
        )
    }

    /// Who may approve a call of this tool (rule 50a).
    fn approval_class(&self) -> rightkit_agent_policy::ApprovalClass {
        self.effect_class().approval_class()
    }

    fn capability_class_for(
        &self,
        _args: &Value,
    ) -> Option<rightkit_agent_policy::CapabilityClass> {
        self.capability_class()
    }

    fn durable_write_kind(&self, _args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        None
    }
}

/// Transparent declaration wrapper for tools whose implementation lives in a
/// large/shared module and always requires the same process resource.
pub struct ProcessResourceTool<T> {
    inner: T,
    kind: rightkit_agent_engine_runtime_resources::ResourceKind,
}

impl<T> ProcessResourceTool<T> {
    pub fn new(inner: T, kind: rightkit_agent_engine_runtime_resources::ResourceKind) -> Self {
        Self { inner, kind }
    }
}

#[async_trait::async_trait]
impl<T> Tool for ProcessResourceTool<T>
where
    T: Tool,
{
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        self.inner.description()
    }
    fn input_schema(&self) -> Value {
        self.inner.input_schema()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        self.inner.execute(args, ctx).await
    }
    fn defer_schema_by_default(&self) -> bool {
        self.inner.defer_schema_by_default()
    }
    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }
    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(self.kind)
    }
    fn process_resource_kind_for(
        &self,
        _args: &Value,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        Some(self.kind)
    }
    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Option<String> {
        self.inner.preview(args, ctx).await
    }
    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        self.inner.capability_class()
    }
    fn capability_class_for(&self, args: &Value) -> Option<rightkit_agent_policy::CapabilityClass> {
        self.inner.capability_class_for(args)
    }
    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        self.inner.durable_write_kind(args)
    }
}

/// Transparent wrapper for mixed tools whose process requirement depends on
/// the concrete invocation. The selector is typed code attached at
/// registration; the engine remains free of tool-name special cases.
pub struct ProcessResourceSelectorTool<T, F> {
    inner: T,
    selector: F,
}

impl<T, F> ProcessResourceSelectorTool<T, F> {
    pub fn new(inner: T, selector: F) -> Self {
        Self { inner, selector }
    }
}

#[async_trait::async_trait]
impl<T, F> Tool for ProcessResourceSelectorTool<T, F>
where
    T: Tool,
    F: Fn(&Value) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> + Send + Sync,
{
    fn name(&self) -> &str {
        self.inner.name()
    }
    fn description(&self) -> &str {
        self.inner.description()
    }
    fn input_schema(&self) -> Value {
        self.inner.input_schema()
    }
    async fn execute(&self, args: Value, ctx: &ToolContext) -> ToolResult {
        self.inner.execute(args, ctx).await
    }
    fn defer_schema_by_default(&self) -> bool {
        self.inner.defer_schema_by_default()
    }
    fn is_read_only(&self) -> bool {
        self.inner.is_read_only()
    }
    fn process_resource_kind(
        &self,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        self.inner.process_resource_kind()
    }
    fn process_resource_kind_for(
        &self,
        args: &Value,
    ) -> Option<rightkit_agent_engine_runtime_resources::ResourceKind> {
        (self.selector)(args).or_else(|| self.inner.process_resource_kind_for(args))
    }
    async fn preview(&self, args: &Value, ctx: &ToolContext) -> Option<String> {
        self.inner.preview(args, ctx).await
    }
    fn capability_class(&self) -> Option<rightkit_agent_policy::CapabilityClass> {
        self.inner.capability_class()
    }
    fn capability_class_for(&self, args: &Value) -> Option<rightkit_agent_policy::CapabilityClass> {
        self.inner.capability_class_for(args)
    }
    fn durable_write_kind(&self, args: &Value) -> Option<rightkit_agent_policy::DurableWriteKind> {
        self.inner.durable_write_kind(args)
    }
}
