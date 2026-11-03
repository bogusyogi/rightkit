//! Whole-screen capture on Windows (rightkit#443): one Windows.Graphics.Capture item per monitor
//! (`IGraphicsCaptureItemInterop::CreateForMonitor`), composited on a canvas that spans the
//! selected monitors, written as lossless [`crate::rkv`] frames stamped with the capture's own
//! clock.
//!
//! It follows the window capture in `mod.rs` step for step: the FrameArrived callback copies a
//! frame into a staging slot and hands the slot to the worker, never encoding or writing; the
//! worker reads the slot back, blits it onto the canvas at the monitor's place, paces to the
//! requested fps and appends the canvas to the file. Monitors are listed through DXGI
//! (`EnumOutputs`), so no callback crosses the FFI boundary. Frame timestamps are
//! `SystemRelativeTime`, the QPC time of the frame in 100 ns ticks, scaled to nanoseconds: the same
//! clock as [`WindowsScreen::host_ns`], which is what [`crate::ScreenRecorder::mark`] stamps.
//!
//! Not built or run: written without compiling; the next gate run on a Windows host is the first
//! compile, and the SwiftRight H19 probe on a real monitor is the acceptance.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use ::windows::core::{factory, IInspectable, Interface, Ref};
use ::windows::Foundation::TypedEventHandler;
use ::windows::Graphics::Capture::{
    Direct3D11CaptureFrame, Direct3D11CaptureFramePool, GraphicsCaptureAccess,
    GraphicsCaptureAccessKind, GraphicsCaptureItem, GraphicsCaptureSession,
};
use ::windows::Graphics::DirectX::DirectXPixelFormat;
use ::windows::Security::Authorization::AppCapabilityAccess::AppCapabilityAccessStatus;
use ::windows::Win32::Foundation::RECT;
use ::windows::Win32::Graphics::Direct3D11::{ID3D11Texture2D, D3D11_TEXTURE2D_DESC};
use ::windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIFactory1};
use ::windows::Win32::Graphics::Gdi::HMONITOR;
use ::windows::Win32::System::WinRT::Direct3D11::IDirect3DDxgiInterfaceAccess;
use ::windows::Win32::System::WinRT::Graphics::Capture::IGraphicsCaptureItemInterop;
use ::windows::Win32::System::WinRT::{RoInitialize, RO_INIT_MULTITHREADED};
use ::windows::Win32::UI::HiDpi::{
    SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use serde_json::{json, Value};

use super::{capture_error, gpu, lock, qpc_now_ns, window};
use crate::backend::Failure;
use crate::error::{Code, RecordError};
use crate::pacer::{Decision, Pacer};
use crate::rkv::{LosslessWriter, MAX_SIDE};
use crate::screen::{
    MonitorInfo, ScreenBackend, ScreenEvent, ScreenFinished, ScreenSpec, ScreenStart, ScreenTarget,
    VIDEO_FILE,
};

/// Staging slots per monitor. Each in-flight frame holds one slot.
const SLOTS: usize = 6;

/// A monitor as DXGI lists it.
struct Output {
    monitor: HMONITOR,
    rect: RECT,
}

impl Output {
    fn width(&self) -> u32 {
        (self.rect.right - self.rect.left).max(0) as u32
    }

    fn height(&self) -> u32 {
        (self.rect.bottom - self.rect.top).max(0) as u32
    }

    fn primary(&self) -> bool {
        self.rect.left == 0 && self.rect.top == 0
    }
}

/// The monitors attached to the desktop: the primary first, then left to right, top to bottom.
fn outputs() -> Result<Vec<Output>, RecordError> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }.map_err(|e| {
        RecordError::new(Code::Unsupported, format!("CreateDXGIFactory1 failed: {e}"))
    })?;
    let mut found: Vec<Output> = Vec::new();
    let mut adapter_index = 0u32;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(adapter_index) } {
        adapter_index += 1;
        let mut output_index = 0u32;
        while let Ok(output) = unsafe { adapter.EnumOutputs(output_index) } {
            output_index += 1;
            let Ok(desc) = (unsafe { output.GetDesc() }) else {
                continue;
            };
            if !desc.AttachedToDesktop.as_bool() || found.iter().any(|o| o.monitor == desc.Monitor)
            {
                continue;
            }
            found.push(Output {
                monitor: desc.Monitor,
                rect: desc.DesktopCoordinates,
            });
        }
    }
    found.sort_by_key(|o| (!o.primary(), o.rect.left, o.rect.top));
    Ok(found)
}

fn infos(outputs: &[Output]) -> Vec<MonitorInfo> {
    outputs
        .iter()
        .enumerate()
        .map(|(index, o)| MonitorInfo {
            index: index as u32,
            x: o.rect.left,
            y: o.rect.top,
            width: o.width(),
            height: o.height(),
            primary: o.primary(),
        })
        .collect()
}

/// One monitor's capture objects and the callback token once registered.
struct MonitorCapture {
    pool: Direct3D11CaptureFramePool,
    session: GraphicsCaptureSession,
    token: Option<i64>,
}

/// One monitor's staging ring and its place on the canvas.
struct MonitorState {
    free_rx: Mutex<Receiver<usize>>,
    free_tx: Sender<usize>,
    readback: Mutex<gpu::Readback>,
    width: u32,
    height: u32,
    /// Top left corner on the canvas, in pixels.
    x: u32,
    y: u32,
}

#[derive(Clone, Copy)]
struct Msg {
    monitor: usize,
    slot: usize,
    host_ns: u64,
}

struct Shared {
    stopped: AtomicBool,
    arrived: AtomicU64,
    backpressure_drops: AtomicU64,
    size_mismatch_drops: AtomicU64,
    copy_failures: AtomicU64,
    frames_tx: Mutex<Option<SyncSender<Msg>>>,
    monitors: Vec<MonitorState>,
}

impl Shared {
    /// Frames the callbacks lost so far. Monotonic.
    fn lost(&self) -> u64 {
        self.backpressure_drops.load(Ordering::Relaxed)
            + self.size_mismatch_drops.load(Ordering::Relaxed)
            + self.copy_failures.load(Ordering::Relaxed)
    }
}

/// What the worker reports when it ends.
struct WorkerOutput {
    clean: bool,
    frames: u64,
    dropped: u64,
    summary: Value,
}

/// A running capture.
struct Running {
    captures: Vec<MonitorCapture>,
    shared: Arc<Shared>,
    worker: JoinHandle<WorkerOutput>,
    border: Value,
}

impl Running {
    /// Stops the callbacks, lets the worker drain and finalize the file, and returns what it
    /// reports. Safe on every path, including a start that failed half way.
    fn shutdown(self) -> (WorkerOutput, Value) {
        let Running {
            captures,
            shared,
            worker,
            border,
        } = self;
        shared.stopped.store(true, Ordering::Release);
        for capture in captures {
            if let Some(token) = capture.token {
                let _ = capture.pool.RemoveFrameArrived(token);
            }
            let _ = capture.session.Close();
            let _ = capture.pool.Close();
        }
        *lock(&shared.frames_tx) = None;
        let output = worker.join().unwrap_or_else(|_| WorkerOutput {
            clean: false,
            frames: 0,
            dropped: 0,
            summary: json!({ "error": "worker thread panicked" }),
        });
        (output, border)
    }
}

/// The Windows side of [`ScreenBackend`]. One take at a time.
pub struct WindowsScreen {
    running: Option<Running>,
    events: Option<Receiver<ScreenEvent>>,
}

impl Default for WindowsScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl WindowsScreen {
    pub fn new() -> Self {
        Self {
            running: None,
            events: None,
        }
    }
}

impl ScreenBackend for WindowsScreen {
    fn monitors(&self) -> Result<Vec<MonitorInfo>, RecordError> {
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        Ok(infos(&outputs()?))
    }

    fn start(&mut self, spec: &ScreenSpec) -> Result<ScreenStart, RecordError> {
        if self.running.is_some() {
            return Err(RecordError::new(
                Code::AlreadyRecording,
                "the Windows screen backend already has a take",
            ));
        }
        let _ =
            unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let listed = outputs()?;
        let selected: Vec<(usize, &Output)> = match spec.target {
            ScreenTarget::Monitor(index) => {
                let found = listed.get(index as usize).ok_or_else(|| {
                    RecordError::new(
                        Code::NotFound,
                        format!("no monitor {index}; {} are attached", listed.len()),
                    )
                })?;
                vec![(index as usize, found)]
            }
            ScreenTarget::Virtual => listed.iter().enumerate().collect(),
        };
        if selected.is_empty() {
            return Err(RecordError::new(Code::NotFound, "no monitor is attached"));
        }

        let _ = unsafe { RoInitialize(RO_INIT_MULTITHREADED) };
        let access =
            GraphicsCaptureAccess::RequestAccessAsync(GraphicsCaptureAccessKind::Borderless)
                .and_then(|operation| operation.join())
                .map_err(|e| {
                    capture_error(Code::Unsupported, "graphics capture access request", e)
                })?;
        let interop = factory::<GraphicsCaptureItem, IGraphicsCaptureItemInterop>()
            .map_err(|e| capture_error(Code::Unsupported, "GraphicsCaptureItem interop", e))?;

        // One item per monitor; its size, not the desktop rectangle, is what the texture will be.
        let mut items: Vec<(
            GraphicsCaptureItem,
            ::windows::Graphics::SizeInt32,
            i32,
            i32,
        )> = Vec::new();
        for (_, output) in &selected {
            let item: GraphicsCaptureItem = unsafe { interop.CreateForMonitor(output.monitor) }
                .map_err(|e| capture_error(Code::Helper, "CreateForMonitor", e))?;
            let size = item
                .Size()
                .map_err(|e| capture_error(Code::Helper, "item size", e))?;
            if size.Width <= 0 || size.Height <= 0 {
                return Err(RecordError::new(
                    Code::Helper,
                    "a monitor reported an empty capture size",
                ));
            }
            items.push((item, size, output.rect.left, output.rect.top));
        }

        // The canvas is the union of the monitors; each monitor sits at its offset from its corner.
        let left = items.iter().map(|(_, _, x, _)| *x).min().unwrap_or(0);
        let top = items.iter().map(|(_, _, _, y)| *y).min().unwrap_or(0);
        let right = items
            .iter()
            .map(|(_, size, x, _)| *x + size.Width)
            .max()
            .unwrap_or(0);
        let bottom = items
            .iter()
            .map(|(_, size, _, y)| *y + size.Height)
            .max()
            .unwrap_or(0);
        let (canvas_width, canvas_height) = ((right - left) as u32, (bottom - top) as u32);
        if canvas_width > MAX_SIDE || canvas_height > MAX_SIDE {
            return Err(RecordError::new(
                Code::Unsupported,
                format!(
                    "the {canvas_width}x{canvas_height} canvas is over {MAX_SIDE} pixels a side"
                ),
            ));
        }

        let device = gpu::Device::create()?;
        let mut states = Vec::with_capacity(items.len());
        for (_, size, x, y) in &items {
            let (width, height) = (size.Width as u32, size.Height as u32);
            let (free_tx, free_rx) = mpsc::channel::<usize>();
            for slot in 0..SLOTS {
                let _ = free_tx.send(slot);
            }
            states.push(MonitorState {
                free_rx: Mutex::new(free_rx),
                free_tx,
                readback: Mutex::new(device.readback(width, height, SLOTS)?),
                width,
                height,
                x: (*x - left) as u32,
                y: (*y - top) as u32,
            });
        }

        let video = spec.out.join(VIDEO_FILE);
        let writer = LosslessWriter::create(&video, canvas_width, canvas_height)?;
        let (frames_tx, frames_rx) = mpsc::sync_channel::<Msg>(SLOTS * items.len());
        let (events_tx, events_rx) = mpsc::channel::<ScreenEvent>();
        let shared = Arc::new(Shared {
            stopped: AtomicBool::new(false),
            arrived: AtomicU64::new(0),
            backpressure_drops: AtomicU64::new(0),
            size_mismatch_drops: AtomicU64::new(0),
            copy_failures: AtomicU64::new(0),
            frames_tx: Mutex::new(Some(frames_tx)),
            monitors: states,
        });
        let worker = {
            let shared = Arc::clone(&shared);
            let fps = spec.fps;
            thread::spawn(move || {
                worker(
                    shared,
                    frames_rx,
                    fps,
                    events_tx,
                    writer,
                    canvas_width,
                    canvas_height,
                )
            })
        };

        let mut running = Running {
            captures: Vec::new(),
            shared: Arc::clone(&shared),
            worker,
            border: Value::Null,
        };
        let mut border_errors: Vec<String> = Vec::new();
        for (monitor, (item, size, _, _)) in items.iter().enumerate() {
            match start_monitor(&device, &shared, monitor, item, *size, access) {
                Ok((capture, border_error)) => {
                    running.captures.push(capture);
                    if let Some(error) = border_error {
                        border_errors.push(error);
                    }
                }
                Err(e) => {
                    let _ = running.shutdown();
                    let _ = std::fs::remove_file(&video);
                    return Err(e);
                }
            }
        }
        for border_error in &border_errors {
            eprintln!("rightkit-record-helper: warning: capture border stays on: {border_error}");
        }
        running.border = json!({
            "borderRequired": !border_errors.is_empty(),
            "borderlessAccess": format!("{access:?}"),
            "errors": border_errors,
        });

        let host_ns_at_start = qpc_now_ns();
        let mut started: Result<(), RecordError> = Ok(());
        for capture in &running.captures {
            if let Err(e) = capture.session.StartCapture() {
                started = Err(capture_error(Code::Helper, "StartCapture", e));
                break;
            }
        }
        if let Err(e) = started {
            let _ = running.shutdown();
            let _ = std::fs::remove_file(&video);
            return Err(e);
        }

        let monitors: Vec<MonitorInfo> = selected
            .iter()
            .zip(&items)
            .map(|((index, output), (_, size, _, _))| MonitorInfo {
                index: *index as u32,
                x: output.rect.left,
                y: output.rect.top,
                width: size.Width as u32,
                height: size.Height as u32,
                primary: output.primary(),
            })
            .collect();
        self.running = Some(running);
        self.events = Some(events_rx);
        Ok(ScreenStart {
            host_ns_at_start,
            width: canvas_width,
            height: canvas_height,
            clock: "qpc_ns",
            os_build: window::os_build(),
            monitors,
        })
    }

    fn host_ns(&self) -> Option<u64> {
        Some(qpc_now_ns())
    }

    fn poll(&mut self) -> Vec<ScreenEvent> {
        self.events
            .as_ref()
            .map(|rx| rx.try_iter().collect())
            .unwrap_or_default()
    }

    fn stop(&mut self) -> Result<ScreenFinished, RecordError> {
        let running = self.running.take().ok_or_else(|| {
            RecordError::new(
                Code::NotRecording,
                "the Windows screen backend has no take to stop",
            )
        })?;
        let (output, border) = running.shutdown();
        Ok(ScreenFinished {
            clean: output.clean,
            frames: output.frames,
            dropped: output.dropped,
            summary: json!({ "border": border, "worker": output.summary }),
        })
    }

    fn abort(&mut self) {
        if let Some(running) = self.running.take() {
            let _ = running.shutdown();
        }
    }
}

/// Builds the pool and session of one monitor and registers its callback. The returned string is
/// why the border stays on, when it does (cosmetic: the take continues).
fn start_monitor(
    device: &gpu::Device,
    shared: &Arc<Shared>,
    monitor: usize,
    item: &GraphicsCaptureItem,
    size: ::windows::Graphics::SizeInt32,
    access: AppCapabilityAccessStatus,
) -> Result<(MonitorCapture, Option<String>), RecordError> {
    let pool = Direct3D11CaptureFramePool::CreateFreeThreaded(
        device.winrt(),
        DirectXPixelFormat::B8G8R8A8UIntNormalized,
        2,
        size,
    )
    .map_err(|e| capture_error(Code::Helper, "CreateFreeThreaded", e))?;
    let session: GraphicsCaptureSession = pool
        .CreateCaptureSession(item)
        .map_err(|e| capture_error(Code::Helper, "CreateCaptureSession", e))?;
    let border_error = if access == AppCapabilityAccessStatus::Allowed {
        session
            .SetIsBorderRequired(false)
            .err()
            .map(|e| e.to_string())
    } else {
        Some(format!(
            "borderless access status is {access:?}, not Allowed"
        ))
    };
    session
        .SetIsCursorCaptureEnabled(false)
        .map_err(|e| capture_error(Code::Helper, "SetIsCursorCaptureEnabled", e))?;

    let callback_shared = Arc::clone(shared);
    let handler = TypedEventHandler::new(
        move |sender: Ref<'_, Direct3D11CaptureFramePool>, _: Ref<'_, IInspectable>| {
            if let Ok(pool) = sender.ok() {
                on_frame(&callback_shared, monitor, pool);
            }
            Ok(())
        },
    );
    let token = pool
        .FrameArrived(&handler)
        .map_err(|e| capture_error(Code::Helper, "FrameArrived", e))?;
    Ok((
        MonitorCapture {
            pool,
            session,
            token: Some(token),
        },
        border_error,
    ))
}

enum CopyError {
    SizeMismatch,
    Failed,
}

/// Runs in the FrameArrived callback. Copies and returns quickly, never encodes or writes.
fn on_frame(shared: &Shared, monitor: usize, pool: &Direct3D11CaptureFramePool) {
    let Ok(frame) = pool.TryGetNextFrame() else {
        return;
    };
    if shared.stopped.load(Ordering::Acquire) {
        let _ = frame.Close();
        return;
    }
    shared.arrived.fetch_add(1, Ordering::Relaxed);
    let state = &shared.monitors[monitor];
    let taken = lock(&state.free_rx).try_recv().ok();
    match taken {
        None => {
            shared.backpressure_drops.fetch_add(1, Ordering::Relaxed);
        }
        Some(slot) => match copy_frame(state, slot, &frame) {
            Ok(host_ns) => {
                let queued = match lock(&shared.frames_tx).as_ref() {
                    Some(tx) => tx
                        .try_send(Msg {
                            monitor,
                            slot,
                            host_ns,
                        })
                        .is_ok(),
                    None => false,
                };
                if !queued {
                    shared.backpressure_drops.fetch_add(1, Ordering::Relaxed);
                    let _ = state.free_tx.send(slot);
                }
            }
            Err(CopyError::SizeMismatch) => {
                shared.size_mismatch_drops.fetch_add(1, Ordering::Relaxed);
                let _ = state.free_tx.send(slot);
            }
            Err(CopyError::Failed) => {
                shared.copy_failures.fetch_add(1, Ordering::Relaxed);
                let _ = state.free_tx.send(slot);
            }
        },
    }
    let _ = frame.Close();
}

fn copy_frame(
    state: &MonitorState,
    slot: usize,
    frame: &Direct3D11CaptureFrame,
) -> Result<u64, CopyError> {
    let ticks = frame
        .SystemRelativeTime()
        .map_err(|_| CopyError::Failed)?
        .Duration;
    if ticks < 0 {
        return Err(CopyError::Failed);
    }
    let surface = frame.Surface().map_err(|_| CopyError::Failed)?;
    let access: IDirect3DDxgiInterfaceAccess = surface.cast().map_err(|_| CopyError::Failed)?;
    let texture: ID3D11Texture2D =
        unsafe { access.GetInterface() }.map_err(|_| CopyError::Failed)?;
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    unsafe { texture.GetDesc(&mut desc) };
    if desc.Width != state.width || desc.Height != state.height {
        return Err(CopyError::SizeMismatch);
    }
    lock(&state.readback).copy_in(slot, &texture);
    // SystemRelativeTime is in 100 ns ticks; the file carries nanoseconds.
    Ok(ticks as u64 * 100)
}

/// Worker thread: reads each slot back, blits it onto the canvas, and appends the canvas to the
/// file when the pacer keeps the frame. The file is finalized on every exit path.
fn worker(
    shared: Arc<Shared>,
    rx: Receiver<Msg>,
    fps: u32,
    events: Sender<ScreenEvent>,
    mut writer: LosslessWriter,
    canvas_width: u32,
    canvas_height: u32,
) -> WorkerOutput {
    let canvas_stride = canvas_width as usize * 4;
    let mut canvas = vec![0u8; canvas_stride * canvas_height as usize];
    let mut scratch: Vec<u8> = Vec::new();
    let mut pacer = Pacer::new(fps);
    let mut seen_lost = 0u64;
    let mut dropped = 0u64;
    let mut last_ns = 0u64;
    let mut failed = false;
    let mut clean = true;
    for msg in rx {
        let state = &shared.monitors[msg.monitor];
        if !failed {
            // Every arrival updates the canvas, kept or not, so a skipped frame is never lost
            // content: the next kept frame carries it.
            let read = lock(&state.readback).read_packed(msg.slot, &mut scratch);
            match read {
                Ok(()) => blit(&mut canvas, canvas_stride, state, &scratch),
                Err(err) => {
                    failed = true;
                    clean = false;
                    shared.stopped.store(true, Ordering::Release);
                    let _ = events.send(ScreenEvent::Failed(Failure::Io(err.message)));
                }
            }
        }
        if !failed {
            // Timestamps in the file never go back, even when two monitors' callbacks race.
            let host_ns = msg.host_ns.max(last_ns);
            let lost = shared.lost();
            if lost > seen_lost {
                let count = u32::try_from(lost - seen_lost).unwrap_or(u32::MAX);
                dropped += u64::from(count);
                let _ = events.send(ScreenEvent::Dropped { host_ns, count });
                seen_lost = lost;
            }
            if pacer.offer(host_ns as i64) == Decision::Keep {
                match writer.write_frame(host_ns, &canvas) {
                    Ok(()) => last_ns = host_ns,
                    Err(err) => {
                        failed = true;
                        clean = false;
                        shared.stopped.store(true, Ordering::Release);
                        let _ = events.send(ScreenEvent::Failed(Failure::Io(err.message)));
                    }
                }
            }
        }
        let _ = state.free_tx.send(msg.slot);
    }
    let frames = writer.frames();
    if let Err(err) = writer.finish() {
        clean = false;
        if !failed {
            let _ = events.send(ScreenEvent::Failed(Failure::Io(err.message)));
        }
    }
    WorkerOutput {
        clean,
        frames,
        dropped,
        summary: json!({
            "arrived": shared.arrived.load(Ordering::Relaxed),
            "losses": {
                "backpressure": shared.backpressure_drops.load(Ordering::Relaxed),
                "sizeMismatch": shared.size_mismatch_drops.load(Ordering::Relaxed),
                "copyFailure": shared.copy_failures.load(Ordering::Relaxed),
            },
        }),
    }
}

/// Copies one monitor's packed rows onto the canvas at the monitor's place.
fn blit(canvas: &mut [u8], canvas_stride: usize, state: &MonitorState, packed: &[u8]) {
    let row = state.width as usize * 4;
    let origin = state.x as usize * 4;
    for y in 0..state.height as usize {
        let from = y * row;
        let to = (state.y as usize + y) * canvas_stride + origin;
        let (Some(source), Some(target)) =
            (packed.get(from..from + row), canvas.get_mut(to..to + row))
        else {
            return;
        };
        target.copy_from_slice(source);
    }
}
