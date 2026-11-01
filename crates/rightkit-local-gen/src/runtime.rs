// Local image generation: see docs/plans/local-image-generation.md
// Owned sd-server child supervisor + JSONL lifecycle log.

use std::fs;
#[cfg(target_os = "macos")]
use std::fs::OpenOptions;
#[cfg(target_os = "macos")]
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use rightkit_child_process::{
    sweep, AdoptedIdentity, OwnedChild, OwnedCommand, ReapOutcome, StderrTail,
};
use serde_json::Value;

use crate::machine::MachineLock;
use crate::provider::{SdCppClient, ValidatedCapabilities};

pub const SESSION_TOKEN_ENV: &str = "SDCPP_SESSION_TOKEN";

/// Most bytes of each backend stream kept for the "recent backend_log" error text.
const TAIL_MAX_BYTES: usize = 16 * 1024;

pub fn new_session_token() -> Result<String, String> {
    rightkit_daemon::PrivateSecret::generate()
        .map(|secret| secret.expose().to_string())
        .map_err(|error| format!("cannot create the session token: {error}"))
}

pub fn new_session_id() -> String {
    format!("rt_{:016x}", rand::random::<u64>())
}

pub fn pick_loopback_port() -> Result<u16, String> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")
        .map_err(|error| format!("bind 127.0.0.1:0: {error}"))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|error| format!("local_addr: {error}"))
}

/// Local-image JSONL lifecycle log: `<log_dir>/local-image.jsonl`, rotated at 16 MiB.
pub const LOCAL_IMAGE_LOG_FILE: &str = "local-image.jsonl";
pub const LOCAL_IMAGE_LOG_MAX_BYTES: u64 = 16 * 1024 * 1024;

/// Event timestamp in Unix milliseconds for `rightkit_logs::EventRecord`.
pub fn log_now_ms() -> i64 {
    (time::OffsetDateTime::now_utc().unix_timestamp_nanos() / 1_000_000) as i64
}

pub(crate) fn rfc3339_now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[derive(Debug)]
pub struct SdServerConfig {
    pub binary: PathBuf,
    pub args: Vec<String>,
    pub log_dir: PathBuf,
    /// Memory mode record (`requested`, `mode`, `flags`, estimates) logged with `runtime_start`;
    /// `Null` for a config that only identifies a recorded server.
    pub memory: Value,
}

pub struct SdServer {
    // RightKit owned child: unix process-group leader / Windows Job Object,
    // kill-tree on drop. Replaces the hand-rolled WindowsJob + group kill.
    child: OwnedChild,
    port: u16,
    token: String,
    session_id: String,
    pid: u32,
    binary_sha256: String,
    lock_path: PathBuf,
    /// Machine-wide single-server lock (see `machine.rs`); released when the server is stopped or dropped.
    machine: Option<MachineLock>,
    stdout_tail: StderrTail,
    stderr_tail: StderrTail,
}

impl std::fmt::Debug for SdServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SdServer")
            .field("port", &self.port)
            .field("session_id", &self.session_id)
            .field("pid", &self.pid)
            .field("binary_sha256", &self.binary_sha256)
            .field("token", &"<redacted>")
            .finish_non_exhaustive()
    }
}

impl SdServer {
    pub fn start(
        cfg: &SdServerConfig,
        port: u16,
        token: &str,
        session_id: &str,
        machine: Option<MachineLock>,
    ) -> Result<Self, String> {
        let lock_path = claim_instance_lock(cfg)?;
        let binary_sha256 = sha256_file(&cfg.binary)?;
        let mut command = Command::new(&cfg.binary);
        command.args(&cfg.args);
        command
            .arg("--listen-ip")
            .arg("127.0.0.1")
            .arg("--listen-port")
            .arg(port.to_string())
            .env(SESSION_TOKEN_ENV, token)
            .env("SDCPP_SESSION_ID", session_id)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut owned = OwnedCommand::from_command(command);
        owned.windows_hide(); // CREATE_NO_WINDOW on Windows
                              // The server dies with this process even when it is SIGKILLed (rightkit-child-process 0.4.2+):
                              // - macOS: a kqueue NOTE_EXIT helper signals the server's process group (TERM, then KILL
                              //   after 2 s) when this process exits however it exits.
                              // - Linux: PR_SET_PDEATHSIG (TERM to the direct child only; fires when the spawning thread
                              //   exits, so `start` must run on a thread that lives as long as the server).
                              // - Windows: no-op; the Job Object kills on close.
                              // Other Unix hosts return `Unsupported`, which fails the start (not a supported target).
        owned.kill_on_parent_exit(true);
        let child = owned
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", cfg.binary.display()))?;
        Self::attach(
            cfg,
            port,
            token,
            session_id,
            child,
            binary_sha256,
            Some(lock_path),
            machine,
        )
    }

    /// Adopts an already-spawned owned supervisor/backend child: attaches the
    /// stdout/stderr log drains, and records `runtime_start`. The OwnedChild
    /// is its process-group leader (unix) / Job Object member (Windows), so
    /// `stop`/Drop cleanup behaves identically to `start`.
    #[allow(
        clippy::too_many_arguments,
        reason = "private adopt step: the child, its identity, lock and tail inputs arrive together from the one spawn site"
    )]
    fn attach(
        cfg: &SdServerConfig,
        port: u16,
        token: &str,
        session_id: &str,
        mut child: OwnedChild,
        binary_sha256: String,
        lock_path: Option<PathBuf>,
        machine: Option<MachineLock>,
    ) -> Result<Self, String> {
        let pid = child.id();
        let stdout = child
            .take_stdout()
            .ok_or_else(|| "sd-server stdout missing".to_string())?;
        let stderr = child
            .take_stderr()
            .ok_or_else(|| "sd-server stderr missing".to_string())?;

        let stdout_tail = backend_tail(stdout, "stdout", &cfg.log_dir, session_id, pid, token);
        let stderr_tail = backend_tail(stderr, "stderr", &cfg.log_dir, session_id, pid, token);

        rightkit_logs::JsonlStore::new(
            cfg.log_dir.join(LOCAL_IMAGE_LOG_FILE),
            LOCAL_IMAGE_LOG_MAX_BYTES,
        )
        .append_best_effort(
            &rightkit_logs::EventRecord::new(
                log_now_ms(),
                rightkit_logs::Level::Info,
                "runtime_start",
            )
            .with_field("session", serde_json::json!(session_id))
            .with_field("pid", serde_json::json!(pid))
            .with_field("port", serde_json::json!(port))
            .with_field("binary", serde_json::json!(cfg.binary.to_string_lossy()))
            .with_field("binary_sha256", serde_json::json!(binary_sha256))
            .with_field("memory", serde_json::json!(cfg.memory.clone()))
            .with_field("args", serde_json::json!(cfg.args.clone())),
        );
        let lock_path = lock_path.unwrap_or_else(|| cfg.log_dir.join(INSTANCE_LOCK_FILE));
        write_instance_lock(&lock_path, cfg, pid, session_id, &binary_sha256, port);
        if let Some(machine) = machine.as_ref() {
            // Record the executable the OS reports for this pid, not the path we asked to run:
            // takeover compares against it.
            let observed = executable_of(pid).unwrap_or_else(|| cfg.binary.clone());
            machine.set_server(pid, Some(port), &observed);
        }

        Ok(Self {
            child,
            port,
            token: token.to_string(),
            session_id: session_id.to_string(),
            pid,
            binary_sha256,
            lock_path,
            machine,
            stdout_tail,
            stderr_tail,
        })
    }

    pub fn wait_ready(
        &mut self,
        client: &SdCppClient,
        timeout: Duration,
        log_dir: &Path,
        mut cancelled: impl FnMut() -> bool,
    ) -> Result<ValidatedCapabilities, String> {
        let start = Instant::now();
        loop {
            if cancelled() {
                return Err("sd-server readiness cancelled".to_string());
            }
            if let Some(status) = self
                .child
                .try_wait()
                .map_err(|error| format!("try_wait: {error}"))?
            {
                rightkit_logs::JsonlStore::new(
                    (log_dir).join(LOCAL_IMAGE_LOG_FILE),
                    LOCAL_IMAGE_LOG_MAX_BYTES,
                )
                .append_best_effort(
                    &rightkit_logs::EventRecord::new(
                        log_now_ms(),
                        rightkit_logs::Level::Info,
                        "runtime_exit",
                    )
                    .with_field("session", serde_json::json!(self.session_id))
                    .with_field("pid", serde_json::json!(self.pid))
                    .with_field("exit_code", serde_json::json!(status.code()))
                    .with_field("reason", serde_json::json!("early_exit")),
                );
                let tail = self.tail_lines(10).join("\n");
                return Err(format!(
                    "sd-server exited before ready (status {status}); recent backend_log:\n{tail}"
                ));
            }
            match client.verify_session(&self.session_id, &mut cancelled) {
                Ok(validated) => {
                    rightkit_logs::JsonlStore::new(
                        (log_dir).join(LOCAL_IMAGE_LOG_FILE),
                        LOCAL_IMAGE_LOG_MAX_BYTES,
                    )
                    .append_best_effort(
                        &rightkit_logs::EventRecord::new(
                            log_now_ms(),
                            rightkit_logs::Level::Info,
                            "runtime_ready",
                        )
                        .with_field("session", serde_json::json!(self.session_id))
                        .with_field("pid", serde_json::json!(self.pid))
                        .with_field("port", serde_json::json!(self.port))
                        .with_field("session_id", serde_json::json!(validated.session_id))
                        .with_field("binary_sha256", serde_json::json!(self.binary_sha256))
                        .with_field(
                            "elapsed_ms",
                            serde_json::json!(start.elapsed().as_millis() as u64),
                        ),
                    );
                    return Ok(validated);
                }
                Err(error) => {
                    if error.code != "transport" {
                        return Err(format!("sd-server session verification failed: {error}"));
                    }
                    if start.elapsed() >= timeout {
                        rightkit_logs::JsonlStore::new(
                            (log_dir).join(LOCAL_IMAGE_LOG_FILE),
                            LOCAL_IMAGE_LOG_MAX_BYTES,
                        )
                        .append_best_effort(
                            &rightkit_logs::EventRecord::new(
                                log_now_ms(),
                                rightkit_logs::Level::Info,
                                "runtime_exit",
                            )
                            .with_field("session", serde_json::json!(self.session_id))
                            .with_field("pid", serde_json::json!(self.pid))
                            .with_field("reason", serde_json::json!("ready_timeout")),
                        );
                        return Err(format!(
                            "sd-server not ready within {}ms: {error}",
                            timeout.as_millis()
                        ));
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    pub fn stop(&mut self, log_dir: &Path) -> Result<(), String> {
        if self
            .child
            .try_wait()
            .map_err(|error| format!("check sd-server status: {error}"))?
            .is_none()
        {
            // Kill the owned tree together. A graceful supervisor exit could
            // otherwise precede a still-loading child & defeat escalation.
            let outcome = self
                .child
                .terminate_tree_bounded(Duration::from_secs(5))
                .map_err(|error| format!("stop sd-server process {}: {error}", self.pid))?;
            if matches!(outcome, ReapOutcome::TimedOut) {
                return Err(format!(
                    "stop sd-server process {}: reap timed out",
                    self.pid
                ));
            }
            rightkit_logs::JsonlStore::new(
                (log_dir).join(LOCAL_IMAGE_LOG_FILE),
                LOCAL_IMAGE_LOG_MAX_BYTES,
            )
            .append_best_effort(
                &rightkit_logs::EventRecord::new(
                    log_now_ms(),
                    rightkit_logs::Level::Info,
                    "runtime_exit",
                )
                .with_field("session", serde_json::json!(self.session_id))
                .with_field("pid", serde_json::json!(self.pid))
                .with_field("reason", serde_json::json!("terminated")),
            );
        }
        self.finish_tails(Duration::from_millis(500));
        let _ = fs::remove_file(&self.lock_path);
        // The server is gone: free the machine-wide lock for the next job.
        self.machine = None;
        Ok(())
    }

    /// Waits at most `timeout` for each stream's drain to end (a grandchild that inherited a
    /// pipe can keep it open); the retained tail is complete either way.
    fn finish_tails(&mut self, timeout: Duration) {
        self.stdout_tail.finish_within(timeout);
        self.stderr_tail.finish_within(timeout);
    }

    /// The machine-wide lock this server runs under (state, waiters, yield rule).
    pub fn machine(&self) -> Option<&MachineLock> {
        self.machine.as_ref()
    }

    pub fn alive(&mut self) -> bool {
        self.child.try_wait().ok().flatten().is_none()
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    pub fn token(&self) -> &str {
        &self.token
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn pid(&self) -> u32 {
        self.pid
    }

    pub fn binary_sha256(&self) -> &str {
        &self.binary_sha256
    }

    /// The last `n` retained backend lines: stdout lines, then stderr lines, each prefixed with
    /// its stream.
    fn tail_lines(&self, n: usize) -> Vec<String> {
        let mut lines: Vec<String> = Vec::new();
        for (name, tail) in [("stdout", &self.stdout_tail), ("stderr", &self.stderr_tail)] {
            let snapshot = tail.snapshot();
            lines.extend(snapshot.lines().map(|line| format!("[{name}] {line}")));
        }
        let skip = lines.len().saturating_sub(n);
        lines.split_off(skip)
    }
}

impl Drop for SdServer {
    fn drop(&mut self) {
        // OwnedChild::drop terminate_trees the process group (unix) / Job
        // Object (Windows); here we only settle drains and the instance lock.
        let _ = self.child.terminate_tree();
        self.finish_tails(Duration::from_millis(250));
        let _ = fs::remove_file(&self.lock_path);
    }
}

fn sha256_file(path: &Path) -> Result<String, String> {
    rightkit_fs::digest::sha256_file(path)
        .map_err(|error| format!("hash backend binary {}: {error}", path.display()))
}

/// Hash executable code independently of native signature bytes while keeping
/// every load command and file-backed segment byte bound to the manifest.
/// Actual installed-file SHA-256 remains available separately for provenance.
#[cfg(target_os = "macos")]
pub fn approval_sha256(path: &Path) -> Result<String, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("stat backend binary {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() {
        return Err("backend integrity: expected regular Mach-O file".to_string());
    }
    let before = parse_macho(path)?;
    let temp = MachONormalizationCopy::new(path)?;
    if before.code_signature.is_some() {
        run_codesign_remove_signature(&temp.path)?;
    }
    let after = parse_macho(&temp.path)?;
    if after.code_signature.is_some() {
        return Err("backend integrity: signature remains after normalization".to_string());
    }
    if before.code_signature.is_some() {
        let before_signature = before.code_signature.unwrap();
        if before.ncmds != after.ncmds + 1 || before.sizeofcmds != after.sizeofcmds + 16 {
            return Err(
                "backend integrity: signature removal changed Mach-O command layout".to_string(),
            );
        }
        if before.normalized_commands != after.normalized_commands {
            return Err(
                "backend integrity: signature removal changed non-signature load commands"
                    .to_string(),
            );
        }
        let after_linkedit_end = after.linkedit.fileoff + after.linkedit.filesize;
        let removed_padding = before_signature.offset.saturating_sub(after_linkedit_end);
        if before.linkedit.fileoff + before.linkedit.filesize
            != before_signature.offset + before_signature.size
            || after_linkedit_end > before_signature.offset
            || removed_padding > 15
            || after.file_len != after_linkedit_end
            || before_signature.offset + before_signature.size != before.file_len
            || before_signature.offset < before.linkedit.fileoff
        {
            return Err(
                "backend integrity: code signature is outside trailing __LINKEDIT data".to_string(),
            );
        }
        if removed_padding > 0 && !range_is_zero(path, after_linkedit_end, removed_padding)? {
            return Err(
                "backend integrity: nonzero alignment padding before code signature".to_string(),
            );
        }
    } else if before.normalized_commands != after.normalized_commands {
        return Err(
            "backend integrity: unsigned normalization changed Mach-O load commands".to_string(),
        );
    }
    // macOS ARM64 uses 16 KiB VM pages; x86_64 uses 4 KiB pages.
    let page_size: u64 = if read_macho_cpu_type(&temp.path)? == 0x0100_000c {
        16 * 1024
    } else {
        4096
    };
    let rounded_size = after
        .linkedit
        .filesize
        .checked_add(page_size - 1)
        .ok_or_else(|| "backend integrity: __LINKEDIT size overflow".to_string())?
        / page_size
        * page_size;
    if after.linkedit.vmsize < rounded_size {
        return Err("backend integrity: invalid __LINKEDIT virtual size".to_string());
    }
    let mut file = OpenOptions::new()
        .write(true)
        .open(&temp.path)
        .map_err(|error| format!("open normalized backend: {error}"))?;
    file.seek(SeekFrom::Start(after.linkedit.vmsize_offset))
        .map_err(|error| format!("seek normalized backend: {error}"))?;
    file.write_all(&rounded_size.to_le_bytes())
        .map_err(|error| format!("canonicalize __LINKEDIT size: {error}"))?;
    file.flush()
        .map_err(|error| format!("flush normalized backend: {error}"))?;
    sha256_file(&temp.path)
}

#[cfg(target_os = "macos")]
fn range_is_zero(path: &Path, offset: u64, length: u64) -> Result<bool, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("open Mach-O alignment padding: {error}"))?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|error| format!("seek Mach-O alignment padding: {error}"))?;
    let mut bytes = vec![0u8; length as usize];
    file.read_exact(&mut bytes)
        .map_err(|error| format!("read Mach-O alignment padding: {error}"))?;
    Ok(bytes.iter().all(|byte| *byte == 0))
}

#[cfg(target_os = "macos")]
fn read_macho_cpu_type(path: &Path) -> Result<u32, String> {
    let mut file = fs::File::open(path).map_err(|error| format!("open Mach-O header: {error}"))?;
    file.seek(SeekFrom::Start(4))
        .map_err(|error| format!("seek Mach-O CPU type: {error}"))?;
    read_u32(&mut file, true)
}

#[cfg(not(target_os = "macos"))]
pub fn approval_sha256(path: &Path) -> Result<String, String> {
    sha256_file(path)
}

#[cfg(target_os = "macos")]
#[derive(Clone, Copy)]
struct CodeSignatureRange {
    offset: u64,
    size: u64,
}

#[cfg(target_os = "macos")]
struct LinkeditSegment {
    fileoff: u64,
    filesize: u64,
    vmsize: u64,
    vmsize_offset: u64,
}

#[cfg(target_os = "macos")]
struct MachOLayout {
    ncmds: u32,
    sizeofcmds: u32,
    file_len: u64,
    code_signature: Option<CodeSignatureRange>,
    linkedit: LinkeditSegment,
    normalized_commands: Vec<Vec<u8>>,
}

#[cfg(target_os = "macos")]
fn parse_macho(path: &Path) -> Result<MachOLayout, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("open Mach-O {}: {error}", path.display()))?;
    let file_len = file
        .metadata()
        .map_err(|error| format!("stat Mach-O: {error}"))?
        .len();
    let mut magic_bytes = [0u8; 4];
    file.read_exact(&mut magic_bytes)
        .map_err(|error| format!("read Mach-O magic: {error}"))?;
    let (little_endian, is_64) = match magic_bytes {
        [0xce, 0xfa, 0xed, 0xfe] => (true, false),
        [0xcf, 0xfa, 0xed, 0xfe] => (true, true),
        [0xfe, 0xed, 0xfa, 0xce] => (false, false),
        [0xfe, 0xed, 0xfa, 0xcf] => (false, true),
        _ => return Err("backend integrity: unsupported or non-thin Mach-O image".to_string()),
    };
    if !little_endian || !is_64 {
        return Err("backend integrity: only little-endian 64-bit Mach-O is supported".to_string());
    }
    file.seek(SeekFrom::Start(4))
        .map_err(|error| format!("seek Mach-O CPU type: {error}"))?;
    let cpu_type = read_u32(&mut file, true)?;
    if cpu_type != 0x0100_0007 && cpu_type != 0x0100_000c {
        return Err("backend integrity: unsupported Mach-O CPU type".to_string());
    }
    let header_size = if is_64 { 32u64 } else { 28u64 };
    if file_len < header_size {
        return Err("backend integrity: truncated Mach-O header".to_string());
    }
    file.seek(SeekFrom::Start(16))
        .map_err(|error| format!("seek Mach-O header: {error}"))?;
    let ncmds = read_u32(&mut file, little_endian)?;
    let sizeofcmds = read_u32(&mut file, little_endian)?;
    let command_end = header_size
        .checked_add(sizeofcmds as u64)
        .ok_or_else(|| "backend integrity: load command size overflow".to_string())?;
    if ncmds > 65_536 || command_end > file_len || sizeofcmds as u64 > file_len / 2 {
        return Err("backend integrity: invalid Mach-O load command table".to_string());
    }
    let mut cursor = header_size;
    let mut code_signature = None;
    let mut linkedit = None;
    let mut normalized_commands = Vec::with_capacity(ncmds as usize);
    for _ in 0..ncmds {
        if cursor + 8 > command_end {
            return Err("backend integrity: truncated Mach-O load command".to_string());
        }
        file.seek(SeekFrom::Start(cursor))
            .map_err(|error| format!("seek Mach-O command: {error}"))?;
        let command = read_u32(&mut file, little_endian)?;
        let command_size = read_u32(&mut file, little_endian)?;
        if command_size < 8 || command_size % 4 != 0 || cursor + command_size as u64 > command_end {
            return Err("backend integrity: invalid Mach-O load command size".to_string());
        }
        let mut command_bytes = vec![0u8; command_size as usize];
        file.seek(SeekFrom::Start(cursor))
            .map_err(|error| format!("seek Mach-O command bytes: {error}"))?;
        file.read_exact(&mut command_bytes)
            .map_err(|error| format!("read Mach-O command: {error}"))?;
        if command == 0x1d {
            if command_size != 16 || code_signature.is_some() {
                return Err(
                    "backend integrity: invalid duplicate code signature command".to_string(),
                );
            }
            let offset = read_u32_from(&command_bytes[8..12], little_endian) as u64;
            let size = read_u32_from(&command_bytes[12..16], little_endian) as u64;
            if size == 0 || offset.checked_add(size).is_none_or(|end| end > file_len) {
                return Err("backend integrity: invalid code signature extent".to_string());
            }
            code_signature = Some(CodeSignatureRange { offset, size });
        } else if (is_64 && command == 0x19) || (!is_64 && command == 1) {
            let (
                min_size,
                segment_name_range,
                vmsize_field,
                fileoff_field,
                filesize_field,
                nsects_field,
                section_size,
            ) = if is_64 {
                (72usize, 8..24, 32usize, 40usize, 48usize, 64usize, 80usize)
            } else {
                (56usize, 8..16, 20usize, 24usize, 28usize, 40usize, 68usize)
            };
            if (command_size as usize) < min_size {
                return Err("backend integrity: truncated Mach-O segment command".to_string());
            }
            let nsects = read_u32_from(
                &command_bytes[nsects_field..nsects_field + 4],
                little_endian,
            ) as usize;
            if min_size
                .checked_add(
                    nsects
                        .checked_mul(section_size)
                        .ok_or_else(|| "backend integrity: section count overflow".to_string())?,
                )
                .is_none_or(|required| required > command_size as usize)
            {
                return Err("backend integrity: invalid Mach-O segment sections".to_string());
            }
            let segment_name = command_bytes[segment_name_range]
                .split(|byte| *byte == 0)
                .next()
                .unwrap_or_default();
            if segment_name == b"__LINKEDIT" {
                if linkedit.is_some() {
                    return Err("backend integrity: duplicate __LINKEDIT segment".to_string());
                }
                let (vmsize, fileoff, filesize) = if is_64 {
                    (
                        read_u64_from(
                            &command_bytes[vmsize_field..vmsize_field + 8],
                            little_endian,
                        ),
                        read_u64_from(
                            &command_bytes[fileoff_field..fileoff_field + 8],
                            little_endian,
                        ),
                        read_u64_from(
                            &command_bytes[filesize_field..filesize_field + 8],
                            little_endian,
                        ),
                    )
                } else {
                    (
                        read_u32_from(
                            &command_bytes[vmsize_field..vmsize_field + 4],
                            little_endian,
                        ) as u64,
                        read_u32_from(
                            &command_bytes[fileoff_field..fileoff_field + 4],
                            little_endian,
                        ) as u64,
                        read_u32_from(
                            &command_bytes[filesize_field..filesize_field + 4],
                            little_endian,
                        ) as u64,
                    )
                };
                if fileoff
                    .checked_add(filesize)
                    .is_none_or(|end| end > file_len)
                    || vmsize < filesize
                {
                    return Err("backend integrity: invalid __LINKEDIT extent".to_string());
                }
                command_bytes[vmsize_field..vmsize_field + if is_64 { 8 } else { 4 }].fill(0);
                command_bytes[filesize_field..filesize_field + if is_64 { 8 } else { 4 }].fill(0);
                linkedit = Some(LinkeditSegment {
                    fileoff,
                    filesize,
                    vmsize,
                    vmsize_offset: cursor + vmsize_field as u64,
                });
            }
        }
        if command != 0x1d {
            normalized_commands.push(command_bytes);
        }
        cursor += command_size as u64;
    }
    if cursor != command_end {
        return Err("backend integrity: load commands do not fill declared table".to_string());
    }
    let linkedit =
        linkedit.ok_or_else(|| "backend integrity: missing __LINKEDIT segment".to_string())?;
    if let Some(signature) = code_signature {
        if signature.offset < linkedit.fileoff
            || signature.offset + signature.size != file_len
            || linkedit.fileoff + linkedit.filesize != file_len
        {
            return Err("backend integrity: signature is not terminal __LINKEDIT data".to_string());
        }
    }
    Ok(MachOLayout {
        ncmds,
        sizeofcmds,
        file_len,
        code_signature,
        linkedit,
        normalized_commands,
    })
}

#[cfg(target_os = "macos")]
fn read_u32(reader: &mut impl Read, little_endian: bool) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    reader
        .read_exact(&mut bytes)
        .map_err(|error| error.to_string())?;
    Ok(read_u32_from(&bytes, little_endian))
}

#[cfg(target_os = "macos")]
fn read_u32_from(bytes: &[u8], little_endian: bool) -> u32 {
    let bytes: [u8; 4] = bytes.try_into().unwrap();
    if little_endian {
        u32::from_le_bytes(bytes)
    } else {
        u32::from_be_bytes(bytes)
    }
}

#[cfg(target_os = "macos")]
fn read_u64_from(bytes: &[u8], little_endian: bool) -> u64 {
    let bytes: [u8; 8] = bytes.try_into().unwrap();
    if little_endian {
        u64::from_le_bytes(bytes)
    } else {
        u64::from_be_bytes(bytes)
    }
}

#[cfg(target_os = "macos")]
struct MachONormalizationCopy {
    directory: PathBuf,
    path: PathBuf,
}

#[cfg(target_os = "macos")]
impl MachONormalizationCopy {
    fn new(source: &Path) -> Result<Self, String> {
        let directory = std::env::temp_dir().join(format!(
            "rightkit-macho-normalize-{}-{:016x}",
            std::process::id(),
            rand::random::<u64>()
        ));
        rightkit_fs::ensure_private_dir(&directory)
            .map_err(|error| format!("create normalization directory: {error}"))?;
        let path = directory.join("sd-server");
        let options = rightkit_fs::AtomicWriteOptions {
            private: true,
            ..Default::default()
        };
        if let Err(error) = rightkit_fs::copy_verified(source, &path, options, None) {
            let _ = fs::remove_dir_all(&directory);
            return Err(format!("copy backend for normalization: {error}"));
        }
        Ok(Self { directory, path })
    }
}

#[cfg(target_os = "macos")]
impl Drop for MachONormalizationCopy {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

#[cfg(target_os = "macos")]
fn run_codesign_remove_signature(path: &Path) -> Result<(), String> {
    let options = rightkit_child_process::RunOptions::new(Duration::from_secs(5));
    match rightkit_child_process::run(
        "/usr/bin/codesign",
        [std::ffi::OsStr::new("--remove-signature"), path.as_os_str()],
        &options,
    ) {
        Ok(out) if out.timed_out => Err("codesign signature normalization timed out".to_string()),
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(format!(
            "codesign signature normalization failed ({})",
            out.status
        )),
        Err(error) => Err(format!("start signature normalization: {error}")),
    }
}

// ---------- single-instance lock + orphan reaping ---------------------------

/// Filename under the local-image data root (log dir's parent would also work;
/// we keep it beside the JSONL log for simplicity).
const INSTANCE_LOCK_FILE: &str = "sd-server.lock";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstanceLock {
    /// Kill target: supervisor (group leader) on unix, backend pid on Windows.
    pub pid: u32,
    /// App process that started the backend; used to distinguish orphans.
    pub owner_pid: u32,
    #[serde(default)]
    pub owner_binary: Option<String>,
    /// Backend executable path recorded at spawn.
    pub binary: String,
    pub binary_sha256: String,
    pub session_id: String,
    pub started_at: String,
    #[serde(default)]
    pub port: Option<u16>,
    /// Process start times (machine `fmt_ms`) of `pid` and `owner_pid`, read from the OS when the
    /// lock is written. pid + start time is the process identity (rightkit-child-process
    /// `AdoptedIdentity`); a record without them cannot be proven and is never killed.
    #[serde(default)]
    pub pid_started: Option<String>,
    #[serde(default)]
    pub owner_started: Option<String>,
}

/// The recorded pid is still the process the lock names: alive with the recorded start time.
fn lock_pid_is(pid: u32, started: Option<&str>) -> bool {
    started.is_some() && crate::machine::same_process_alive(pid, started)
}

#[derive(Debug, PartialEq)]
pub enum LockCheck {
    /// No usable lock, dead pid, or a pid reused by an unrelated executable.
    Free,
    /// Recorded server is alive AND its owning app is alive — refuse a second.
    Busy { pid: u32, owner_pid: u32 },
    /// Recorded server is alive but its owner died — safe to reap.
    Orphan { pid: u32 },
}

/// Public lifecycle metadata for passive desktop/engine clients; no session credential.
pub fn active_instance(log_dir: &Path) -> Option<InstanceLock> {
    let lock: InstanceLock =
        serde_json::from_slice(&fs::read(log_dir.join(INSTANCE_LOCK_FILE)).ok()?).ok()?;
    let live = lock.owner_pid != 0
        && lock_pid_is(lock.owner_pid, lock.owner_started.as_deref())
        && lock_pid_is(lock.pid, lock.pid_started.as_deref());
    live.then_some(lock)
}

/// Full executable path of a live pid (None if dead/unreadable), informational only
/// (rightkit-child-process `AdoptedIdentity::exe`).
pub(crate) fn executable_of(pid: u32) -> Option<PathBuf> {
    let identity = AdoptedIdentity::capture(std::num::NonZeroU32::new(pid)?).ok()??;
    identity.exe().map(Path::to_path_buf)
}

/// Inspect the instance lock. The recorded pid counts only while it is alive with its recorded
/// start time (pid reuse and legacy records without a start time are Free and never killed).
fn check_instance_lock(lock_path: &Path) -> LockCheck {
    let Ok(text) = fs::read_to_string(lock_path) else {
        return LockCheck::Free;
    };
    let Ok(lock) = serde_json::from_str::<InstanceLock>(&text) else {
        let _ = fs::remove_file(lock_path);
        return LockCheck::Free;
    };
    if lock.pid == 0 || !process_alive(lock.pid) {
        let _ = fs::remove_file(lock_path);
        return LockCheck::Free;
    }
    if !lock_pid_is(lock.pid, lock.pid_started.as_deref()) {
        return LockCheck::Free;
    }
    if lock.owner_pid != 0 && lock_pid_is(lock.owner_pid, lock.owner_started.as_deref()) {
        LockCheck::Busy {
            pid: lock.pid,
            owner_pid: lock.owner_pid,
        }
    } else {
        LockCheck::Orphan { pid: lock.pid }
    }
}

/// Enforce at most one sd-server per data root. Reaps a recorded orphan whose
/// owner died; refuses a spawn while a live owner holds the backend.
fn claim_instance_lock(cfg: &SdServerConfig) -> Result<PathBuf, String> {
    let lock_path = cfg.log_dir.join(INSTANCE_LOCK_FILE);
    match check_instance_lock(&lock_path) {
        LockCheck::Free => Ok(lock_path),
        LockCheck::Busy { pid, owner_pid } => Err(format!(
            "backend_busy: sd-server already managed by a live instance (pid {pid}, owner {owner_pid})"
        )),
        LockCheck::Orphan { pid } => {
            rightkit_logs::JsonlStore::new(cfg.log_dir.join(LOCAL_IMAGE_LOG_FILE), LOCAL_IMAGE_LOG_MAX_BYTES).append_best_effort(&rightkit_logs::EventRecord::new(log_now_ms(), rightkit_logs::Level::Info, "instance_orphan_reaped").with_field("pid", serde_json::json!(pid)));
            let gone = kill_recorded_tree(pid);
            let _ = fs::remove_file(&lock_path);
            if gone {
                Ok(lock_path)
            } else {
                Err(format!("backend_busy: orphaned sd-server pid {pid} did not exit"))
            }
        }
    }
}

fn write_instance_lock(
    lock_path: &Path,
    cfg: &SdServerConfig,
    pid: u32,
    session_id: &str,
    binary_sha256: &str,
    port: u16,
) {
    let record = InstanceLock {
        pid,
        owner_pid: std::process::id(),
        owner_binary: std::env::current_exe()
            .ok()
            .map(|p| p.to_string_lossy().into_owned()),
        binary: cfg.binary.to_string_lossy().to_string(),
        binary_sha256: binary_sha256.to_string(),
        session_id: session_id.to_string(),
        started_at: rfc3339_now(),
        port: Some(port),
        pid_started: crate::machine::process_started(pid),
        owner_started: crate::machine::process_started(std::process::id()),
    };
    let _ = rightkit_fs::atomic_write(
        lock_path,
        serde_json::to_string_pretty(&record)
            .unwrap_or_default()
            .as_bytes(),
    );
}

/// Kill a recorded model server that the caller has ALREADY proven to be that server (pid and
/// start time, see `machine::identity_check`, which returns the proven identity). `root` is the
/// recorded kill target (watchdog/group leader on macOS, the server elsewhere). Stops its live
/// descendants first, then the server, graceful then forced after 2 s, every signal checked
/// against the pid's start time (rightkit-child-process `sweep`), so a recycled pid is refused.
/// Descendants whose parent already exited are not findable (an owned server's group is killed by
/// `kill_on_parent_exit` instead). Waits up to 10 s for the server to go; true when it is gone.
pub(crate) fn kill_verified_pid(root: &AdoptedIdentity) -> bool {
    let mut targets = root.descendants().unwrap_or_default();
    targets.reverse();
    targets.push(root.clone());
    let _ = sweep(&targets, Duration::from_secs(2));
    root.wait_confirmed_gone(Duration::from_secs(10))
        .unwrap_or(false)
}

/// Reap a recorded orphaned backend at service start. Returns the pid when
/// one was killed. Public for the service entry point.
pub fn reap_orphaned_backend(log_dir: &Path, _binary: &Path, _supervised: bool) -> Option<u32> {
    match check_instance_lock(&log_dir.join(INSTANCE_LOCK_FILE)) {
        LockCheck::Orphan { pid } => {
            rightkit_logs::JsonlStore::new(
                (log_dir).join(LOCAL_IMAGE_LOG_FILE),
                LOCAL_IMAGE_LOG_MAX_BYTES,
            )
            .append_best_effort(
                &rightkit_logs::EventRecord::new(
                    log_now_ms(),
                    rightkit_logs::Level::Info,
                    "instance_orphan_reaped",
                )
                .with_field("pid", serde_json::json!(pid))
                .with_field("at", serde_json::json!("service_start")),
            );
            kill_recorded_tree(pid);
            let _ = fs::remove_file(log_dir.join(INSTANCE_LOCK_FILE));
            Some(pid)
        }
        _ => None,
    }
}

/// Shared idle-unload decision used by the service worker loop and by the
/// live qualification harness: stop the server when `last_active` is older
/// than `idle` (0/None disables). Returns true when a server was stopped.
pub fn unload_if_idle(
    server: &mut Option<SdServer>,
    last_active: Option<Instant>,
    idle: Duration,
    log_dir: &Path,
) -> Result<bool, String> {
    let Some(at) = last_active else {
        return Ok(false);
    };
    if idle.is_zero() || at.elapsed() < idle || server.is_none() {
        return Ok(false);
    }
    server.take().unwrap().stop(log_dir)?;
    rightkit_logs::JsonlStore::new(
        (log_dir).join(LOCAL_IMAGE_LOG_FILE),
        LOCAL_IMAGE_LOG_MAX_BYTES,
    )
    .append_best_effort(
        &rightkit_logs::EventRecord::new(log_now_ms(), rightkit_logs::Level::Info, "runtime_idle")
            .with_field("idle_seconds", serde_json::json!(idle.as_secs())),
    );
    Ok(true)
}

/// Explicit unload (`local_image.unload`): stop the server now, whatever its last-active time,
/// and log it as `runtime_unload`. The caller must only call this from the idle branch of the
/// service worker, where no job holds the server. Returns true when a server was stopped.
pub fn unload_now(server: &mut Option<SdServer>, log_dir: &Path) -> Result<bool, String> {
    let Some(mut running) = server.take() else {
        return Ok(false);
    };
    running.stop(log_dir)?;
    rightkit_logs::JsonlStore::new(
        (log_dir).join(LOCAL_IMAGE_LOG_FILE),
        LOCAL_IMAGE_LOG_MAX_BYTES,
    )
    .append_best_effort(&rightkit_logs::EventRecord::new(
        log_now_ms(),
        rightkit_logs::Level::Info,
        "runtime_unload",
    ));
    Ok(true)
}

/// Resident memory of the backend supervisor and its descendants, in MB
/// (rightkit-child-process `tree_usage`: one process-table snapshot, every member identity-checked).
pub fn group_rss_mb(pid: u32) -> Option<f64> {
    let root = rightkit_child_process::AdoptedIdentity::capture(std::num::NonZeroU32::new(pid)?)
        .ok()??;
    let usage = rightkit_child_process::tree_usage(&root).ok()?;
    (usage.resident_bytes > 0).then(|| usage.resident_bytes as f64 / 1_048_576.0)
}

/// Liveness probe (rightkit-child-process `AdoptedProcess::is_running`: `kill(pid, 0)` on Unix,
/// a zero-timeout wait on a query handle on Windows). A zombie or an exited process whose object
/// lingers reads as not running.
pub fn process_alive(pid: u32) -> bool {
    let Some(pid) = std::num::NonZeroU32::new(pid) else {
        return false;
    };
    rightkit_child_process::AdoptedProcess::new(pid)
        .is_running()
        .unwrap_or(false)
}

/// Stop a RECORDED sd-server (an orphan this process does not own) and its live descendants.
/// The caller has already proven the pid is that server. Captures the pid's identity now and
/// signals only that identity (see `kill_verified_pid`). True when the pid is gone (including
/// when it never existed).
pub fn kill_recorded_tree(pid: u32) -> bool {
    let Some(nz) = std::num::NonZeroU32::new(pid) else {
        return true;
    };
    match AdoptedIdentity::capture(nz) {
        Ok(Some(root)) => kill_verified_pid(&root),
        Ok(None) => true,
        Err(_) => !process_alive(pid),
    }
}

/// Drain one backend stream into a bounded redacted tail and the JSONL `backend_log` event
/// (rightkit-child-process `StderrTail`; the session token never reaches either).
fn backend_tail<R: std::io::Read + Send + 'static>(
    stream: R,
    name: &'static str,
    log_dir: &Path,
    session: &str,
    pid: u32,
    token: &str,
) -> StderrTail {
    let secrets = rightkit_logs::KnownSecrets::from_values([token]);
    let log_path = log_dir.join(LOCAL_IMAGE_LOG_FILE);
    let session = session.to_string();
    StderrTail::capture(
        stream,
        TAIL_MAX_BYTES,
        move |line| Some(secrets.redact(line)),
        move |line| {
            rightkit_logs::JsonlStore::new(log_path.clone(), LOCAL_IMAGE_LOG_MAX_BYTES)
                .append_best_effort(
                    &rightkit_logs::EventRecord::new(
                        log_now_ms(),
                        rightkit_logs::Level::Info,
                        "backend_log",
                    )
                    .with_field("stream", serde_json::json!(name))
                    .with_field("line", serde_json::json!(line))
                    .with_field("session", serde_json::json!(session))
                    .with_field("pid", serde_json::json!(pid)),
                );
        },
    )
}

#[cfg(test)]
mod unload_now_tests {
    use super::*;

    #[test]
    fn unload_with_no_server_stops_nothing() {
        let dir = std::env::temp_dir();
        let mut server: Option<SdServer> = None;
        assert_eq!(unload_now(&mut server, &dir), Ok(false));
        assert!(server.is_none());
    }
}
