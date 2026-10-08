//! Windows.Graphics.Capture backend for one HWND, implementing the core's
//! [`Backend`] seam.
//!
//! The FrameArrived callback copies a frame into a staging slot and hands the
//! slot to a worker. The worker paces, reads the slot back, hashes the raw
//! pixels, encodes them into an H.264 or HEVC MP4 through Media Foundation, and
//! reports events through a channel that `poll` drains. The callback never
//! encodes or writes. The core owns every bundle file and every protocol check.

pub mod emit;
mod gpu;
mod mf;
mod window;

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
use ::windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use ::windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use ::windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use ::windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
use rightkit_contracts::types::record_session::{RecorderOs, VisibilityState};
use rightkit_contracts::types::Rect;
use serde_json::{json, Value};

pub use crate::codec::Codec;
pub use crate::pacer::Losses;
use crate::backend::{
    Backend, BackendPlan, Failure, Finished, HelperEvent, HelperIdentity, Permission, Source, StartInfo,
};
use crate::codec::sample_time_100ns;
use crate::error::{Code, RecordError};
use crate::hash::blake3_text;
use crate::plan::PixelMasking;
use emit::{Emitter, KeptFrame};

/// Staging slots in the ring. Each in-flight frame holds one slot.
const SLOTS: usize = 6;
/// Bundle-relative media file name the core's bundle layout expects.
const MEDIA_FILE: &str = "clean.mp4";
const HELPER_IDENTITY: &str = "rightkit-record-helper";
/// How often `poll` re-checks that the target window can still deliver frames.
const WINDOW_CHECK_EVERY: Duration = Duration::from_millis(100);

/// The Windows backend. One take at a time. Capture starts in [`Backend::start`]
/// and ends in [`Backend::stop`] or [`Backend::abort`].
pub struct WindowsBackend {
    codec: Codec,
    capture: Option<Capture>,
    events: Option<Receiver<HelperEvent>>,
    encoder: Option<Value>,
    summary: Option<Value>,
}

impl WindowsBackend {
    pub fn new(codec: Codec) -> Self {
        Self { codec, capture: None, events: None, encoder: None, summary: None }
    }

    /// The encoder that was configured, once a take has started.
    pub fn encoder(&self) -> Option<&Value> {
        self.encoder.as_ref()
    }

    /// Frame counts, losses and raw arrival intervals, once the take has ended.
    pub fn summary(&self) -> Option<&Value> {
        self.summary.as_ref()
    }
}

/// A running capture: the pool and session, the callback token once registered,
/// the slot ring and the worker that owns the sink.
struct Capture {
    hwnd: HWND,
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: Option<i64>,
    shared: Arc<Shared>,
    worker: JoinHandle<WorkerOutput>,
    last_window_check: Instant,
    /// Declared last so it drops after the sink is finalized.
    _mf: mf::Startup,
}

impl Capture {
    /// Stops the callback, lets the worker drain and finalize the sink, and
    /// returns what the worker reports. Safe on every path, including a failed start.
    fn shutdown(self) -> WorkerOutput {
        let Capture { shared, pool, session, token, worker, .. } = self;
        shared.stopped.store(true, Ordering::Release);
        if let Some(token) = token {
            let _ = pool.RemoveFrameArrived(token);
        }
        let _ = session.Close();
        let _ = pool.Close();
        *lock(&shared.frames_tx) = None;
        worker.join().unwrap_or_else(|_| WorkerOutput {
            clean: false,
            summary: json!({ "error": "worker thread panicked" }),
        })
    }
}

impl Backend for WindowsBackend {
    fn os(&self) -> RecorderOs {
        RecorderOs::Windows
    }

    fn os_build(&self) -> String {
        window::os_build()
    }

    fn helper(&self) -> HelperIdentity {
        HelperIdentity { identity: HELPER_IDENTITY.into(), signed: false }
    }

    /// Screen-recording permission is requested in `start`, which reports a
    /// denial as `permission_denied`, so this reports granted until then.
    fn permission(&self) -> Permission {
        Permission::Granted
    }

    fn sources(&self) -> Result<Vec<Source>, RecordError> {
        Err(RecordError::new(Code::Unsupported, "window enumeration is not implemented in the Windows backend"))
    }

    fn start(&mut self, plan: &BackendPlan) -> Result<StartInfo, RecordError> {
        if self.capture.is_some() {
            return Err(RecordError::new(Code::AlreadyRecording, "the Windows backend already has a take"));
        }
        check_plan(plan)?;
        let hwnd = hwnd_of(plan.window_id)?;

        let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let initial = window::check(hwnd)?;
        // A take that declares off-screen visibility must not be on a monitor, or the
        // bundle would claim a state the window is not in. StartInfo has no field to
        // report the actual state, so the mismatch is refused instead.
        if matches!(plan.visibility, VisibilityState::OffScreenVisible) && initial.on_monitor {
            return Err(RecordError::new(
                Code::Unsupported,
                "window overlaps a monitor, so it is not off-screen; the take declares off_screen_visible",
            ));
        }
        let scale = f64::from(initial.dpi) / 96.0;

        let media_startup = mf::Startup::start()?;
        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let access = GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
            .and_then(|operation| operation.get())
            .map_err(|e| capture_error(Code::Unsupported, "graphics capture access request", e))?;
        if access != AppCapabilityAccessStatus::Allowed {
            return Err(RecordError::new(
                Code::PermissionDenied,
                format!("graphics capture borderless access status is {access:?}, not Allowed"),
            ));
        }

        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| capture_error(Code::Unsupported, "GraphicsCaptureItem interop", e))?;
        let item: GraphicsCaptureItem = unsafe { interop.CreateForWindow(hwnd) }
            .map_err(|e| capture_error(Code::Helper, "CreateForWindow", e))?;
        let size = item.Size().map_err(|e| capture_error(Code::Helper, "item size", e))?;
        let (width, height) = (size.Width as u32, size.Height as u32);

        // Encoder setup runs before capture starts, so a missing codec fails with no capture.
        let writer = mf::Mp4Writer::create(&plan.out_dir.join(MEDIA_FILE), self.codec, width, height, plan.fps)?;
        let encoder = writer.encoder().to_json();
        let (encoded_width, encoded_height) = writer.frame_size();

        let device = gpu::Device::create()?;
        let readback = device.readback(width, height, SLOTS)?;
        let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
            device.winrt(),
            DirectXPixelFormat::B8G8R8A8UIntNormalized,
            2,
            size,
        )
        .map_err(|e| capture_error(Code::Helper, "CreateFreeThreaded", e))?;
        let session: GraphicsCaptureSession = pool
            .CreateCaptureSession(&item)
            .map_err(|e| capture_error(Code::Helper, "CreateCaptureSession", e))?;
        session
            .SetIsBorderRequired(false)
            .map_err(|e| capture_error(Code::Helper, "SetIsBorderRequired", e))?;
        session
            .SetIsCursorCaptureEnabled(false)
            .map_err(|e| capture_error(Code::Helper, "SetIsCursorCaptureEnabled", e))?;

        let (free_tx, free_rx) = mpsc::channel::<usize>();
        for slot in 0..SLOTS {
            let _ = free_tx.send(slot);
        }
        let (frames_tx, frames_rx) = mpsc::sync_channel::<FrameMsg>(SLOTS);
        let (events_tx, events_rx) = mpsc::channel::<HelperEvent>();
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
            let fps = plan.fps;
            thread::spawn(move || worker(shared, frames_rx, fps, events_tx, writer))
        };

        let callback_shared = Arc::clone(&shared);
        let handler = TypedEventHandler::new(move |sender: Ref<'_, Direct3D11CaptureFramePool>, _: Ref<'_, IInspectable>| {
            if let Ok(pool) = sender.ok() {
                on_frame(&callback_shared, pool);
            }
            Ok(())
        });
        let mut capture = Capture {
            hwnd,
            pool,
            session,
            token: None,
            shared,
            worker,
            last_window_check: Instant::now(),
            _mf: media_startup,
        };
        match capture.pool.FrameArrived(&handler) {
            Ok(token) => capture.token = Some(token),
            Err(e) => {
                capture.shutdown();
                return Err(capture_error(Code::Helper, "FrameArrived", e));
            }
        }

        let host_ns_at_start = qpc_now_ns();
        if let Err(e) = capture.session.StartCapture() {
            capture.shutdown();
            return Err(capture_error(Code::Helper, "StartCapture", e));
        }

        let bounds_pt = Rect {
            x: f64::from(initial.rect.left) / scale,
            y: f64::from(initial.rect.top) / scale,
            width: f64::from(initial.rect.right - initial.rect.left) / scale,
            height: f64::from(initial.rect.bottom - initial.rect.top) / scale,
        };
        self.encoder = Some(encoder);
        self.events = Some(events_rx);
        self.capture = Some(capture);
        self.summary = None;
        Ok(StartInfo {
            host_ns_at_start,
            width: encoded_width,
            height: encoded_height,
            scale,
            bounds_pt,
            dpi: initial.dpi,
            codec: self.codec.as_str().into(),
            os_build: window::os_build(),
        })
    }

    /// Drains the events the worker reported. Also re-checks the target window,
    /// because a window that is hidden, minimized or closed mid-take ends the take.
    fn poll(&mut self) -> Vec<HelperEvent> {
        let mut batch: Vec<HelperEvent> = self.events.as_ref().map(|rx| rx.try_iter().collect()).unwrap_or_default();
        if let Some(capture) = self.capture.as_mut() {
            if capture.last_window_check.elapsed() >= WINDOW_CHECK_EVERY {
                capture.last_window_check = Instant::now();
                if let Err(err) = window::check(capture.hwnd) {
                    let failure = match err.code {
                        Code::NotFound => Failure::TargetExited,
                        _ => Failure::Io(err.message),
                    };
                    batch.push(HelperEvent::Failed(failure));
                }
            }
        }
        batch
    }

    fn stop(&mut self) -> Result<Finished, RecordError> {
        let capture = self
            .capture
            .take()
            .ok_or_else(|| RecordError::new(Code::NotRecording, "the Windows backend has no take to stop"))?;
        let output = capture.shutdown();
        self.summary = Some(output.summary);
        Ok(Finished { clean: output.clean, tracks: Vec::new() })
    }

    fn abort(&mut self) {
        if let Some(capture) = self.capture.take() {
            let output = capture.shutdown();
            self.summary = Some(output.summary);
        }
    }
}

/// Refuses requests the Windows backend cannot honor, before any capture starts.
fn check_plan(plan: &BackendPlan) -> Result<(), RecordError> {
    if !matches!(plan.fps, 30 | 60) {
        return Err(RecordError::new(Code::ArgInvalid, format!("fps must be 30 or 60, got {}", plan.fps)));
    }
    if !plan.audio.is_empty() {
        return Err(RecordError::new(Code::Unsupported, "the Windows backend captures no audio tracks yet"));
    }
    match &plan.masking {
        PixelMasking::Declared(regions) if !regions.is_empty() => Err(RecordError::new(
            Code::Unsupported,
            "the Windows backend does not apply declared pixel masks yet",
        )),
        PixelMasking::Unknown => Err(RecordError::new(
            Code::MaskGeometryUnknown,
            "pixel mask geometry is unknown; the take is refused before capture",
        )),
        _ => Ok(()),
    }
}

fn hwnd_of(window_id: u64) -> Result<HWND, RecordError> {
    if window_id == 0 || window_id > isize::MAX as u64 {
        return Err(RecordError::new(Code::ArgInvalid, format!("window id {window_id} is not a valid HWND")));
    }
    Ok(HWND(window_id as usize as *mut c_void))
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

/// What the worker reports when it ends: whether the sink finished cleanly, and the summary.
struct WorkerOutput {
    clean: bool,
    summary: Value,
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
    // SystemRelativeTime is in 100 ns ticks; the core works in nanoseconds.
    Ok(ticks as u64 * 100)
}

fn take_slot(shared: &Shared) -> Option<usize> {
    lock(&shared.free_rx).try_recv().ok()
}

/// Worker thread: paces, drops and kept frames become events; kept frames are read
/// back, hashed, and encoded. The sink is finalized on every exit path.
fn worker(
    shared: Arc<Shared>,
    rx: Receiver<FrameMsg>,
    fps: u32,
    events: Sender<HelperEvent>,
    mut writer: mf::Mp4Writer,
) -> WorkerOutput {
    let mut emitter = Emitter::new(fps);
    let mut scratch = Vec::new();
    let mut seen = Losses::default();
    let mut failed = false;
    let mut clean = true;
    for msg in rx {
        let now = shared.losses();
        let losses = Losses {
            backpressure: now.backpressure.saturating_sub(seen.backpressure),
            unknown: now.unknown.saturating_sub(seen.unknown),
        };
        seen = now;
        if !failed {
            let kept = emitter.offer(msg.host_ns, losses);
            let mut batch = Vec::new();
            emitter.drain(&mut batch);
            if kept {
                match write_kept(&shared, &mut writer, &mut emitter, &mut scratch, msg) {
                    Ok(event) => batch.push(event),
                    Err(err) => {
                        failed = true;
                        clean = false;
                        shared.stopped.store(true, Ordering::Release);
                        batch.push(HelperEvent::Failed(Failure::Io(err.message)));
                    }
                }
            }
            send_all(&events, batch);
        }
        let _ = shared.free_tx.send(msg.slot);
    }

    let mut tail = Vec::new();
    emitter.finish(&mut tail);
    if let Err(err) = writer.finalize() {
        clean = false;
        if !failed {
            tail.push(HelperEvent::Failed(Failure::Io(err.message)));
        }
    }
    send_all(&events, tail);

    WorkerOutput {
        clean,
        summary: json!({
            "kept": emitter.kept(),
            "arrived": shared.arrived.load(Ordering::Relaxed),
            "losses": {
                "backpressure": shared.backpressure_drops.load(Ordering::Relaxed),
                "sizeMismatch": shared.size_mismatch_drops.load(Ordering::Relaxed),
                "copyFailure": shared.copy_failures.load(Ordering::Relaxed),
            },
            "arrivalIntervalsMs": emitter.arrival_summary(),
        }),
    }
}

/// Reads one kept frame back, hashes its raw pixels, writes it to the sink and
/// returns the readiness or frame event. The hash covers the packed BGRA pixels
/// before conversion and cropping.
fn write_kept(
    shared: &Shared,
    writer: &mut mf::Mp4Writer,
    emitter: &mut Emitter,
    scratch: &mut Vec<u8>,
    msg: FrameMsg,
) -> Result<HelperEvent, RecordError> {
    let kept: KeptFrame = emitter.keep(msg.host_ns);
    lock(&shared.readback).read_packed(msg.slot, scratch)?;
    let raw_hash = blake3_text(scratch);
    let sample_time = sample_time_100ns(kept.host_ns, kept.first_host_ns);
    writer.write_frame(scratch, shared.width as usize, sample_time)?;
    Ok(kept.event(raw_hash))
}

fn send_all(events: &Sender<HelperEvent>, batch: Vec<HelperEvent>) {
    for event in batch {
        // A closed receiver means the take is already gone; nothing is left to report to.
        let _ = events.send(event);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn capture_error(code: Code, step: &str, error: ::windows::core::Error) -> RecordError {
    RecordError::new(code, format!("{step} failed: {error}"))
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
