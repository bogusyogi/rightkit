//! Opt-in frontend lookahead probe for the macOS CoreML Parakeet path.
//!
//! The worker owns separately loaded MelSpectrogram and AudioEncoder stages.
//! `Stage` is `Send` but intentionally not `Sync`, so no stage or mutable
//! prediction input crosses between the decode thread and this worker. The
//! worker holds one request at a time and returns one encoded tensor; callers
//! must keep decode order and fall back to the normal frontend on a miss.
//!
//! Production activation supplies an explicit memory gate after a validated
//! fused model load; evaluator callers may still use the environment config.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};

use objc2::rc::autoreleasepool;

use crate::coreml::{ml_f16, ml_i32, read_f16, Stage};

use super::model::{stage_path, PipelineMeta};

/// Static Parakeet v3 encoder width used by the production decode path.
pub const ENCODER_DIM: usize = 1024;
const BYTES_PER_F32: usize = std::mem::size_of::<f32>();
const GIB: u64 = 1024 * 1024 * 1024;

/// FluidVoice's source-observed admission thresholds. These are policy
/// defaults for this probe, not a claim that every machine benefits from it.
pub const MIN_PHYSICAL_MEMORY_BYTES: u64 = 24 * GIB;
pub const MIN_AVAILABLE_MEMORY_BYTES: u64 = 8 * GIB;

/// Memory facts supplied by caller's platform probe. Keeping this input
/// explicit avoids pretending this module has a reliable cross-version
/// available-memory API.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MemoryGate {
    pub physical_memory_bytes: u64,
    pub available_memory_bytes: u64,
}

impl MemoryGate {
    pub fn admits(self) -> bool {
        self.physical_memory_bytes >= MIN_PHYSICAL_MEMORY_BYTES
            && self.available_memory_bytes >= MIN_AVAILABLE_MEMORY_BYTES
    }
}

/// Admission controls for [`FrontendLookaheadProbe`]. Default configuration
/// is disabled, preserving the normal four-stage runtime exactly.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrontendLookaheadConfig {
    pub enabled: bool,
    /// Upper bound for the transient worker tensors and its one cached output.
    pub max_memory_bytes: usize,
    pub memory_gate: Option<MemoryGate>,
    pub output_backings: bool,
}

impl FrontendLookaheadConfig {
    pub fn production(memory_gate: MemoryGate) -> Self {
        Self {
            enabled: true,
            max_memory_bytes: 8 * 1024 * 1024,
            memory_gate: Some(memory_gate),
            output_backings: true,
        }
    }

    pub fn evaluator(memory_gate: MemoryGate, output_backings: bool) -> Self {
        Self {
            enabled: true,
            max_memory_bytes: 8 * 1024 * 1024,
            memory_gate: Some(memory_gate),
            output_backings,
        }
    }
}

/// Run a macOS probe tool under RightKit process ownership (`rightkit_child_process::run`)
/// and return its stdout on a zero exit inside the deadline. Stderr is bounded and discarded.
#[cfg(target_os = "macos")]
fn probe_stdout(program: &str, args: &[&str]) -> Option<String> {
    let options = rightkit_child_process::RunOptions::new(std::time::Duration::from_secs(10))
        .stdout_cap(1024 * 1024)
        .stderr_cap(4096);
    let output = rightkit_child_process::run(program, args, &options).ok()?;
    if output.timed_out || !output.status.success() || output.stdout.truncated {
        return None;
    }
    String::from_utf8(output.stdout.bytes).ok()
}

/// Read native macOS memory facts used by production admission. The free
/// percentage is the same system-wide signal used by qualification receipts;
/// unavailable facts fail closed and leave split frontend execution intact.
#[cfg(target_os = "macos")]
pub fn native_memory_gate() -> Option<MemoryGate> {
    let physical = probe_stdout("/usr/sbin/sysctl", &["-n", "hw.memsize"])
        .and_then(|value| value.trim().parse::<u64>().ok())?;
    let pressure = probe_stdout("/usr/bin/memory_pressure", &["-Q"])?;
    let free_percent = pressure
        .lines()
        .find_map(|line| line.strip_prefix("System-wide memory free percentage: "))
        .and_then(|value| value.trim().trim_end_matches('%').parse::<u64>().ok())
        .filter(|value| *value <= 100)?;
    Some(MemoryGate {
        physical_memory_bytes: physical,
        available_memory_bytes: physical / 100 * free_percent,
    })
}

#[cfg(not(target_os = "macos"))]
pub fn native_memory_gate() -> Option<MemoryGate> {
    None
}

/// Exactly one cached encoder output. `data` is the logical
/// `[encoder_dim, enc_frames]` plane, row-major, matching `read_f16` output
/// consumed by production `decode_window` (`data[d * encoder_frames + t]`).
/// `mel_full` is retained for unchanged window-health diagnostics.
#[derive(Debug)]
pub struct EncodedWindow {
    pub sequence: u64,
    pub real_samples: usize,
    pub encoder_dim: usize,
    pub encoder_frames: usize,
    pub mel_full: Vec<f32>,
    pub data: Vec<f32>,
}

/// Counters proving whether the candidate path actually pre-encoded and
/// consumed windows. These count logical windows, not CoreML internal calls.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FrontendLookaheadCounters {
    pub requests: usize,
    pub encoded: usize,
    pub consumed: usize,
    pub failures: usize,
}

/// Admission and residency facts serialized by evaluators with counters.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrontendLookaheadStats {
    pub counters: FrontendLookaheadCounters,
    pub estimated_peak_bytes: usize,
    pub max_memory_bytes: usize,
    pub physical_memory_bytes: u64,
    pub available_memory_bytes: u64,
    pub worker_resident: bool,
}

impl EncodedWindow {
    pub fn estimated_bytes(&self) -> usize {
        self.mel_full
            .len()
            .saturating_add(self.data.len())
            .saturating_mul(BYTES_PER_F32)
    }

    pub fn value(&self, dimension: usize, frame: usize) -> Option<f32> {
        if dimension >= self.encoder_dim || frame >= self.encoder_frames {
            return None;
        }
        let index = dimension
            .checked_mul(self.encoder_frames)?
            .checked_add(frame)?;
        self.data.get(index).copied()
    }
}

enum Request {
    Encode { sequence: u64, segment: Vec<f32> },
    Shutdown,
}

enum Response {
    Ready(EncodedWindow),
    Failed { sequence: u64, error: String },
}

#[derive(Default)]
struct SharedCounters {
    requests: AtomicUsize,
    encoded: AtomicUsize,
    consumed: AtomicUsize,
    failures: AtomicUsize,
}

impl SharedCounters {
    fn snapshot(&self) -> FrontendLookaheadCounters {
        FrontendLookaheadCounters {
            requests: self.requests.load(Ordering::Relaxed),
            encoded: self.encoded.load(Ordering::Relaxed),
            consumed: self.consumed.load(Ordering::Relaxed),
            failures: self.failures.load(Ordering::Relaxed),
        }
    }
}

/// One-worker frontend lookahead probe. It is deliberately neither `Clone`
/// nor shared; dropping it joins all CoreML work before model stages leave.
pub struct FrontendLookaheadProbe {
    requests: SyncSender<Request>,
    responses: Receiver<Response>,
    worker: Option<JoinHandle<()>>,
    in_flight: Option<u64>,
    terminal_error: Option<String>,
    win_samples: usize,
    estimated_peak_bytes: usize,
    max_memory_bytes: usize,
    memory_gate: MemoryGate,
    counters: Arc<SharedCounters>,
}

impl FrontendLookaheadProbe {
    /// Load independent frontend stages and start one background worker.
    /// Returns `Ok(None)` while disabled. The caller must pass the same
    /// encoder compute profile used by production `CoreMlParakeet`.
    pub fn spawn(
        dir: &Path,
        encoder_profile: &str,
        meta: &PipelineMeta,
        config: FrontendLookaheadConfig,
    ) -> Result<Option<Self>, String> {
        if !config.enabled {
            return Ok(None);
        }
        let win_samples = (meta.window_sec.max(0.0) * 16_000.0) as usize;
        let estimated_peak_bytes = estimate_peak_bytes(meta, win_samples)?;
        let gate = config
            .memory_gate
            .ok_or("frontend lookahead requires physical and available memory facts")?;
        if !gate.admits() {
            return Err(format!(
                "frontend lookahead memory gate rejected {} physical / {} available bytes",
                gate.physical_memory_bytes, gate.available_memory_bytes
            ));
        }
        if config.max_memory_bytes < estimated_peak_bytes {
            return Err(format!(
                "frontend lookahead memory budget {} is below estimated {} bytes",
                config.max_memory_bytes, estimated_peak_bytes
            ));
        }

        // Load before spawning so a failed stage load cannot leave an orphan
        // worker. Once moved, worker owns both stages for its whole lifetime.
        let mut mel =
            Stage::load_with_compute_profile(&stage_path(dir, "MelSpectrogram"), encoder_profile)?;
        let mut enc =
            Stage::load_with_compute_profile(&stage_path(dir, "AudioEncoder"), encoder_profile)?;
        if config.output_backings {
            mel.enable_output_backings();
            enc.enable_output_backings();
        }
        let (requests, request_rx) = mpsc::sync_channel(1);
        let (response_tx, responses) = mpsc::channel();
        let counters = Arc::new(SharedCounters::default());
        let worker_counters = Arc::clone(&counters);
        let worker_meta = meta.clone();
        let worker = thread::Builder::new()
            .name("coreml-frontend-lookahead".into())
            .spawn(move || {
                worker_loop(
                    mel,
                    enc,
                    worker_meta,
                    win_samples,
                    request_rx,
                    response_tx,
                    worker_counters,
                )
            })
            .map_err(|error| format!("spawn frontend lookahead worker: {error}"))?;
        Ok(Some(Self {
            requests,
            responses,
            worker: Some(worker),
            in_flight: None,
            terminal_error: None,
            win_samples,
            estimated_peak_bytes,
            max_memory_bytes: config.max_memory_bytes,
            memory_gate: gate,
            counters,
        }))
    }

    pub fn estimated_peak_bytes(&self) -> usize {
        self.estimated_peak_bytes
    }

    pub fn is_pending(&self) -> bool {
        self.in_flight.is_some()
    }

    pub fn stats(&self) -> FrontendLookaheadStats {
        FrontendLookaheadStats {
            counters: self.counters.snapshot(),
            estimated_peak_bytes: self.estimated_peak_bytes,
            max_memory_bytes: self.max_memory_bytes,
            physical_memory_bytes: self.memory_gate.physical_memory_bytes,
            available_memory_bytes: self.memory_gate.available_memory_bytes,
            worker_resident: self
                .worker
                .as_ref()
                .map(|worker| !worker.is_finished())
                .unwrap_or(false),
        }
    }

    /// Queue one exact production window. Input is already conditioned by the
    /// shared `condition_for_asr` authority; this method never reconditions it.
    /// Segment boundaries and real length are preserved exactly.
    pub fn request(&mut self, sequence: u64, segment: &[f32]) -> Result<(), String> {
        if let Some(error) = &self.terminal_error {
            return Err(error.clone());
        }
        if self.in_flight.is_some() {
            return Err("frontend lookahead already has one request in flight".into());
        }
        if segment.len() > self.win_samples {
            return Err(format!(
                "frontend lookahead segment {} exceeds model window {}",
                segment.len(),
                self.win_samples
            ));
        }
        match self.requests.try_send(Request::Encode {
            sequence,
            segment: segment.to_vec(),
        }) {
            Ok(()) => {
                self.in_flight = Some(sequence);
                self.counters.requests.fetch_add(1, Ordering::Relaxed);
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err("frontend lookahead request queue is full".into()),
            Err(TrySendError::Disconnected(_)) => {
                let error = "frontend lookahead worker stopped".to_string();
                self.terminal_error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Nonblocking result poll. `Ok(None)` means no completed result yet;
    /// caller should decode through normal frontend and retain ordering.
    pub fn try_take(&mut self) -> Result<Option<EncodedWindow>, String> {
        match self.responses.try_recv() {
            Ok(Response::Ready(window)) => {
                self.in_flight = None;
                self.counters.consumed.fetch_add(1, Ordering::Relaxed);
                Ok(Some(window))
            }
            Ok(Response::Failed { sequence, error }) => {
                self.in_flight = None;
                let error = format!("frontend lookahead sequence {sequence}: {error}");
                self.terminal_error = Some(error.clone());
                Err(error)
            }
            Err(TryRecvError::Empty) => Ok(None),
            Err(TryRecvError::Disconnected) => {
                self.in_flight = None;
                let error = "frontend lookahead worker stopped without a result".to_string();
                self.terminal_error = Some(error.clone());
                Err(error)
            }
        }
    }

    /// Wait for one requested sequence. There is at most one in-flight
    /// request, so this cannot reorder windows. It is called only after the
    /// caller has decoded the preceding window, preserving overlap.
    pub fn take(&mut self, sequence: u64) -> Result<EncodedWindow, String> {
        if self.in_flight != Some(sequence) {
            return Err(format!(
                "frontend lookahead sequence {sequence} is not in flight"
            ));
        }
        match self.responses.recv() {
            Ok(Response::Ready(window)) if window.sequence == sequence => {
                self.in_flight = None;
                self.counters.consumed.fetch_add(1, Ordering::Relaxed);
                Ok(window)
            }
            Ok(Response::Ready(window)) => {
                self.in_flight = None;
                let error = format!(
                    "frontend lookahead sequence mismatch: expected {sequence}, got {}",
                    window.sequence
                );
                self.terminal_error = Some(error.clone());
                Err(error)
            }
            Ok(Response::Failed {
                sequence: failed,
                error,
            }) => {
                self.in_flight = None;
                let error = format!("frontend lookahead sequence {failed}: {error}");
                self.terminal_error = Some(error.clone());
                Err(error)
            }
            Err(_) => {
                self.in_flight = None;
                let error = "frontend lookahead worker stopped without a result".to_string();
                self.terminal_error = Some(error.clone());
                Err(error)
            }
        }
    }
}

impl Drop for FrontendLookaheadProbe {
    fn drop(&mut self) {
        // A bounded request queue may contain one Encode. Sending Shutdown
        // waits only until worker receives that request; response channel is
        // unbounded, so worker cannot deadlock trying to report completion.
        let _ = self.requests.send(Request::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

fn worker_loop(
    mel: Stage,
    enc: Stage,
    meta: PipelineMeta,
    win_samples: usize,
    requests: Receiver<Request>,
    responses: mpsc::Sender<Response>,
    counters: Arc<SharedCounters>,
) {
    while let Ok(request) = requests.recv() {
        match request {
            Request::Shutdown => break,
            Request::Encode { sequence, segment } => {
                match encode_window(&mel, &enc, &meta, win_samples, sequence, &segment) {
                    Ok(window) => {
                        counters.encoded.fetch_add(1, Ordering::Relaxed);
                        let _ = responses.send(Response::Ready(window));
                    }
                    Err(error) => {
                        counters.failures.fetch_add(1, Ordering::Relaxed);
                        let _ = responses.send(Response::Failed { sequence, error });
                        break;
                    }
                }
            }
        }
    }
}

fn encode_window(
    mel: &Stage,
    enc: &Stage,
    meta: &PipelineMeta,
    win_samples: usize,
    sequence: u64,
    segment: &[f32],
) -> Result<EncodedWindow, String> {
    let worker_started = std::time::Instant::now();
    let mut mel_predict_ms = 0.0f64;
    let mut encoder_predict_ms = 0.0f64;
    let real_samples = segment.len();
    let mut padded = vec![0.0f32; win_samples];
    padded[..real_samples].copy_from_slice(segment);
    let mel_output = mel_output_name(mel)?;
    let mel_full = autoreleasepool(|_| -> Result<Vec<f32>, String> {
        let audio = ml_f16(&[1, win_samples], &padded)?;
        let len = ml_i32(&[1], &[real_samples as i32])?;
        let started = std::time::Instant::now();
        let output = mel.predict(&[("audio", &audio), ("audio_len", &len)], &mel_output)?;
        mel_predict_ms = started.elapsed().as_secs_f64() * 1_000.0;
        Ok(read_f16(&output))
    })?;
    if mel_full.len() % meta.n_mel != 0 {
        return Err(format!(
            "mel output length {} is not divisible by {}",
            mel_full.len(),
            meta.n_mel
        ));
    }
    let mel_t_full = mel_full.len() / meta.n_mel;
    if mel_t_full < meta.mel_frames {
        return Err(format!(
            "mel output has {mel_t_full} frames, expected at least {}",
            meta.mel_frames
        ));
    }
    let mut mel = vec![0.0f32; meta.n_mel * meta.mel_frames];
    for channel in 0..meta.n_mel {
        for frame in 0..meta.mel_frames {
            mel[channel * meta.mel_frames + frame] = mel_full[channel * mel_t_full + frame];
        }
    }
    let enc_flat = autoreleasepool(|_| -> Result<Vec<f32>, String> {
        let input = ml_f16(&[1, meta.n_mel, meta.mel_frames], &mel)?;
        let len = ml_i32(&[1], &[meta.mel_frames as i32])?;
        let enc_output = enc_output_name(enc)?;
        let started = std::time::Instant::now();
        let output = enc.predict(
            &[("melspectrogram_features", &input), ("mel_len", &len)],
            &enc_output,
        )?;
        encoder_predict_ms = started.elapsed().as_secs_f64() * 1_000.0;
        Ok(read_f16(&output))
    })?;
    if enc_flat.len() % ENCODER_DIM != 0 {
        return Err(format!(
            "encoder output length {} is not divisible by {ENCODER_DIM}",
            enc_flat.len()
        ));
    }
    tracing::info!(
        target: "rightkit_asr_stage_timing",
        sequence,
        sample_count = real_samples,
        mel_predict_ms,
        encoder_predict_ms,
        worker_total_ms = worker_started.elapsed().as_secs_f64() * 1_000.0,
        timing_relation = "worker overlaps preceding window decode; nonadditive",
        "frontend_lookahead_worker_stages"
    );
    Ok(EncodedWindow {
        sequence,
        real_samples,
        encoder_dim: ENCODER_DIM,
        encoder_frames: enc_flat.len() / ENCODER_DIM,
        mel_full,
        data: enc_flat,
    })
}

fn mel_output_name(stage: &Stage) -> Result<String, String> {
    stage
        .output_names()
        .into_iter()
        .next()
        .ok_or("MelSpectrogram has no output".into())
}

fn enc_output_name(stage: &Stage) -> Result<String, String> {
    stage
        .output_names()
        .into_iter()
        .next()
        .ok_or("AudioEncoder has no output".into())
}

/// Peak transient estimate: padded PCM + full/sliced mel + encoder output while
/// one encoded tensor is retained. CoreML model residency is excluded because
/// it is device/compiler dependent; worker stages are separate model objects.
pub fn estimate_peak_bytes(meta: &PipelineMeta, win_samples: usize) -> Result<usize, String> {
    let pcm = win_samples
        .checked_mul(BYTES_PER_F32)
        .ok_or("PCM estimate overflow")?;
    let mel = meta
        .n_mel
        .checked_mul(meta.mel_frames)
        .and_then(|n| n.checked_mul(BYTES_PER_F32))
        .ok_or("mel estimate overflow")?;
    let mel_full = meta
        .n_mel
        .checked_mul(meta.mel_frames.saturating_add(1))
        .and_then(|n| n.checked_mul(BYTES_PER_F32))
        .ok_or("full mel estimate overflow")?;
    let encoder = ENCODER_DIM
        .checked_mul(meta.enc_frames.saturating_add(1))
        .and_then(|n| n.checked_mul(BYTES_PER_F32))
        .ok_or("encoder estimate overflow")?;
    pcm.checked_add(mel)
        .and_then(|n| n.checked_add(mel_full))
        .and_then(|n| n.checked_add(encoder))
        .and_then(|n| n.checked_add(encoder))
        .ok_or_else(|| "frontend lookahead estimate overflow".into())
}
