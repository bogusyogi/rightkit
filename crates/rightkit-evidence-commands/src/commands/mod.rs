//! Evidence and run command logic: argument structs and handlers that return
//! JSON results or a typed [`CommandError`]. The host CLI registers the
//! commands and prints the results.

pub mod budget;
pub mod completion;
pub mod contract;
pub mod decision;
pub mod explain;
pub mod freeze;
mod freeze_consumer;
mod freeze_delivery;
mod freeze_git;
mod freeze_review;
mod freeze_store;
pub mod governance;
pub mod init;
pub mod keys;
pub mod run;
pub mod schedule;
pub mod state;

use serde_json::Value;
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
};

/// Host-owned names the commands use: the CLI program shown in usage text,
/// the repository workspace directory and config file, and the system
/// authority-store directories. Declared once with [`set_command_host`];
/// otherwise the neutral default (`rightkit`, `.rightkit`) applies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandHost {
    /// CLI program name shown in usage text.
    pub program: String,
    /// Repository-relative workspace directory (decision store, ignore entry).
    pub workspace_dir: String,
    /// Repository-relative config file written by `init`.
    pub config_file: String,
    /// Vendor directory of the macOS authority store.
    pub vendor_dir: String,
    /// Application directory of the macOS authority store.
    pub app_dir: String,
    /// Directory under `/var/lib` of the Linux authority store.
    pub xdg_dir: String,
}

impl Default for CommandHost {
    fn default() -> Self {
        Self {
            program: "rightkit".to_string(),
            workspace_dir: ".rightkit".to_string(),
            config_file: "rightkit.config.json".to_string(),
            vendor_dir: "RightKit".to_string(),
            app_dir: "RightKit".to_string(),
            xdg_dir: "rightkit".to_string(),
        }
    }
}

static COMMAND_HOST: std::sync::OnceLock<CommandHost> = std::sync::OnceLock::new();

/// Declare the host's command names. The first call wins; it returns `false`
/// when a host was already set.
pub fn set_command_host(host: CommandHost) -> bool {
    COMMAND_HOST.set(host).is_ok()
}

/// The command host in effect for this process.
pub fn command_host() -> &'static CommandHost {
    COMMAND_HOST.get_or_init(CommandHost::default)
}

/// The CLI program name shown in usage text.
pub fn program_name() -> &'static str {
    &command_host().program
}

/// Result of one command: a JSON document or a typed failure.
pub type CommandResult = Result<Value, CommandError>;

/// Arguments shared by commands that take free-form trailing arguments.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct CommonArgs {
    #[arg(long)]
    pub json: bool,
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<OsString>,
}

/// Strip the Windows verbatim prefix for display.
pub fn display_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(&text))
}

/// Build the native application from `RIGHTKIT_NATIVE_APPLICATION_CONFIG`
/// (inline JSON or a path).
pub fn native_application_for(
    repository_id: &str,
) -> Result<Arc<rightkit_evidence_application::NativeApplication>, CommandError> {
    if let Ok(configured) = std::env::var("RIGHTKIT_NATIVE_APPLICATION_CONFIG") {
        return rightkit_evidence_application::NativeApplicationConfig::from_versioned_source(
            &configured,
        )
        .and_then(rightkit_evidence_application::NativeApplicationConfig::build)
        .map(Arc::new)
        .map_err(|error| {
            CommandError::incomplete(format!("native application rejected: {error}"))
        });
    }
    Err(CommandError::incomplete(format!(
        "native application config is required for repository {repository_id}"
    )))
}

/// The host-injected audit plan signing key (`AUDIT_PLAN_SIGNING_KEY`).
pub fn audit_signing_key() -> Result<Vec<u8>, CommandError> {
    std::env::var_os("AUDIT_PLAN_SIGNING_KEY")
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string_lossy().as_bytes().to_vec())
        .ok_or_else(|| {
            CommandError::incomplete("native audit requires host-injected AUDIT_PLAN_SIGNING_KEY")
        })
}

/// The sole inventory source: a read-only local filesystem walk.
pub fn audit_inventory_source(
    root: &Path,
) -> Result<Arc<rightkit_code_audit_engine::FilesystemInventorySource>, CommandError> {
    rightkit_code_audit_engine::FilesystemInventorySource::new(root)
        .map(Arc::new)
        .map_err(|error| CommandError::incomplete(error.to_string()))
}

/// [`audit_inventory_source`] narrowed to `only_paths` (diff-scoped audits);
/// `None` is the whole-repository inventory.
pub fn audit_inventory_source_scoped(
    root: &Path,
    only_paths: Option<&std::collections::BTreeSet<String>>,
) -> Result<Arc<rightkit_code_audit_engine::FilesystemInventorySource>, CommandError> {
    rightkit_code_audit_engine::FilesystemInventorySource::new(root)
        .map(|source| Arc::new(source.with_only_paths(only_paths.cloned())))
        .map_err(|error| CommandError::incomplete(error.to_string()))
}

/// A command failure with its process exit code.
#[derive(Debug)]
pub struct CommandError {
    pub code: i32,
    pub message: String,
}

impl CommandError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            code: 4,
            message: message.into(),
        }
    }
    pub fn incomplete(message: impl Into<String>) -> Self {
        Self {
            code: 2,
            message: message.into(),
        }
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self {
            code: 3,
            message: message.into(),
        }
    }
    pub fn cancelled() -> Self {
        Self {
            code: 2,
            message: "CANCELLED: task cancelled by Ctrl-C".into(),
        }
    }
    pub fn integrity(message: impl Into<String>) -> Self {
        Self {
            code: 5,
            message: message.into(),
        }
    }
    pub fn policy(message: impl Into<String>) -> Self {
        Self {
            code: 1,
            message: message.into(),
        }
    }
}

/// Map any displayable failure to an internal command error.
pub fn io_error(error: impl std::fmt::Display) -> CommandError {
    CommandError::internal(error.to_string())
}
