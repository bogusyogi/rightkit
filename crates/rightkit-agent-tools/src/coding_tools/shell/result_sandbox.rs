fn sandbox_operator_tier_value() -> Option<String> {
    std::env::var("RIGHTKIT_SANDBOX_TIER")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn sandbox_tier_value_from_workspace_file(workspace_root: &Path) -> Option<String> {
    let path = workspace_root
        .join(".rightkit")
        .join("sandbox-settings.json");
    let content = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&content).ok()?;
    value
        .get("tier")?
        .as_str()
        .map(|tier| tier.trim().to_string())
}

/// Largest captured-output copy kept for recovery (matches the durable
/// tool-result store ceiling). Larger output is stored truncated and reported
/// as partial.
const SHELL_OUTPUT_SPILL_MAX_BYTES: usize = 8 * 1024 * 1024;
const SHELL_OUTPUT_SPILL_KEEP_FILES: usize = 64;

/// Why a recovery copy of shell output is unavailable. Typed so callers never
/// see an empty success or a raw IO string for a copy that is gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellOutputUnavailable {
    /// The copy was stored, then removed by keep-count retention.
    Evicted,
    /// The copy could not be written, so no handle was issued.
    WriteFailed,
    /// No copy exists at the path and its directory does not hold one.
    Missing,
}

impl ShellOutputUnavailable {
    pub fn label(self) -> &'static str {
        match self {
            Self::Evicted => "evicted",
            Self::WriteFailed => "write failed",
            Self::Missing => "missing",
        }
    }
}

/// Outcome of persisting a recovery copy.
#[derive(Debug, PartialEq, Eq)]
enum ShellOutputSpill {
    /// `handle` is the text appended to the reduced result.
    Stored {
        handle: String,
    },
    Unavailable(ShellOutputUnavailable),
}

/// Persist the shell output and return the recovery-handle text to append to
/// the reduced result. A failed write is reported as typed unavailable; it
/// never fails the command.
fn spill_full_shell_output(
    workspace_root: Option<&Path>,
    session_id: &str,
    full: &str,
) -> ShellOutputSpill {
    // the host app's own session store, never the user's workspace.
    let _ = workspace_root;
    match crate::coding_tools::output_compaction::session_store_root() {
        Some(base) => spill_shell_output_into(&base, session_id, full),
        None => ShellOutputSpill::Unavailable(ShellOutputUnavailable::WriteFailed),
    }
}

fn spill_shell_output_into(base: &Path, session_id: &str, full: &str) -> ShellOutputSpill {
    let dir = base
        .join(crate::coding_tools::output_compaction::session_store_dir_name(session_id))
        .join("shell-output");
    if std::fs::create_dir_all(&dir).is_err() {
        return ShellOutputSpill::Unavailable(ShellOutputUnavailable::WriteFailed);
    }

    let captured = full.len();
    let mut stored = captured.min(SHELL_OUTPUT_SPILL_MAX_BYTES);
    while stored > 0 && !full.is_char_boundary(stored) {
        stored -= 1;
    }
    let truncated = stored < captured;
    let mut content = full[..stored].to_string();
    if truncated {
        content.push_str(&format!(
            "\n[recovery copy truncated at {stored} of {captured} bytes]\n"
        ));
    }
    let digest = rightkit_crypto::sha256(content.as_bytes());
    let name = format!("{}.txt", rightkit_crypto::hex_encode(&digest[..8]));
    let path = dir.join(name);
    if std::fs::write(&path, content.as_bytes()).is_err() {
        return ShellOutputSpill::Unavailable(ShellOutputUnavailable::WriteFailed);
    }

    // Bound disk use: keep only the most recent spills for this session.
    if let Ok(entries) = std::fs::read_dir(&dir) {
        let mut files: Vec<(std::time::SystemTime, std::path::PathBuf)> = entries
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                let modified = entry.metadata().ok()?.modified().ok()?;
                Some((modified, entry.path()))
            })
            .collect();
        if files.len() > SHELL_OUTPUT_SPILL_KEEP_FILES {
            files.sort_by_key(|(modified, _)| *modified);
            let excess = files.len() - SHELL_OUTPUT_SPILL_KEEP_FILES;
            for (_, stale) in files.into_iter().take(excess) {
                if stale != path {
                    let _ = std::fs::remove_file(stale);
                }
            }
        }
    }
    // "full" is claimed only when every captured byte is in the stored copy.
    let handle = if truncated {
        format!(
            "\n[output truncated: captured {captured} bytes, stored {stored} bytes (partial copy, not the complete output) at {}; read that file (offset/limit) to recover the omitted lines]\n",
            path.display()
        )
    } else {
        format!(
            "\n[full output ({captured} bytes captured and stored) at {}; read that file (offset/limit) to recover the omitted lines]\n",
            path.display()
        )
    };
    ShellOutputSpill::Stored { handle }
}

/// Read a stored recovery copy back, or say typed why it is unavailable.
pub fn read_shell_output_recovery(path: &Path) -> Result<String, ShellOutputUnavailable> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(text),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Err(absent_recovery_reason(path)),
        // Unreadable copies are reported as missing, never as an empty success.
        Err(_) => Err(ShellOutputUnavailable::Missing),
    }
}

/// A handle is issued only after a successful write, so an absent copy in an
/// existing shell-output directory was removed by retention.
fn absent_recovery_reason(path: &Path) -> ShellOutputUnavailable {
    match path.parent() {
        Some(dir) if dir.is_dir() => ShellOutputUnavailable::Evicted,
        _ => ShellOutputUnavailable::Missing,
    }
}

/// For an absolute path inside this session's shell-output store that cannot
/// be read, the typed reason. `None` for other paths and for readable copies.
pub fn shell_output_recovery_unavailable(
    path: &Path,
    session_id: &str,
) -> Option<ShellOutputUnavailable> {
    let root = crate::coding_tools::output_compaction::session_store_root()?;
    let rest = path.strip_prefix(&root).ok()?;
    let mut parts = rest.components();
    let session_dir = crate::coding_tools::output_compaction::session_store_dir_name(session_id);
    if parts.next()?.as_os_str() != session_dir.as_str()
        || parts.next()?.as_os_str() != "shell-output"
    {
        return None;
    }
    read_shell_output_recovery(path).err()
}

fn output_to_result(
    output: std::process::Output,
    process_id: Option<String>,
    ctx: Option<&ToolContext>,
    command: &str,
) -> ToolResult {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // Typed signaled status (unix only): `code()` is `None` when the reaped
    // child was terminated by a signal.
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&output.status);
    #[allow(unused_mut)]
    let mut result = output_parts_to_result(
        output.status.code().unwrap_or(-1),
        output.status.success(),
        &stdout,
        &stderr,
        process_id,
        ctx,
        command,
    );
    #[cfg(unix)]
    if let Some(signal) = signal {
        let status = rightkit_agent_engine_protocol::ShellExitStatus::Signaled {
            signal: Some(signal),
        };
        result.content = format!("[status: {}]\n{}", status.label(), result.content);
    }
    result
}

fn output_parts_to_result(
    exit_code: i32,
    exit_ok: bool,
    stdout: &str,
    stderr: &str,
    process_id: Option<String>,
    ctx: Option<&ToolContext>,
    command: &str,
) -> ToolResult {
    let mut combined = String::new();
    combined.push_str(&stdout);
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push('\n');
        }
        combined.push_str("[stderr]\n");
        combined.push_str(&stderr);
    }

    let full_copy = crate::coding_tools::output_compaction::recovery_copy(&combined);
    let mut combined = crate::coding_tools::output_compaction::reduce_output(&combined, exit_ok, command);
    // Never lose the only full copy: when reduction omitted content, keep the
    // complete (masked) output durably and tell the model how to fetch it.
    if crate::coding_tools::output_compaction::reduction_omitted_content(&full_copy, &combined) {
        let session = ctx.map(|ctx| ctx.session_id.as_str()).unwrap_or("detached");
        let root = ctx.map(|ctx| ctx.workspace_root.as_path());
        match spill_full_shell_output(root, session, &full_copy) {
            ShellOutputSpill::Stored { handle } => combined.push_str(&handle),
            ShellOutputSpill::Unavailable(reason) => combined.push_str(&format!(
                "\n[recovery of omitted output unavailable: {}]\n",
                reason.label()
            )),
        }
    }

    // Retain denial evidence for the shared EFF-001 approval path.
    let denial = ctx.and_then(|ctx| {
        let tier = crate::coding_tools::shell::effective_sandbox_tier(
            &ctx.workspace_root,
            Some(ctx.sandbox_permission_mode()),
        );
        if tier.is_disabled() {
            return None;
        }
        let source = if cfg!(windows) {
            crate::coding_tools::sandbox::DenialSource::WindowsAppContainer
        } else if cfg!(target_os = "macos") {
            crate::coding_tools::sandbox::DenialSource::MacosSeatbelt
        } else if cfg!(target_os = "linux") {
            crate::coding_tools::sandbox::DenialSource::LinuxLandlockSeccomp
        } else {
            crate::coding_tools::sandbox::DenialSource::Unknown
        };
        crate::coding_tools::sandbox::report_confined_denial(
            &combined,
            command,
            exit_ok,
            &crate::coding_tools::sandbox::ConfinementInfo {
                source,
                tier,
                network_allowed: !tier.denies_network()
                    || ctx
                        .sandbox_retry_grant
                        .as_ref()
                        .is_some_and(|grant| grant.network),
                workspace_root: ctx.workspace_root.clone(),
            },
        )
    });
    if let Some(denial) = &denial {
        combined.push('\n');
        combined.push_str(&denial.render());
    }
    // Windows limit, not a RightKit gap: git's realpath lstat()s every ancestor, and an
    // admin-owned ancestor such as C:\Users cannot be leased to the container. RightKit's
    // `AppContainerAncestorLease` already reports those ancestors (`unleasable()`); this only
    // explains the resulting git error to the user.
    if cfg!(windows)
        && !exit_ok
        && combined.contains("failed to stat '")
        && combined.contains("Permission denied")
    {
        combined.push_str(
            "\n[sandbox] git could not read attributes of an admin-owned workspace ancestor the sandbox cannot lease (Windows limit for workspaces under C:\\Users: a non-admin cannot grant the container access to it).",
        );
    }

    if let Some(ctx) = ctx {
        record_foreground_process(
            ctx,
            command,
            &combined,
            crate::coding_tools::background_shell::BgStatus::Exited(exit_code),
            process_id.clone(),
        );
    }
    let result = if exit_ok && denial.is_none() {
        ToolResult::ok(combined)
    } else {
        ToolResult::error(format!("[exit {}]\n{}", exit_code, combined))
    };
    let result = result.with_process_id(process_id);
    match denial {
        Some(denial) => result.with_sandbox_denial(denial),
        None => result,
    }
}
