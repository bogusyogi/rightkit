use serde::{Deserialize, Serialize};

/// Capability classes (spec §13). Every tool is gated by exactly one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CapabilityClass {
    ReadFile,
    WriteFile,
    Shell,
    /// Native OCR/transcription/extraction/fetch tools. Kept separate from
    /// broad shell authority so a role can process media without arbitrary
    /// command execution (T04).
    Media,
    Network,
    Browser,
    Mcp,
    Secrets,
    GitWrite,
    ProcessControl,
    Destructive,
    ArtifactTrust,
    AddContext,
    /// Tool declared a class we don't recognize — treated as effectful (gated), never silently allowed.
    Unknown,
}

/// Map a tool's `permission_class` string (e.g. `"read_file"`) to a [`CapabilityClass`].
pub fn parse_capability(permission_class: Option<&str>) -> CapabilityClass {
    match permission_class {
        Some("read_file")
        | Some("list_files")
        | Some("search")
        | Some("git_read")
        | Some("git_diff")
        | Some("list_mcp_tools")
        | Some("list_mcp_resources")
        | Some("read_mcp_resource")
        | Some("todo_write")
        // Pure discovery/clock meta tools: no workspace or network effect.
        | Some("tool_search")
        | Some("get_time")
        | Some("lsp")
        | Some("task_get")
        | Some("task_list") => CapabilityClass::ReadFile,
        Some("write_file") | Some("edit_file") | Some("multi_edit") | Some("undo_edit")
        | Some("apply_patch") => CapabilityClass::WriteFile,
        Some("shell") | Some("powershell") => CapabilityClass::Shell,
        Some("media")
        | Some("ocr")
        | Some("ocr_onnx")
        | Some("transcribe")
        | Some("web_extract")
        | Some("media_fetch")
        | Some("ffmpeg_extract") => CapabilityClass::Media,
        Some("network") => CapabilityClass::Network,
        Some("browser") => CapabilityClass::Browser,
        Some("mcp") | Some("mcp_call") => CapabilityClass::Mcp,
        Some("secrets") => CapabilityClass::Secrets,
        Some("git_write") => CapabilityClass::GitWrite,
        Some("process_control") => CapabilityClass::ProcessControl,
        // Only `task_stop` controls a process — it terminates running work.
        // Creating, updating and reading a task is orchestration bookkeeping in
        // the task store, already bounded by worker caps, budget ceilings and
        // spawn-depth rules. Classing all four as ProcessControl meant AutoSafe
        // paused on `task_create`, so a sub-agent (which runs AutoSafe and has
        // no way to answer an approval) stalled on the conductor's own
        // bookkeeping call before doing any work.
        Some("task_stop") => CapabilityClass::ProcessControl,
        Some("task_create") | Some("task_update") | Some("task_output") => {
            CapabilityClass::AddContext
        }
        Some("destructive") => CapabilityClass::Destructive,
        Some("artifact_trust") => CapabilityClass::ArtifactTrust,
        Some("add_context") => CapabilityClass::AddContext,
        _ => CapabilityClass::Unknown,
    }
}

/// Read-only classes never mutate the workspace and are allowed in every state except `Locked`.
pub fn is_read_only(cls: CapabilityClass) -> bool {
    cls == CapabilityClass::ReadFile
}

/// Classes that write the filesystem — the deny-glob write list applies to these.
pub fn is_file_write(cls: CapabilityClass) -> bool {
    matches!(cls, CapabilityClass::WriteFile | CapabilityClass::GitWrite)
}
