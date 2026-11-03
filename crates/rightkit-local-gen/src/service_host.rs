//! The local-image service as a set of named engine commands (`local_image.*`): one lazily
//! started [`LocalImageService`] per host, its job and install events forwarded through the
//! host's [`EventCallback`], and the service shut down with the host.
//!
//! An engine registers [`COMMANDS`] (with [`PREFIX`]) and forwards each request to
//! [`Host::dispatch`]. Errors keep the service's own text; the code is the text's `snake_case:`
//! prefix when it has one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use serde_json::Value;

use crate::commands as li;
use crate::paths::{self, EnvValues};
use crate::{
    settings, EventCallback, LocalImageService, ServiceOptions, DEFAULT_IDLE_UNLOAD_SECONDS,
};

/// A command failure: a stable `code` and the service's whole message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostError {
    /// Stable, snake_case code.
    pub code: String,
    /// The original message.
    pub message: String,
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for HostError {}

/// Prefix of every command name.
pub const PREFIX: &str = "local_image.";

pub fn is_command(name: &str) -> bool {
    name.starts_with(PREFIX)
}

/// The commands `run` serves (without [`PREFIX`]). The engine wire registers every method by exact
/// name, so keep this list beside the `match` in `run`.
pub const COMMANDS: &[&str] = &[
    "profiles",
    "runtime",
    "submit",
    "get",
    "list",
    "cancel",
    "resume",
    "import_reference",
    "install",
    "install.pause",
    "install.cancel",
    "unload",
    "set_idle_unload",
    "set_memory_mode",
];

pub struct Host {
    /// The app data root (the settings document lives under it).
    app_data: PathBuf,
    events: EventCallback,
    service: OnceLock<Result<LocalImageService, String>>,
}

impl Host {
    pub fn new(app_data: PathBuf, events: EventCallback) -> Self {
        Self {
            app_data,
            events,
            service: OnceLock::new(),
        }
    }

    /// Start (once) and return the service. Blocks while another thread is starting it.
    fn service(&self) -> Result<LocalImageService, String> {
        self.service.get_or_init(|| self.start()).clone()
    }

    /// Begin the service now, off the caller's thread, so the first request does not pay for it.
    pub fn warm(self: &Arc<Self>) {
        let host = Arc::clone(self);
        let _ = std::thread::Builder::new()
            .name("rightkit-local-gen-start".to_string())
            .spawn(move || {
                let _ = host.service();
            });
    }

    fn start(&self) -> Result<LocalImageService, String> {
        let env = EnvValues::from_process();
        let saved_model_root = settings::read(&self.app_data).model_root.map(PathBuf::from);
        let resolved = paths::local_image(
            &self.app_data,
            &paths::content_root(&env),
            saved_model_root.as_deref(),
            &env,
            &exe_dirs(),
        );
        let idle_dir = self.app_data.clone();
        let memory_dir = self.app_data.clone();
        let options = ServiceOptions {
            events: Arc::clone(&self.events),
            idle_unload_seconds: Arc::new(move || {
                settings::read(&idle_dir)
                    .idle_unload_seconds
                    .unwrap_or(DEFAULT_IDLE_UNLOAD_SECONDS)
            }),
            worker_request_prefix: None,
            memory_mode: Arc::new(move || settings::read(&memory_dir).memory_mode),
            ..ServiceOptions::default()
        };
        LocalImageService::start_with_options(resolved, options)
    }

    /// Stop the service if it was started (the model server and the owner lock end with it).
    pub fn shutdown(&self) {
        if let Some(Ok(service)) = self.service.get() {
            service.shutdown();
        }
    }

    pub fn dispatch(&self, name: &str, input: &Value) -> Result<Value, HostError> {
        let service = self.service().map_err(|message| HostError {
            code: "local_image_unavailable".to_string(),
            message,
        })?;
        run(&service, &self.app_data, name, input).map_err(error_from_message)
    }
}

fn run(
    service: &LocalImageService,
    app_data: &Path,
    name: &str,
    input: &Value,
) -> Result<Value, String> {
    let text = |key: &str| -> Result<String, String> {
        input
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| format!("invalid_request: {key} is required"))
    };
    let encode =
        |value: Result<Value, serde_json::Error>| value.map_err(|e| format!("encode: {e}"));
    match name.strip_prefix(PREFIX).unwrap_or(name) {
        "profiles" => li::profiles_blocking(service),
        "runtime" => encode(serde_json::to_value(li::runtime_blocking(service)?)),
        "submit" => {
            let request: li::SubmitRequest = serde_json::from_value(input.clone())
                .map_err(|e| format!("invalid_request: {e}"))?;
            encode(serde_json::to_value(li::submit_blocking(service, request)?))
        }
        "get" => encode(serde_json::to_value(li::get_blocking(
            service,
            text("job_id")?,
        )?)),
        "list" => li::list_blocking(
            service,
            input
                .get("conversation_id")
                .and_then(Value::as_str)
                .map(str::to_string),
            input
                .get("limit")
                .and_then(Value::as_u64)
                .map(|n| n.min(u64::from(u32::MAX)) as u32),
        ),
        "cancel" => li::cancel_blocking(service, text("job_id")?),
        "resume" => li::resume_blocking(service, text("job_id")?),
        "import_reference" => li::import_reference_blocking(service, text("path")?),
        "install" => li::install_profile_blocking(service, text("profile_id")?),
        "install.pause" => li::pause_install_blocking(service, text("profile_id")?),
        "install.cancel" => li::cancel_install_blocking(service, text("profile_id")?),
        "unload" => li::unload_blocking(service),
        "set_idle_unload" => {
            let seconds = input
                .get("seconds")
                .and_then(Value::as_u64)
                .ok_or("invalid_request: seconds must be a whole number")?;
            encode(serde_json::to_value(li::set_idle_unload_blocking(
                service, app_data, seconds,
            )?))
        }
        "set_memory_mode" => encode(serde_json::to_value(li::set_memory_mode_blocking(
            service,
            app_data,
            &text("mode")?,
        )?)),
        other => Err(format!(
            "invalid_request: unknown local image command {other:?}"
        )),
    }
}

/// `unknown_job: x` becomes code `unknown_job`; text with no such prefix becomes `local_image_error`.
/// The message always stays the whole original text.
fn error_from_message(message: String) -> HostError {
    let code = message
        .split_once(':')
        .map(|(head, _)| head)
        .filter(|head| {
            !head.is_empty() && head.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
        })
        .unwrap_or("local_image_error")
        .to_string();
    HostError { code, message }
}

/// The folder of this executable, the first place `sd-server` is looked for (it ships beside the
/// engine, which ships beside the shell).
fn exe_dirs() -> Vec<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .into_iter()
        .collect()
}
