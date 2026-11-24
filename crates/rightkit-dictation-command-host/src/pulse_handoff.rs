//! Hand-off of unresolved addressed-control utterances to the local Pulse CLI.
//!
//! Rules for callers:
//! - Clear the utterance only on [`Receipt::Accepted`].
//! - Never type or replay the transcript into a focused app, whatever the receipt.
//! - [`Receipt::Refused`] is not retried automatically.
//! - [`Receipt::Unreachable`]: no engine answered and nothing ran. Keep the words; send again with the
//!   same key when Pulse is up or on the owner's action ([`submit`] never resends it by itself).
//! - [`Receipt::Unknown`]: sent, no answer. [`submit`] resends once with the same key; after that keep
//!   the words and wait for the owner.
//!
//! The transcript travels on the CLI's stdin only, never on its command line.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use rightkit_agent_task_contract::device::{DevicePlatform, DeviceRef, DEVICE_REF_SCHEMA_VERSION};
use rightkit_agent_task_contract::task::{UtteranceRef, UTTERANCE_REF_SCHEMA_VERSION};
use rightkit_agent_task_contract::voice as contract;
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

    /// The record the Pulse command line reads: the agent task contract's own `VoiceHandoff`, so what is sent
    /// is what the contract's types accept (it refuses unknown fields, and its transcript, session id, wake id
    /// and confidence sit at the top level, not inside `utterance`). Call [`HandoffRecord::validate`] first;
    /// the confidence is narrowed to the contract's `f32`.
    pub fn to_voice_handoff(&self) -> contract::VoiceHandoff {
        contract::VoiceHandoff {
            schema_version: contract::VOICE_HANDOFF_SCHEMA_VERSION,
            utterance: UtteranceRef {
                schema_version: UTTERANCE_REF_SCHEMA_VERSION,
                utterance_id: self.utterance.utterance_id.clone(),
                generation: self.utterance.generation,
            },
            session_id: self.utterance.session_id.clone(),
            wake_id: self.utterance.wake_id.clone(),
            transcript: self.utterance.transcript.clone(),
            confidence: self.utterance.confidence as f32,
            device: DeviceRef {
                schema_version: DEVICE_REF_SCHEMA_VERSION,
                device_id: self.device.device_id.clone(),
                platform: match self.device.platform {
                    Platform::Mac => DevicePlatform::Mac,
                    Platform::Windows => DevicePlatform::Windows,
                    Platform::Linux => DevicePlatform::Linux,
                    Platform::Ios => DevicePlatform::Ios,
                },
                display_name: self.device.display_name.clone(),
            },
            owner_id: self.owner_id.clone(),
            mode: contract::VoiceMode::AddressedControl,
            checks: contract::VoiceChecks {
                schema_version: contract::VOICE_CHECKS_SCHEMA_VERSION,
                r0: contract::R0Check {
                    outcome: contract::R0Outcome::NoExactMatch,
                    catalog_version: self.checks.r0.catalog_version.clone(),
                },
                r1: contract::R1Check {
                    outcome: contract::R1Outcome::UnresolvedAgentRequest,
                    registry_version: self.checks.r1.registry_version.clone(),
                },
            },
            context_refs: self.context_refs.clone(),
        }
    }

    /// The key the command line derives from the utterance (`VoiceHandoff::to_task_envelope`), so a resend
    /// after an unknown receipt names the same task.
    pub fn idempotency_key(&self) -> String {
        format!(
            "voice:{}:{}",
            self.utterance.utterance_id, self.utterance.generation
        )
    }
}

/// What the host knows about itself and the owner for every hand-off. The rest of a record comes from
/// the recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffIdentity {
    pub device_id: String,
    pub display_name: Option<String>,
    pub owner_id: String,
    /// Version of the exact command catalog the recording was checked against (R0).
    pub catalog_version: String,
    /// Version of the destination and action registry the recording was checked against (R1).
    pub registry_version: String,
}

/// The platform this build runs on, as a hand-off names it.
pub fn local_platform() -> Platform {
    #[cfg(target_os = "macos")]
    {
        Platform::Mac
    }
    #[cfg(target_os = "windows")]
    {
        Platform::Windows
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Platform::Linux
    }
}

impl HandoffIdentity {
    /// The record for one final addressed recording that no command matched: R0 found no exact match and
    /// R1 left it an agent request, which is the only thing a caller may send. `utterance_id` names the
    /// recording and the generation is 1, so the key (`voice:<utterance_id>:1`) is the same on every
    /// resend. The ASR gives no confidence, so it is recorded as 1.0, as the transcript event does.
    pub fn record_for(&self, utterance_id: &str, transcript: &str) -> HandoffRecord {
        HandoffRecord {
            utterance: Utterance {
                schema_version: SCHEMA_VERSION,
                utterance_id: utterance_id.to_owned(),
                generation: 1,
                session_id: utterance_id.to_owned(),
                wake_id: None,
                transcript: transcript.to_owned(),
                confidence: 1.0,
            },
            device: Device {
                schema_version: SCHEMA_VERSION,
                device_id: self.device_id.clone(),
                platform: local_platform(),
                display_name: self.display_name.clone(),
            },
            owner_id: self.owner_id.clone(),
            mode: MODE_ADDRESSED_CONTROL.to_owned(),
            checks: Checks {
                schema_version: SCHEMA_VERSION,
                r0: R0Check {
                    outcome: R0Outcome::NoExactMatch,
                    catalog_version: self.catalog_version.clone(),
                },
                r1: R1Check {
                    outcome: R1Outcome::UnresolvedAgentRequest,
                    registry_version: self.registry_version.clone(),
                },
            },
            context_refs: Vec::new(),
        }
    }
}

/// What the command line printed for a hand-off.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Receipt {
    /// The engine committed acceptance (`receipt: "accepted"`, exit 0). `task_status` is the task's status
    /// name (`accepted`, `leased`, ...), see [`pill_for_status`].
    Accepted {
        task_id: String,
        idempotency_key: String,
        task_status: String,
    },
    /// Validation or the engine refused it (`status: "refused"`, exit 2).
    Refused { reason: String },
    /// No engine answered, or the command line could not be started: nothing ran (`status: "unreachable"`,
    /// exit 3).
    Unreachable,
    /// Sent, and no answer came back (`status: "unknown"`, exit 3), or the output could not be read.
    Unknown,
}

impl Receipt {
    /// The line the pill shows for a receipt that keeps the words, or `None` for an accepted one (the pill
    /// then follows the task, see [`pill_for_status`]).
    pub fn pill_message(&self) -> Option<String> {
        match self {
            Receipt::Accepted { .. } => None,
            Receipt::Refused { reason } if reason.is_empty() => Some("Pulse refused it".to_owned()),
            Receipt::Refused { reason } => Some(reason.clone()),
            Receipt::Unreachable => Some("Pulse is not running".to_owned()),
            Receipt::Unknown => Some("Pulse did not answer".to_owned()),
        }
    }
}

/// Parse the CLI's stdout. The engine's accepted answer carries `receipt: "accepted"` and no `status`;
/// every failure carries `status`. Never the exit code alone.
pub fn parse_receipt(stdout: &str) -> Receipt {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
        return Receipt::Unknown;
    };
    let text = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_owned);
    if text("receipt").as_deref() == Some("accepted") {
        // An accepted answer missing its ids is not trusted: a resend with the same key finds the same task.
        return match (text("task_id"), text("idempotency_key")) {
            (Some(task_id), Some(idempotency_key)) => Receipt::Accepted {
                task_id,
                idempotency_key,
                task_status: text("task_status").unwrap_or_default(),
            },
            _ => Receipt::Unknown,
        };
    }
    match text("status").as_deref() {
        Some("refused") => Receipt::Refused {
            reason: text("reason").unwrap_or_default(),
        },
        Some("unreachable") => Receipt::Unreachable,
        _ => Receipt::Unknown,
    }
}

/// What the pill shows for a task, from its status name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskPill {
    /// `accepted`, `leased`, `running`.
    Working,
    /// `waiting-user`: an approval is waiting on the notch or phone. Nothing spoken approves it.
    WaitingApproval,
    /// `waiting-device`.
    WaitingDevice,
    /// `cancel-requested`.
    Cancelling,
    /// `succeeded`. The answer card on the notch is the source of truth; `first_words` is the start of the
    /// answer when it could be read.
    Succeeded { first_words: Option<String> },
    /// `failed`, with its reason when the engine gave one.
    Failed { reason: Option<String> },
    /// `cancelled`, with its reason when the engine gave one.
    Cancelled { reason: Option<String> },
    /// `uncertain`: the outcome is not known.
    Uncertain { reason: Option<String> },
    /// A status name this build does not know, shown as named.
    Other(String),
}

/// Map a task status name to its pill state (`docs/contracts/voice.md`, "Receipts and the pill").
/// `reason` is the engine's error text, used by the three states that show one.
pub fn pill_for_status(status: &str, reason: Option<String>) -> TaskPill {
    match status {
        "accepted" | "leased" | "running" => TaskPill::Working,
        "waiting-user" => TaskPill::WaitingApproval,
        "waiting-device" => TaskPill::WaitingDevice,
        "cancel-requested" => TaskPill::Cancelling,
        "succeeded" => TaskPill::Succeeded { first_words: None },
        "failed" => TaskPill::Failed { reason },
        "cancelled" => TaskPill::Cancelled { reason },
        "uncertain" => TaskPill::Uncertain { reason },
        other => TaskPill::Other(other.to_owned()),
    }
}

/// The installed command line, per user first and then the older machine-wide place; the first that
/// exists. Fixed places, never `PATH` or an environment variable: the environment must not choose which
/// executable receives the hand-off. Mac: `/Applications/Pulse.app/Contents/Helpers/pulse`. Windows:
/// `%LOCALAPPDATA%\Programs\Pulse\Helpers\pulse.exe` (the per-user install), then
/// `Program Files\Pulse\Helpers\pulse.exe`; both folders come from the known-folder API.
pub fn default_cli_path() -> Option<PathBuf> {
    cli_candidates().into_iter().find(|path| path.is_file())
}

fn cli_candidates() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        vec![PathBuf::from(
            "/Applications/Pulse.app/Contents/Helpers/pulse",
        )]
    }
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::Shell::{FOLDERID_LocalAppData, FOLDERID_ProgramFiles};

        let mut paths = Vec::new();
        if let Some(local) = crate::user_dirs::known_folder(&FOLDERID_LocalAppData) {
            paths.push(
                local
                    .join("Programs")
                    .join("Pulse")
                    .join("Helpers")
                    .join("pulse.exe"),
            );
        }
        if let Some(pf) = crate::user_dirs::known_folder(&FOLDERID_ProgramFiles) {
            paths.push(pf.join("Pulse").join("Helpers").join("pulse.exe"));
        }
        paths
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Vec::new()
    }
}

/// How long one command-line call may take before it counts as unanswered.
pub const CLI_TIMEOUT: Duration = Duration::from_secs(15);

enum CliRun {
    /// The command line could not be started: nothing ran.
    NotStarted,
    /// It started and did not finish in time, or its output could not be taken.
    Silent,
    Output(String),
}

fn run_cli(args: &[&str], stdin: Option<&str>, cli_path: &Path, timeout: Duration) -> CliRun {
    let mut cmd = OwnedCommand::new(cli_path);
    cmd.command_mut()
        .args(args)
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let Ok(mut child) = cmd.spawn() else {
        return CliRun::NotStarted;
    };
    let Some(mut stdout) = child.take_stdout() else {
        let _ = child.terminate_tree();
        return CliRun::Silent;
    };
    let reader = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = stdout.by_ref().take(1 << 20).read_to_string(&mut buf);
        buf
    });
    let writer = stdin.and_then(|text| {
        let payload = text.as_bytes().to_vec();
        child.take_stdin().map(|mut pipe| {
            std::thread::spawn(move || {
                let _ = pipe.write_all(&payload);
            })
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
        CliRun::Output(out)
    } else {
        CliRun::Silent
    }
}

fn submit_once(json: &str, cli_path: &Path, timeout: Duration) -> Receipt {
    match run_cli(
        &["task", "submit", "--stdin"],
        Some(json),
        cli_path,
        timeout,
    ) {
        CliRun::NotStarted => Receipt::Unreachable,
        CliRun::Silent => Receipt::Unknown,
        CliRun::Output(out) => parse_receipt(&out),
    }
}

/// Submit one record to the Pulse CLI. Resends once, same record and key, on `Unknown` only: an
/// `Unreachable` receipt means nothing ran, and its retry is the caller's, on the owner's action or when
/// Pulse is up.
pub fn submit(record: &HandoffRecord, cli_path: &Path, timeout: Duration) -> Receipt {
    if let Err(reason) = record.validate() {
        return Receipt::Refused { reason };
    }
    let Ok(json) = serde_json::to_string(&record.to_voice_handoff()) else {
        return Receipt::Refused {
            reason: "the hand-off could not be written".to_owned(),
        };
    };
    let first = submit_once(&json, cli_path, timeout);
    if first == Receipt::Unknown {
        submit_once(&json, cli_path, timeout)
    } else {
        first
    }
}

/// [`submit`] to the installed command line ([`default_cli_path`]) within [`CLI_TIMEOUT`] per call. With no
/// Pulse installed nothing runs, so the receipt is `Unreachable`.
pub fn submit_installed(record: &HandoffRecord) -> Receipt {
    match default_cli_path() {
        Some(path) => submit(record, &path, CLI_TIMEOUT),
        None => Receipt::Unreachable,
    }
}

/// The task's pill state now, from `pulse task status <task-id>` (and, for a task that succeeded,
/// `pulse task result <task-id>` for the first words of the answer). `None` when the command line did not
/// answer with a task status: the pill then keeps its last state.
pub fn task_pill(task_id: &str, cli_path: &Path, timeout: Duration) -> Option<TaskPill> {
    let CliRun::Output(out) = run_cli(&["task", "status", task_id], None, cli_path, timeout) else {
        return None;
    };
    let v = serde_json::from_str::<serde_json::Value>(out.trim()).ok()?;
    // A failure receipt also has `status`; only a task status carries `terminal`.
    v.get("terminal")?.as_bool()?;
    let status = v.get("status")?.as_str()?;
    let reason = v.get("error").and_then(|e| e.as_str()).map(str::to_owned);
    match pill_for_status(status, reason) {
        TaskPill::Succeeded { .. } => Some(TaskPill::Succeeded {
            first_words: answer_first_words(task_id, cli_path, timeout),
        }),
        other => Some(other),
    }
}

/// How many words of the answer the pill shows.
const PILL_ANSWER_WORDS: usize = 12;

fn answer_first_words(task_id: &str, cli_path: &Path, timeout: Duration) -> Option<String> {
    let CliRun::Output(out) = run_cli(&["task", "result", task_id], None, cli_path, timeout) else {
        return None;
    };
    let v = serde_json::from_str::<serde_json::Value>(out.trim()).ok()?;
    let text = v.get("result")?.get("text")?.as_str()?;
    let words: Vec<&str> = text.split_whitespace().take(PILL_ANSWER_WORDS).collect();
    (!words.is_empty()).then(|| words.join(" "))
}
