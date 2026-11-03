//! Hand-off of unresolved addressed-control utterances to the local Pulse CLI.
//!
//! Rules for callers:
//! - Clear the utterance only on [`Receipt::Accepted`].
//! - Never type or replay the transcript into a focused app, whatever the receipt.
//! - [`Receipt::Refused`] is not retried automatically.
//!
//! The transcript travels on the CLI's stdin only, never on its command line.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_child_process::OwnedCommand;
use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_TRANSCRIPT_BYTES: usize = 4096;
pub const MODE_ADDRESSED_CONTROL: &str = "addressed-control";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Utterance {
    pub schema_version: u32,
    pub utterance_id: String,
    pub generation: u64,
    pub session_id: String,
    /// Wake activation id. Optional: the app has no wake activation yet, so
    /// hand-offs come from push-to-talk control and carry no wake id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake_id: Option<String>,
    pub transcript: String,
    pub confidence: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Mac,
    Windows,
    Linux,
    Ios,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub schema_version: u32,
    pub device_id: String,
    pub platform: Platform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum R0Outcome {
    ExactCatalogMatch,
    NoExactMatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum R1Outcome {
    ResolvedLocal,
    UnresolvedAgentRequest,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct R0Check {
    pub outcome: R0Outcome,
    pub catalog_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct R1Check {
    pub outcome: R1Outcome,
    pub registry_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checks {
    pub schema_version: u32,
    pub r0: R0Check,
    pub r1: R1Check,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HandoffRecord {
    pub utterance: Utterance,
    pub device: Device,
    pub owner_id: String,
    pub mode: String,
    pub checks: Checks,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context_refs: Vec<String>,
}

impl HandoffRecord {
    pub fn validate(&self) -> Result<(), String> {
        let u = &self.utterance;
        let nonempty = |name: &str, v: &str| {
            if v.is_empty() {
                Err(format!("{name} must be non-empty"))
            } else {
                Ok(())
            }
        };
        for (v, what) in [
            (u.schema_version, "utterance.schema_version"),
            (self.device.schema_version, "device.schema_version"),
            (self.checks.schema_version, "checks.schema_version"),
        ] {
            if v != SCHEMA_VERSION {
                return Err(format!("{what} must be {SCHEMA_VERSION}"));
            }
        }
        nonempty("utterance_id", &u.utterance_id)?;
        nonempty("session_id", &u.session_id)?;
        if let Some(w) = &u.wake_id {
            nonempty("wake_id", w)?;
        }
        nonempty("transcript", &u.transcript)?;
        nonempty("device_id", &self.device.device_id)?;
        if let Some(n) = &self.device.display_name {
            nonempty("display_name", n)?;
        }
        nonempty("owner_id", &self.owner_id)?;
        nonempty("catalog_version", &self.checks.r0.catalog_version)?;
        nonempty("registry_version", &self.checks.r1.registry_version)?;
        if self.context_refs.iter().any(|r| r.is_empty()) {
            return Err("context_refs entries must be non-empty".into());
        }
        if u.transcript.len() > MAX_TRANSCRIPT_BYTES {
            return Err(format!("transcript exceeds {MAX_TRANSCRIPT_BYTES} bytes"));
        }
        if !(0.0..=1.0).contains(&u.confidence) {
            return Err("confidence must be within 0..=1".into());
        }
        if self.mode != MODE_ADDRESSED_CONTROL {
            return Err(format!("mode must be {MODE_ADDRESSED_CONTROL}"));
        }
        if self.checks.r0.outcome != R0Outcome::NoExactMatch {
            return Err("r0 must be no-exact-match".into());
        }
        if self.checks.r1.outcome != R1Outcome::UnresolvedAgentRequest {
            return Err("r1 must be unresolved-agent-request".into());
        }
        Ok(())
    }

    pub fn idempotency_key(&self) -> String {
        format!(
            "voice:{}:{}",
            self.utterance.utterance_id, self.utterance.generation
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receipt {
    Accepted {
        task_id: String,
        idempotency_key: String,
    },
    Refused {
        reason: String,
    },
    NotBuilt,
    Unknown,
}

/// Parse the CLI's stdout. Branches on the JSON `status` field, never the exit code.
pub fn parse_receipt(stdout: &str) -> Receipt {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return Receipt::Unknown;
    };
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_owned);
    match v.get("status").and_then(|s| s.as_str()) {
        Some("accepted") => match (text("task_id"), text("idempotency_key")) {
            (Some(task_id), Some(idempotency_key)) => Receipt::Accepted {
                task_id,
                idempotency_key,
            },
            _ => Receipt::Unknown,
        },
        Some("refused") => Receipt::Refused {
            reason: text("reason").unwrap_or_default(),
        },
        Some("not-built") => Receipt::NotBuilt,
        _ => Receipt::Unknown,
    }
}

pub fn default_cli_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        Some(PathBuf::from(
            "/Applications/Pulse.app/Contents/Helpers/pulse",
        ))
    }
    #[cfg(target_os = "windows")]
    {
        // The Program Files known folder, not `%ProgramFiles%`: the environment must not choose
        // which executable receives the handoff.
        let pf =
            crate::user_dirs::known_folder(&windows_sys::Win32::UI::Shell::FOLDERID_ProgramFiles)?;
        Some(pf.join("Pulse").join("Helpers").join("pulse.exe"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        None
    }
}

fn submit_once(json: &str, cli_path: &Path, timeout: Duration) -> Receipt {
    let mut cmd = OwnedCommand::new(cli_path);
    cmd.command_mut()
        .args(["task", "submit", "--stdin"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return Receipt::Unknown;
    };
    let Some(mut stdout) = child.take_stdout() else {
        let _ = child.terminate_tree();
        return Receipt::Unknown;
    };
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.by_ref().take(1 << 20).read_to_string(&mut buf);
        buf
    });
    let payload = json.as_bytes().to_vec();
    let writer = child.take_stdin().map(|mut stdin| {
        std::thread::spawn(move || {
            let _ = stdin.write_all(&payload);
        })
    });
    let exited = matches!(child.wait_timeout(timeout), Ok(Some(_)));
    if !exited {
        let _ = child.terminate_tree();
    }
    if let Some(w) = writer {
        let _ = w.join();
    }
    let out = reader.join().unwrap_or_default();
    if exited {
        parse_receipt(&out)
    } else {
        Receipt::Unknown
    }
}

/// Submit one record to the Pulse CLI. Resends once, same record and key, on Unknown.
pub fn submit(record: &HandoffRecord, cli_path: &Path, timeout: Duration) -> Receipt {
    if record.validate().is_err() {
        return Receipt::Unknown;
    }
    let mut value = match serde_json::to_value(record) {
        Ok(v) => v,
        Err(_) => return Receipt::Unknown,
    };
    if let Some(obj) = value.as_object_mut() {
        obj.insert(
            "idempotency_key".into(),
            serde_json::Value::String(record.idempotency_key()),
        );
    }
    let Ok(json) = serde_json::to_string(&value) else {
        return Receipt::Unknown;
    };
    let first = submit_once(&json, cli_path, timeout);
    if first == Receipt::Unknown {
        submit_once(&json, cli_path, timeout)
    } else {
        first
    }
}
