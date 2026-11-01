//! Launch specification for `agy` print-mode turns.
//!
//! The shape mirrors `ClaudeLaunchOptions::launch_spec`: a `LaunchSpec` with
//! the vendor binary as `program`, the same `FORBIDDEN_BYOK_ENV_VARS`
//! environment denial, and the workspace root as `cwd`. The differences are
//! the `agy` flag surface: `-p` + `--output-format stream-json` for the wire,
//! `--conversation <id>` (not `--resume`) for continuity, `--add-dir` for the
//! workspace grant, and `--dangerously-skip-permissions` for the host app's
//! `Bypass` permission mode.
//!
//! agy has no `--tools ""`/allowed-tools flag, so the Claude-style "native
//! tool surface removed" launch policy cannot be expressed for this vendor:
//! its native tools stay enabled and are governed by the vendor's own
//! permission mode (default `request-review`; auto-approved only under
//! `dangerously_skip_permissions`).

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::driver_host::cli_runtime::auth::FORBIDDEN_BYOK_ENV_VARS;
use crate::driver_host::{invalid, validate_model_id, DriverHostError, DriverKind, LaunchSpec};

/// Launch options for one `agy` print-mode turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgyLaunchOptions {
    pub executable: PathBuf,
    /// Turn cwd — normally the session workspace root. `agy` spawns its MCP
    /// servers in its own cwd, so the registered `host app` stdio server
    /// (no `--workspace` arg) projects this directory.
    pub cwd: PathBuf,
    /// Vendor conversation id from a previous `init` event, passed back as
    /// `--conversation <id>` for session continuity across turns.
    pub conversation_id: Option<String>,
    /// `--model <id>` — an `agy models` id. Validated against
    /// [`validate_model_id`] before reaching argv.
    pub model: Option<String>,
    /// the host app `Bypass` (yolo) permission mode: auto-approve every vendor
    /// permission request via `--dangerously-skip-permissions`. Every other
    /// mode keeps the vendor's default `request-review` policy.
    pub dangerously_skip_permissions: bool,
    /// Extra directories the agent may access (`--add-dir`, repeatable).
    pub add_dirs: Vec<PathBuf>,
}

impl AgyLaunchOptions {
    pub fn launch_spec(&self) -> Result<LaunchSpec, DriverHostError> {
        let mut args = vec![
            "--print".into(),
            "--output-format".into(),
            "stream-json".into(),
        ];
        if let Some(model) = self.model.as_deref() {
            let model = validate_model_id(model)
                .map_err(|reason| invalid(DriverKind::Antigravity, reason))?;
            args.extend(["--model".into(), model.to_string()]);
        }
        if let Some(id) = self.conversation_id.as_deref() {
            let id =
                validate_model_id(id).map_err(|reason| invalid(DriverKind::Antigravity, reason))?;
            args.extend(["--conversation".into(), id.to_string()]);
        }
        for dir in &self.add_dirs {
            args.extend(["--add-dir".into(), dir.to_string_lossy().into_owned()]);
        }
        if self.dangerously_skip_permissions {
            args.push("--dangerously-skip-permissions".into());
        }
        Ok(LaunchSpec {
            program: self.executable.clone(),
            args,
            env: Default::default(),
            env_remove: FORBIDDEN_BYOK_ENV_VARS
                .iter()
                .map(|name| (*name).to_string())
                .collect(),
            cwd: self.cwd.clone(),
        })
    }
}

/// The user-prompt argv for `agy -p`.
///
/// agy's `-p/--print` flag consumes the prompt text itself rather than an
/// NDJSON frame on stdin (unlike Claude Code's `--input-format stream-json`),
/// so the prompt is appended as the flag's argument.
pub fn user_prompt_frame(prompt: &str) -> Vec<String> {
    vec!["-p".into(), prompt.to_string()]
}

/// Build the `agy mcp add` argv that registers the the host app stdio gateway.
/// Extracted so the registration helper and its tests share one source of
/// argv truth: `agy mcp add --type stdio <name> <command> -- <args...>`.
pub fn mcp_add_argv(server_name: &str, command: &std::path::Path, args: &[String]) -> Vec<String> {
    let mut argv = vec![
        "mcp".into(),
        "add".into(),
        "--type".into(),
        "stdio".into(),
        server_name.into(),
        command.to_string_lossy().into_owned(),
        "--".into(),
    ];
    argv.extend(args.iter().cloned());
    argv
}

/// Serialize a `tools/list`-shaped MCP server entry — kept for parity with the
/// Claude harness's `mcp_config` JSON where a config file is expected.
#[allow(dead_code)]
pub(crate) fn mcp_server_json(command: &std::path::Path, args: &[String]) -> Value {
    json!({
        "type": "stdio",
        "command": command.to_string_lossy(),
        "args": args,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> AgyLaunchOptions {
        AgyLaunchOptions {
            executable: "agy.exe".into(),
            cwd: "D:\\work".into(),
            conversation_id: None,
            model: None,
            dangerously_skip_permissions: false,
            add_dirs: vec![],
        }
    }

    #[test]
    fn base_launch_is_print_stream_json() {
        let spec = options().launch_spec().unwrap();
        assert_eq!(spec.args, ["--print", "--output-format", "stream-json"]);
        assert_eq!(spec.cwd, PathBuf::from("D:\\work"));
    }

    #[test]
    fn model_conversation_and_dirs_are_appended() {
        let mut opts = options();
        opts.model = Some("gemini-3.8-flash-high".into());
        opts.conversation_id = Some("conv-1".into());
        opts.add_dirs = vec!["D:\\work".into(), "D:\\shared".into()];
        let spec = opts.launch_spec().unwrap();
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--model", "gemini-3.8-flash-high"]));
        assert!(spec
            .args
            .windows(2)
            .any(|pair| pair == ["--conversation", "conv-1"]));
        assert_eq!(
            spec.args.iter().filter(|arg| *arg == "--add-dir").count(),
            2
        );
    }

    #[test]
    fn bypass_maps_to_dangerously_skip_permissions() {
        let mut opts = options();
        opts.dangerously_skip_permissions = true;
        let spec = opts.launch_spec().unwrap();
        assert!(spec
            .args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
        assert!(!options()
            .launch_spec()
            .unwrap()
            .args
            .iter()
            .any(|arg| arg == "--dangerously-skip-permissions"));
    }

    #[test]
    fn hostile_model_and_conversation_ids_are_rejected() {
        let mut opts = options();
        opts.model = Some("gpt; rm -rf /".into());
        assert!(opts.launch_spec().is_err());
        opts.model = None;
        opts.conversation_id = Some("conv with spaces".into());
        assert!(opts.launch_spec().is_err());
    }

    #[test]
    fn launch_spec_denies_provider_keys_and_daemon_tokens() {
        let spec = options().launch_spec().unwrap();
        for name in [
            "OPENROUTER_API_KEY",
            "MINIMAX_API_KEY",
            "RIGHTKIT_DAEMON_TOKEN",
            "RIGHTKIT_GATEWAY_TOKEN",
        ] {
            assert!(spec.env_remove.contains(name));
        }
    }

    #[test]
    fn mcp_add_argv_places_separator_before_command_args() {
        let argv = mcp_add_argv(
            "rightkit-agent",
            std::path::Path::new("rightkit-agent.exe"),
            &["mcp-server".into(), "--workspace".into(), "D:\\w".into()],
        );
        assert_eq!(
            argv,
            [
                "mcp",
                "add",
                "--type",
                "stdio",
                "rightkit-agent",
                "rightkit-agent.exe",
                "--",
                "mcp-server",
                "--workspace",
                "D:\\w",
            ]
        );
    }
}
