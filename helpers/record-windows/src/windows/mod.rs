//! Windows.Graphics.Capture backend for one HWND (RK2 Spike B design).
//!
//! The FrameArrived callback takes the frame, copies it into a staging slot, and
//! hands the slot index to a worker. Pacing, drop aggregation, readback, encoding
//! (or BMP writes) and events run on the worker, so no encode or disk work happens
//! in the callback. The worker never runs on the capture thread.
//!
//! This module depends only on `crate::{args, bmp, codec, error, journal, pacer, record}`
//! and the `windows` crate, so it moves unchanged into rightkit-record.

mod gpu;
mod mf;
mod window;

use std::ffi::c_void;
use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ::windows::core::{factory, IInspectable, Interface, Ref};
use ::windows::Foundation::TypedEventHandler;
use ::windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess, GraphicsCaptureAccessKind,
    GraphicsCaptureItem, GraphicsCaptureSession,
};
use ::windows::Graphics::DirectX::DirectXPixelFormat;
use ::windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;
use ::windows::Win32::Foundation::HWND;
use ::windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
use ::windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use ::windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use ::windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use ::windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use ::windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use rightkit_contracts::types::record_event::DropReason;
use serde_json::{json, Value};

use crate::args::CaptureArgs;
use crate::bmp;
use crate::codec::{self, Codec};
use crate::error::{Code, RecordError};
use crate::journal::Journal;
use crate::pacer::{ArrivalStats, Closed, Decision, DropAggregator, Losses, Pacer};
use crate::record::{self, ClockDraft, DraftInput};

/// Staging slots in the ring. Each in-flight frame holds one slot.
const SLOTS: usize = 6;
const STATUS_PARTIAL: &str = "partial_quarantined";
const MP4_NAME: &str = "capture.mp4";

type EventJournal = Journal<BufWriter<File>>;

/// Entry point. Writes the encoded video (or BMP frames), events and the
/// record-session draft into `args.out`. Session events bracket the capture on
/// every exit path, including a refused window and a setup error.
pub fn run(args: &CaptureArgs) -> Result<Value, RecordError> {
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
    prepare_out_dir(&args.out, args.codec)?;
    let now_ms = unix_millis();
    let session_id = format!("bundle.win-{now_ms}");
    let created_at = record::iso8601_utc((now_ms / 1000) as i64);

    let file = File::create(args.out.join("events.ndjson")).map_err(|e| io_error("create events.ndjson", e))?;
    let journal: Arc<Mutex<EventJournal>> = Arc::new(Mutex::new(Journal::new(BufWriter::new(file), session_id.clone())));
    lock(&journal).session_start(qpc_now_ns())?;

    let captured = capture(args, &journal);
    let ended = {
        let mut guard = lock(&journal);
        guard.session_end(qpc_now_ns()).and_then(|()| guard.flush())
    };
    let captured = match (captured, ended) {
        (Err(err), _) | (Ok(_), Err(err)) => return Err(err),
        (Ok(captured), Ok(())) => captured,
    };

    let event_count = lock(&journal).count();
    let initial = &captured.initial;
    let output = &captured.output;
    let stop_error = captured.stop_error;
    let reason = match &stop_error {
        Some(err) => format!("first cut, not complete: capture stopped early ({err}); no bundle writer yet"),
        None => "first cut, not complete: no bundle writer; scenario, target, files, dependencies and redaction not produced".to_string(),
    };
    let clock = output.first_host_ns.map(|first| ClockDraft { host_ns_at_start: captured.host_ns_at_start, first_host_ns: first });
    let os_build = window::os_build();
    let scale = f64::from(initial.dpi) / 96.0;
    let rect = initial.rect;
    let bounds_pt = [
        f64::from(rect.left) * 96.0 / f64::from(initial.dpi),
        f64::from(rect.top) * 96.0 / f64::from(initial.dpi),
        f64::from(rect.right - rect.left) * 96.0 / f64::from(initial.dpi),
        f64::from(rect.bottom - rect.top) * 96.0 / f64::from(initial.dpi),
    ];
    let visibility_state = if initial.on_monitor { "on_screen_visible" } else { "off_screen_visible" };
    let dropped = output.paced_drops + output.encoder_drops + captured.backpressure + captured.size_mismatch + captured.copy_failures;
    let draft = record::session_draft(&DraftInput {
        bundle_id: &session_id,
        created_at: &created_at,
        reason: &reason,
        os_build: &os_build,
        fps: args.fps,
        codec: args.codec.draft_codec(),
        scale,
        width: captured.width,
        height: captured.height,
        visibility_state,
        bounds_pt,
        dpi: initial.dpi,
        clock,
        frames_count: output.kept,
        frames_dropped: dropped,
        events_count: event_count,
    });
    let draft_path = args.out.join("record-session.draft.json");
    fs::write(&draft_path, serde_json::to_string_pretty(&draft).unwrap_or_default())
        .map_err(|e| io_error("write record-session.draft.json", e))?;
    let validation = record::validate_draft(&draft);

    if let Some(err) = stop_error {
        return Err(err);
    }
    let seconds = captured.elapsed.as_secs_f64().max(f64::MIN_POSITIVE);
    let output_file = match args.codec {
        Codec::Bmp => None,
        _ => Some(args.out.join(MP4_NAME)),
    };
    Ok(json!({
        "ok": true,
        "outDir": args.out,
        "status": STATUS_PARTIAL,
        "requestedFps": args.fps,
        "codec": args.codec.as_str(),
        "container": args.codec.container(),
        "encoder": captured.encoder,
        "outputFile": output_file,
        "encodedSize": { "width": captured.encoded_size.0, "height": captured.encoded_size.1 },
        "visibilityState": visibility_state,
        "elapsedMs": captured.elapsed.as_millis() as u64,
        "arrived": captured.arrived,
        "arrivalIntervalsMs": output.arrivals.summary(),
        "kept": output.kept,
        "achievedFps": output.kept as f64 / seconds,
        "pacedDrops": output.paced_drops,
        "encoderDrops": output.encoder_drops,
        "backpressureDrops": captured.backpressure,
        "sizeMismatchDrops": captured.size_mismatch,
        "copyFailures": captured.copy_failures,
        "events": event_count,
        "recordSessionDraft": {
            "path": draft_path,
            "validatesAsRecordSession": validation.is_ok(),
            "validationError": validation.err(),
        },
    }))
}

/// What a capture attempt that reached the worker leaves behind for the report.
struct Captured {
    initial: window::WindowState,
    width: u32,
    height: u32,
    encoded_size: (u32, u32),
    encoder: Option<Value>,
    host_ns_at_start: u64,
    elapsed: Duration,
    arrived: u64,
    backpressure: u64,
    size_mismatch: u64,
    copy_failures: u64,
    output: WorkerOutput,
    /// Failure that ended the capture early, or the worker's own error.
    stop_error: Option<RecordError>,
}

/// Runs the capture. Returns an error for refusals and setup failures, before any frame.
fn capture(args: &CaptureArgs, journal: &Arc<Mutex<EventJournal>>) -> Result<Captured, RecordError> {
    let hwnd = HWND(args.hwnd as *mut c_void);
    let initial = window::check(hwnd)?;

    // Declared before the sink, so the sink is finalized before MFShutdown runs.
    let _mf = if args.codec.uses_media_foundation() { Some(mf::Startup::start()?) } else { None };

    let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
    let access = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
        .and_then(|operation| operation.get())
        .map_err(|e| capture_error(Code::CaptureUnsupported, "graphics capture access request", e))?;
    if access != AppCapabilityAccessStatus::Allowed {
        return Err(RecordError::new(
            Code::CaptureUnsupported,
            format!("graphics capture borderless access status is {access:?}, not Allowed"),
        ));
    }

    let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
        .map_err(|e| capture_error(Code::CaptureUnsupported, "GraphicsCaptureItem interop", e))?;
    let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd) }
        .map_err(|e| capture_error(Code::CaptureFailed, "CreateForWindow", e))?;
    let size = item.Size().map_err(|e| capture_error(Code::CaptureFailed, "item size", e))?;
    let (width, height) = (size.Width as u32, size.Height as u32);

    // Encoder setup runs before capture starts, so a missing codec fails with no capture.
    let (sink, encoder, encoded_size) = match args.codec {
        Codec::Bmp => (Sink::Bmp { frames: args.out.join("frames") }, None, (width, height)),
        _ => {
            let writer = mf::Mp4Writer::create(&args.out.join(MP4_NAME), args.codec, width, height, args.fps)?;
            let encoder = writer.encoder().to_json();
            let encoded_size = writer.frame_size();
            (Sink::Mp4(writer), Some(encoder), encoded_size)
        }
    };

    let device = gpu::Device::create()?;
    let readback = device.readback(width, height, SLOTS)?;
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        device.winrt(),
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )
    .map_err(|e| capture_error(Code::CaptureFailed, "CreateFreeThreaded", e))?;
    let session: GraphicsCaptureSession = pool
        .CreateCaptureSession(&item)
        .map_err(|e| capture_error(Code::CaptureFailed, "CreateCaptureSession", e))?;
    session
        .SetIsBorderRequired(false)
        .map_err(|e| capture_error(Code::CaptureFailed, "SetIsBorderRequired", e))?;
    session
        .SetIsCursorCaptureEnabled(false)
        .map_err(|e| capture_error(Code::CaptureFailed, "SetIsCursorCaptureEnabled", e))?;

    let (free_tx, free_rx) = mpsc::channel::<usize>();
    for slot in 0..SLOTS {
        let _ = free_tx.send(slot);
    }
    let (frames_tx, frames_rx) = mpsc::sync_channel::<FrameMsg>(SLOTS);
    let shared = Arc::new(Shared {
        stopped: AtomicBool::new(false),
        arrived: AtomicU64::new(0),
        backpressure_drops: AtomicU64::new(0),
        size_mismatch_drops: AtomicU64::new(0),
        copy_failures: AtomicU64::new(0),
        free_rx: Mutex::new(free_rx),
        free_tx,
        frames_tx: Mutex::new(Some(frames_tx)),
        readback: Mutex::new(readback),
        width,
        height,
    });

    let worker = {
        let shared = Arc::clone(&shared);
        let journal = Arc::clone(journal);
        let fps = args.fps;
        thread::spawn(move || worker(shared, frames_rx, fps, journal, sink))
    };

    let callback_shared = Arc::clone(&shared);
    let handler = TypedEventHandler::new(move |sender: Ref<'_, Direct3D11CaptureFramePool>, _: Ref<'_, IInspectable>| {
        if let Ok(pool) = sender.ok() {
            on_frame(&callback_shared, pool);
        }
        Ok(())
    });
    let token = pool
        .FrameArrived(&handler)
        .map_err(|e| capture_error(Code::CaptureFailed, "FrameArrived", e))?;

    let host_ns_at_start = qpc_now_ns();
    let started = Instant::now();
    if let Err(e) = session.StartCapture() {
        shutdown(&shared, &pool, &session, token, worker);
        return Err(capture_error(Code::CaptureFailed, "StartCapture", e));
    }

    let deadline = started + Duration::from_secs(u64::from(args.seconds));
    let mut failure: Option<RecordError> = None;
    while Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
        if shared.stopped.load(Ordering::Acquire) {
            break;
        }
        if let Err(err) = window::check(hwnd) {
            failure = Some(err);
            break;
        }
    }
    let elapsed = started.elapsed();
    // The worker finalizes the sink here, on the normal, error and refusal paths alike.
    let output = shutdown(&shared, &pool, &session, token, worker);
    let stop_error = failure.or_else(|| output.error.clone());

    Ok(Captured {
        initial,
        width,
        height,
        encoded_size,
        encoder,
        host_ns_at_start,
        elapsed,
        arrived: shared.arrived.load(Ordering::Relaxed),
        backpressure: shared.backpressure_drops.load(Ordering::Relaxed),
        size_mismatch: shared.size_mismatch_drops.load(Ordering::Relaxed),
        copy_failures: shared.copy_failures.load(Ordering::Relaxed),
        output,
        stop_error,
    })
}

struct Shared {
    stopped: AtomicBool,
    arrived: AtomicU64,
    backpressure_drops: AtomicU64,
    size_mismatch_drops: AtomicU64,
    copy_failures: AtomicU64,
    free_rx: Mutex<Receiver<usize>>,
    free_tx: Sender<usize>,
    frames_tx: Mutex<Option<SyncSender<FrameMsg>>>,
    readback: Mutex<gpu::Readback>,
    width: u32,
    height: u32,
}

impl Shared {
    /// Totals of the losses the callback has counted so far. Monotonic.
    fn losses(&self) -> Losses {
        Losses {
            backpressure: self.backpressure_drops.load(Ordering::Relaxed),
            unknown: self.size_mismatch_drops.load(Ordering::Relaxed) + self.copy_failures.load(Ordering::Relaxed),
        }
    }
}

#[derive(Clone, Copy)]
struct FrameMsg {
    slot: usize,
    host_ns: u64,
}

/// Where kept frames go. The MP4 variant owns the Media Foundation sink writer.
enum Sink {
    Bmp { frames: PathBuf },
    Mp4(mf::Mp4Writer),
}

impl Sink {
    fn finalize(&mut self) -> Result<(), RecordError> {
        match self {
            Sink::Bmp { .. } => Ok(()),
            Sink::Mp4(writer) => writer.finalize(),
        }
    }
}

/// Worker-owned frame state: the sink and a reusable BGRA buffer.
struct Recorder {
    sink: Sink,
    scratch: Vec<u8>,
}

#[derive(Default)]
struct WorkerOutput {
    kept: u64,
    paced_drops: u64,
    /// Frames that arrived after a sink failure and were not written.
    encoder_drops: u64,
    first_host_ns: Option<u64>,
    arrivals: ArrivalStats,
    error: Option<RecordError>,
}

/// Runs in the FrameArrived callback. Copies and returns quickly, never encodes or writes.
fn on_frame(shared: &Shared, pool: &Direct3D11CaptureFramePool) {
    let Ok(frame) = pool.TryGetNextFrame() else {
        return;
    };
    if shared.stopped.load(Ordering::Acquire) {
        let _ = frame.Close();
        return;
    }
    shared.arrived.fetch_add(1, Ordering::Relaxed);
    match take_slot(shared) {
        None => {
            shared.backpressure_drops.fetch_add(1, Ordering::Relaxed);
        }
        Some(slot) => match copy_frame(shared, slot, &frame) {
            Ok(host_ns) => {
                let queued = match lock(&shared.frames_tx).as_ref() {
                    Some(tx) => tx.try_send(FrameMsg { slot, host_ns }).is_ok(),
                    None => false,
                };
                if !queued {
                    shared.backpressure_drops.fetch_add(1, Ordering::Relaxed);
                    let _ = shared.free_tx.send(slot);
                }
            }
            Err(CopyError::SizeMismatch) => {
                shared.size_mismatch_drops.fetch_add(1, Ordering::Relaxed);
                let _ = shared.free_tx.send(slot);
            }
            Err(CopyError::Failed) => {
                shared.copy_failures.fetch_add(1, Ordering::Relaxed);
                let _ = shared.free_tx.send(slot);
            }
        },
    }
    let _ = frame.Close();
}

enum CopyError {
    SizeMismatch,
    Failed,
}

fn copy_frame(shared: &Shared, slot: usize, frame: &Direct3D11CaptureFrame) -> Result<u64, CopyError> {
    let ticks = frame.SystemRelativeTime().map_err(|_| CopyError::Failed)?.Duration;
    if ticks < 0 {
        return Err(CopyError::Failed);
    }
    let surface = frame.Surface().map_err(|_| CopyError::Failed)?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|_| CopyError::Failed)?;
    let texture: ID3D11Texture2D = unsafe { access.GetInterface() }.map_err(|_| CopyError::Failed)?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    if desc.Width != shared.width || desc.Height != shared.height {
        return Err(CopyError::SizeMismatch);
    }
    lock(&shared.readback).copy_in(slot, &texture);
    Ok(ticks as u64 * 100)
}

fn take_slot(shared: &Shared) -> Option<usize> {
    lock(&shared.free_rx).try_recv().ok()
}

/// Worker thread: paces, aggregates drops, reads back kept frames, writes them to
/// the sink and events, then finalizes the sink. Finalization runs after errors too.
fn worker(
    shared: Arc<Shared>,
    rx: Receiver<FrameMsg>,
    fps: u32,
    journal: Arc<Mutex<EventJournal>>,
    sink: Sink,
) -> WorkerOutput {
    let mut output = WorkerOutput::default();
    let mut recorder = Recorder { sink, scratch: Vec::new() };
    let mut pacer = Pacer::new(fps);
    let mut aggregator = DropAggregator::new(fps);
    let mut closed: Vec<Closed> = Vec::new();
    let mut seen = Losses::default();
    for msg in rx {
        let now = shared.losses();
        let losses = Losses {
            backpressure: now.backpressure.saturating_sub(seen.backpressure),
            unknown: now.unknown.saturating_sub(seen.unknown),
        };
        seen = now;
        if output.error.is_some() {
            output.encoder_drops += 1;
        } else {
            let kept = pacer.offer(msg.host_ns as i64) == Decision::Keep;
            output.arrivals.record(msg.host_ns as i64);
            aggregator.arrival(kept, losses, &mut closed);
            write_drops(&journal, msg.host_ns, &mut closed, &mut output);
            if !kept {
                output.paced_drops += 1;
            } else if let Err(err) = keep_frame(&shared, &mut output, &mut recorder, &journal, msg) {
                output.error = Some(err);
                output.encoder_drops += 1;
                shared.stopped.store(true, Ordering::Release);
            }
        }
        let _ = shared.free_tx.send(msg.slot);
    }

    let end_ns = output.first_host_ns.unwrap_or(0);
    aggregator.finish(&mut closed);
    write_drops(&journal, end_ns, &mut closed, &mut output);
    if output.encoder_drops > 0 {
        // Sink failures are one run: they are reported together, after the last kept frame.
        let count = u32::try_from(output.encoder_drops).unwrap_or(u32::MAX);
        let interval_ms = f64::from(count) * 1000.0 / f64::from(fps);
        let result = lock(&journal).frame_dropped(end_ns, count, interval_ms, DropReason::Encoder);
        if let Err(err) = result {
            output.error.get_or_insert(err);
        }
    }
    if let Err(err) = recorder.sink.finalize() {
        output.error.get_or_insert(err);
    }
    output
}

/// Writes the closed drop runs as frame_dropped events. A write failure is kept as the worker error.
fn write_drops(journal: &Mutex<EventJournal>, host_ns: u64, closed: &mut Vec<Closed>, output: &mut WorkerOutput) {
    for run in closed.drain(..) {
        let result = lock(journal).frame_dropped(host_ns, run.count, run.interval_ms, run.reason);
        if let Err(err) = result {
            output.error.get_or_insert(err);
        }
    }
}

fn keep_frame(
    shared: &Shared,
    output: &mut WorkerOutput,
    rec: &mut Recorder,
    journal: &Mutex<EventJournal>,
    msg: FrameMsg,
) -> Result<(), RecordError> {
    let first = *output.first_host_ns.get_or_insert(msg.host_ns);
    let sample_pts_ns = msg.host_ns.saturating_sub(first);
    lock(&shared.readback).read_packed(msg.slot, &mut rec.scratch)?;
    let index = output.kept;
    match &mut rec.sink {
        Sink::Bmp { frames } => {
            let mut file = Vec::with_capacity(bmp::HEADER_LEN + rec.scratch.len());
            file.extend_from_slice(&bmp::header(shared.width, shared.height));
            file.extend_from_slice(&rec.scratch);
            let path = frames.join(format!("frame_{index:06}.bmp"));
            fs::write(&path, &file).map_err(|e| io_error("write frame", e))?;
        }
        Sink::Mp4(writer) => {
            let sample_time = codec::sample_time_100ns(msg.host_ns, first);
            writer.write_frame(&rec.scratch, shared.width as usize, sample_time)?;
        }
    }
    output.kept += 1;
    lock(journal).frame_ready(index, msg.host_ns, sample_pts_ns)
}

/// Stops capture, lets the worker drain and finalize, and returns its output. Safe to call on any path.
fn shutdown(
    shared: &Shared,
    pool: &Direct3D11CaptureFramePool,
    session: &GraphicsCaptureSession,
    token: i64,
    worker: JoinHandle<WorkerOutput>,
) -> WorkerOutput {
    shared.stopped.store(true, Ordering::Release);
    let _ = pool.RemoveFrameArrived(token);
    let _ = session.Close();
    let _ = pool.Close();
    *lock(&shared.frames_tx) = None;
    worker.join().unwrap_or_else(|_| WorkerOutput {
        error: Some(RecordError::new(Code::CaptureFailed, "worker thread panicked")),
        ..WorkerOutput::default()
    })
}

fn prepare_out_dir(out: &Path, codec: Codec) -> Result<(), RecordError> {
    fs::create_dir_all(out).map_err(|e| io_error("create out dir", e))?;
    let mut entries = fs::read_dir(out).map_err(|e| io_error("read out dir", e))?;
    if entries.next().is_some() {
        return Err(RecordError::new(
            Code::OutDirNotEmpty,
            format!("output directory is not empty: {}", out.display()),
        ));
    }
    if codec == Codec::Bmp {
        fs::create_dir_all(out.join("frames")).map_err(|e| io_error("create frames dir", e))?;
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn capture_error(code: Code, step: &str, error: ::windows::core::Error) -> RecordError {
    RecordError::new(code, format!("{step} failed: {error}"))
}

fn io_error(step: &str, error: std::io::Error) -> RecordError {
    RecordError::new(Code::Io, format!("{step} failed: {error}"))
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn qpc_now_ns() -> u64 {
    let mut counter = 0i64;
    let mut frequency = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut counter);
        let _ = QueryPerformanceFrequency(&mut frequency);
    }
    if frequency <= 0 {
        return 0;
    }
    (counter as u128 * 1_000_000_000 / frequency as u128) as u64
}
