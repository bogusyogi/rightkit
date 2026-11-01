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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_parse_maps_tool_classes() {
        assert_eq!(
            parse_capability(Some("read_file")),
            CapabilityClass::ReadFile
        );
        // CR-30: the dedicated retrieval schema was removed; externalized
        // results are read through ordinary `read_file` at the virtual path.
        // A stale declaration must fall through to Unknown (gated), never
        // silently ride along as read-only.
        assert_eq!(
            parse_capability(Some("read_tool_result")),
            CapabilityClass::Unknown
        );
        assert_eq!(
            parse_capability(Some("list_files")),
            CapabilityClass::ReadFile
        );
        assert_eq!(parse_capability(Some("search")), CapabilityClass::ReadFile);
        // Tool discovery must never raise an approval prompt.
        assert_eq!(
            parse_capability(Some("tool_search")),
            CapabilityClass::ReadFile
        );
        assert_eq!(
            parse_capability(Some("get_time")),
            CapabilityClass::ReadFile
        );
        assert_eq!(
            parse_capability(Some("git_read")),
            CapabilityClass::ReadFile
        );
        assert_eq!(
            parse_capability(Some("git_diff")),
            CapabilityClass::ReadFile
        );
        assert_eq!(
            parse_capability(Some("list_mcp_tools")),
            CapabilityClass::ReadFile
        );
        assert_eq!(
            parse_capability(Some("list_mcp_resources")),
            CapabilityClass::ReadFile
        );
        assert_eq!(
            parse_capability(Some("read_mcp_resource")),
            CapabilityClass::ReadFile
        );
        assert_eq!(
            parse_capability(Some("todo_write")),
            CapabilityClass::ReadFile
        );
        assert_eq!(parse_capability(Some("lsp")), CapabilityClass::ReadFile);
        assert_eq!(
            parse_capability(Some("write_file")),
            CapabilityClass::WriteFile
        );
        assert_eq!(
            parse_capability(Some("edit_file")),
            CapabilityClass::WriteFile
        );
        assert_eq!(
            parse_capability(Some("apply_patch")),
            CapabilityClass::WriteFile
        );
        assert_eq!(parse_capability(Some("shell")), CapabilityClass::Shell);
        assert_eq!(parse_capability(Some("powershell")), CapabilityClass::Shell);
        assert_eq!(parse_capability(Some("media")), CapabilityClass::Media);
        assert_eq!(parse_capability(Some("ocr")), CapabilityClass::Media);
        assert_eq!(parse_capability(Some("mcp")), CapabilityClass::Mcp);
        assert_eq!(
            parse_capability(Some("task_get")),
            CapabilityClass::ReadFile
        );
        // Bookkeeping, not process control: AutoSafe must not pause on it, or
        // a sub-agent stalls on the conductor's own task call before working.
        assert_eq!(
            parse_capability(Some("task_create")),
            CapabilityClass::AddContext
        );
        assert_eq!(
            parse_capability(Some("task_stop")),
            CapabilityClass::ProcessControl
        );
        assert_eq!(parse_capability(Some("bogus")), CapabilityClass::Unknown);
        assert_eq!(parse_capability(None), CapabilityClass::Unknown);
    }

    #[test]
    fn is_read_only_only_for_read_file() {
        assert!(is_read_only(CapabilityClass::ReadFile));
        assert!(!is_read_only(CapabilityClass::WriteFile));
        assert!(!is_read_only(CapabilityClass::Shell));
        assert!(!is_read_only(CapabilityClass::Media));
        assert!(!is_read_only(CapabilityClass::Unknown));
    }

    #[test]
    fn is_file_write_for_write_and_git_write() {
        assert!(is_file_write(CapabilityClass::WriteFile));
        assert!(is_file_write(CapabilityClass::GitWrite));
        assert!(!is_file_write(CapabilityClass::ReadFile));
        assert!(!is_file_write(CapabilityClass::Shell));
    }
}
