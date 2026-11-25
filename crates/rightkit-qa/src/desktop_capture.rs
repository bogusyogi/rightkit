//! Native-surface capture for visual journeys: window discovery, stills, frame sequences, a pointer
//! helper and PNG statistics.
//!
//! Overlays such as a floating capsule, hint or clipboard card are native surfaces, not the app's web
//! view, so the control server's `screenshot` verb cannot see them. This module captures them from the
//! desktop instead.
//!
//! macOS (an overlay is one AppKit panel):
//!  - windows are found with CoreGraphics `CGWindowListCopyWindowInfo`, read through `osascript -l
//!    JavaScript`, filtered to the app's own pid (`observe_windows`);
//!  - a still is `screencapture -x -o -l <window id>`: that window's own pixels (alpha kept, no shadow,
//!    no desktop behind it). A missing Screen Recording permission makes the command fail instead of
//!    returning a wallpaper picture, so a refusal is loud.
//!
//! Windows (overlays are layered native windows the app reports itself):
//!  - [`parse_overlay_rects`] turns the app's own report into [`Obs`]; a still or a sequence is a fixed
//!    screen region ([`fixed_region`]) captured with `Graphics.CopyFromScreen` in a hidden PowerShell.
//!    The region includes whatever is behind the overlay, so a journey proves the overlay is in it by
//!    requiring the captures of different states to differ.
//!
//! Sequences are timestamped frame folders. macOS spawns one `screencapture` per frame on a fixed fps
//! schedule, so each frame is stamped with its spawn and exit time; the state at capture lies between
//! the two. Windows runs one PowerShell loop that stamps each frame in epoch milliseconds.
//!
//! Nothing here changes the app or its data. The pure parts (geometry, [`summarise`], [`analyse_png`],
//! [`parse_overlay_rects`], [`fixed_region`]) build on every platform; discovery, stills, sequences and
//! the pointer need macOS or Windows and return an error elsewhere.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "macos", windows))]
use std::process::Command;
#[cfg(target_os = "macos")]
use std::sync::{Arc, Mutex};
#[cfg(any(target_os = "macos", windows))]
use std::time::{Duration, Instant};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};

/// A rectangle in screen points (macOS) or logical pixels (Windows), origin top-left.
#[derive(Clone, Copy, Debug)]
pub struct Rect {
    /// Left edge.
    pub x: f64,
    /// Top edge.
    pub y: f64,
    /// Width.
    pub w: f64,
    /// Height.
    pub h: f64,
}

impl Rect {
    /// The centre point.
    pub fn center(&self) -> (f64, f64) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }

    /// `{x, y, w, h}`.
    pub fn to_json(self) -> Value {
        json!({ "x": self.x, "y": self.y, "w": self.w, "h": self.h })
    }
}

/// One native window the app shows.
#[derive(Clone, Debug)]
pub struct Obs {
    /// A name for the window: the report key on Windows, `window-<id>` on macOS.
    pub label: String,
    /// The CoreGraphics window number on macOS; 0 on Windows.
    pub id: u64,
    /// Where it is on screen.
    pub rect: Rect,
    /// Backing scale as the app reports it (Windows); 0 on macOS, where it is measured from the capture.
    pub scale: f64,
}

/// What a window looks like for "has it changed": label and size rounded to whole points.
pub type Sig = Vec<(String, i64, i64)>;

/// The [`Sig`] of `obs`, sorted.
pub fn sig(obs: &[Obs]) -> Sig {
    let mut out: Sig = obs
        .iter()
        .map(|o| {
            (
                o.label.clone(),
                o.rect.w.round() as i64,
                o.rect.h.round() as i64,
            )
        })
        .collect();
    out.sort();
    out
}

/// Milliseconds since the Unix epoch.
pub fn now_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn num(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or(f64::NAN)
}

// ---------------------------------------------------------------------------------------------
// Window discovery
// ---------------------------------------------------------------------------------------------

/// Which of an app's windows `observe_windows` keeps (macOS).
#[derive(Clone, Copy, Debug)]
pub struct WindowFilter {
    /// Windows wider than this are the main window or a splash, not an overlay.
    pub max_width: f64,
    /// Windows taller than this are excluded likewise.
    pub max_height: f64,
    /// Windows whose bottom edge is at or above this y are in the menu bar band (the tray icon is a
    /// window of the same pid at the same level as an overlay).
    pub min_bottom: f64,
}

impl Default for WindowFilter {
    /// 720 by 560 points and a 60 point menu bar band.
    fn default() -> Self {
        WindowFilter {
            max_width: 720.0,
            max_height: 560.0,
            min_bottom: 60.0,
        }
    }
}

/// Whether a CoreGraphics window record is an overlay `filter` keeps: a known id, not fully
/// transparent, finite geometry of at least one point, no larger than the filter's size and not in
/// the menu bar band. Pure, so it is testable without a desktop.
pub fn keep_window(id: u64, alpha: f64, rect: Rect, filter: &WindowFilter) -> bool {
    !(id == 0
        || alpha <= 0.0
        || !(rect.x.is_finite() && rect.y.is_finite() && rect.w.is_finite() && rect.h.is_finite())
        || rect.w < 1.0
        || rect.h < 1.0
        || rect.w > filter.max_width
        || rect.h > filter.max_height
        || rect.y + rect.h <= filter.min_bottom)
}

/// Parse the JSON window list the macOS JXA script prints (`[{id, alpha, x, y, w, h}, ...]`) into
/// the windows `filter` keeps.
pub fn parse_window_list(text: &str, filter: &WindowFilter) -> Result<Vec<Obs>, String> {
    let list: Vec<Value> = serde_json::from_str(text.trim())
        .map_err(|e| format!("window list is not JSON ({e}): {}", text.trim()))?;
    let mut found = Vec::new();
    for w in list {
        let rect = Rect {
            x: num(&w, "x"),
            y: num(&w, "y"),
            w: num(&w, "w"),
            h: num(&w, "h"),
        };
        let alpha = w.get("alpha").and_then(Value::as_f64).unwrap_or(1.0);
        let id = w.get("id").and_then(Value::as_u64).unwrap_or(0);
        if !keep_window(id, alpha, rect, filter) {
            continue;
        }
        found.push(Obs {
            label: format!("window-{id}"),
            id,
            rect,
            scale: 0.0,
        });
    }
    Ok(found)
}

/// CoreGraphics window list for one pid, as JSON. Option 1 is on-screen only, 16 excludes desktop
/// elements; window 0 is the null window. Bounds are in points, origin top-left of the primary display.
#[cfg(target_os = "macos")]
const WINDOWS_JXA: &str = r#"
ObjC.import('CoreGraphics');
const all = ObjC.deepUnwrap(ObjC.castRefToObject($.CGWindowListCopyWindowInfo(1 | 16, 0)));
JSON.stringify(all.filter(w => w.kCGWindowOwnerPID === __PID__).map(w => ({
  id: w.kCGWindowNumber, layer: w.kCGWindowLayer, alpha: w.kCGWindowAlpha,
  x: w.kCGWindowBounds.X, y: w.kCGWindowBounds.Y, w: w.kCGWindowBounds.Width, h: w.kCGWindowBounds.Height
})));
"#;

/// macOS: the native overlay windows of process `pid` now, as `filter` keeps them. Use this when the
/// app has no report of its own for its native overlays.
#[cfg(target_os = "macos")]
pub fn observe_windows(pid: u32, filter: &WindowFilter) -> Result<Vec<Obs>, String> {
    let script = WINDOWS_JXA.replace("__PID__", &pid.to_string());
    let output = Command::new("osascript")
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("osascript: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "window list failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_window_list(&String::from_utf8_lossy(&output.stdout), filter)
}

/// Where an app's own overlay report keeps its windows ([`parse_overlay_rects`]).
#[derive(Clone, Debug)]
pub struct OverlayRectsSpec {
    /// Key of the primary overlay's window record (`{visible, x, y, scale}`).
    pub primary_key: String,
    /// Key of the primary overlay's hit box inside that record's report (`{x, y, w, h}`, relative to
    /// the window), or `None` to use the window record's own `x, y, w, h`.
    pub primary_hitbox_key: Option<String>,
    /// Keys of further overlays, each `{visible, x, y, w, h, scale}`.
    pub secondary_keys: Vec<String>,
}

/// Turn an app's overlay report into [`Obs`] (Windows route; pure, so it builds everywhere). The
/// primary overlay is reported only when visible and, with a hit-box key, when the hit box is an
/// object; its rect is the window origin plus the hit box. Secondary overlays are reported when they
/// are objects and visible. The label is the report key.
pub fn parse_overlay_rects(report: &Value, spec: &OverlayRectsSpec) -> Vec<Obs> {
    let visible = |v: &Value| v.get("visible").and_then(Value::as_bool).unwrap_or(false);
    let mut found = Vec::new();
    let primary = &report[spec.primary_key.as_str()];
    match &spec.primary_hitbox_key {
        Some(hit_key) => {
            let hit = &report[hit_key.as_str()];
            if visible(primary) && hit.is_object() {
                found.push(Obs {
                    label: spec.primary_key.clone(),
                    id: 0,
                    rect: Rect {
                        x: num(primary, "x") + num(hit, "x"),
                        y: num(primary, "y") + num(hit, "y"),
                        w: num(hit, "w"),
                        h: num(hit, "h"),
                    },
                    scale: num(primary, "scale"),
                });
            }
        }
        None => {
            if primary.is_object() && visible(primary) {
                found.push(Obs {
                    label: spec.primary_key.clone(),
                    id: 0,
                    rect: Rect {
                        x: num(primary, "x"),
                        y: num(primary, "y"),
                        w: num(primary, "w"),
                        h: num(primary, "h"),
                    },
                    scale: num(primary, "scale"),
                });
            }
        }
    }
    for key in &spec.secondary_keys {
        let w = &report[key.as_str()];
        if w.is_object() && visible(w) {
            found.push(Obs {
                label: key.clone(),
                id: 0,
                rect: Rect {
                    x: num(w, "x"),
                    y: num(w, "y"),
                    w: num(w, "w"),
                    h: num(w, "h"),
                },
                scale: num(w, "scale"),
            });
        }
    }
    found
}

/// Windows: the fixed region every capture uses, `width` by `height` logical pixels centred on the
/// overlay labelled `label` and ending `below` pixels under it, so every state is cropped
/// identically. Built from the settled idle overlay; returns the region and its backing scale.
pub fn fixed_region(
    idle: &[Obs],
    label: &str,
    width: f64,
    height: f64,
    below: f64,
) -> Option<(Rect, f64)> {
    let anchor = idle.iter().find(|o| o.label == label)?;
    let (cx, _) = anchor.rect.center();
    let bottom = anchor.rect.y + anchor.rect.h + below;
    let scale = if anchor.scale > 0.0 {
        anchor.scale
    } else {
        1.0
    };
    Some((
        Rect {
            x: (cx - width / 2.0).max(0.0),
            y: (bottom - height).max(0.0),
            w: width,
            h: height,
        },
        scale,
    ))
}

// ---------------------------------------------------------------------------------------------
// Pointer
// ---------------------------------------------------------------------------------------------

/// The pointer in the same coordinates as [`Rect`] (macOS points, origin top-left of the primary display).
#[cfg(target_os = "macos")]
pub fn pointer_position() -> Option<(f64, f64)> {
    let script = "ObjC.import('AppKit'); const l = $.NSEvent.mouseLocation; const h = $.NSScreen.screens.objectAtIndex(0).frame.size.height; JSON.stringify([l.x, h - l.y]);";
    let output = Command::new("osascript")
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    let v: Value = serde_json::from_str(String::from_utf8_lossy(&output.stdout).trim()).ok()?;
    Some((v.get(0)?.as_f64()?, v.get(1)?.as_f64()?))
}

/// The pointer in screen pixels (Windows).
#[cfg(windows)]
pub fn pointer_position() -> Option<(f64, f64)> {
    use windows_sys::Win32::Foundation::POINT;
    use windows_sys::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is a valid, writable POINT for the duration of the call.
    let ok = unsafe { GetCursorPos(&mut point) };
    (ok != 0).then(|| (f64::from(point.x), f64::from(point.y)))
}

/// No pointer read on this platform.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn pointer_position() -> Option<(f64, f64)> {
    None
}

/// Puts the real pointer back where it was when the guard was made, also when the journey panics.
pub struct PointerRestore(Option<(f64, f64)>);

impl PointerRestore {
    /// Remember the pointer now.
    pub fn remember() -> PointerRestore {
        PointerRestore(pointer_position())
    }
}

impl Drop for PointerRestore {
    fn drop(&mut self) {
        if let Some((x, y)) = self.0 {
            let _ = move_pointer(x, y);
        }
    }
}

/// Move the real pointer. macOS warps the cursor and posts a mouse-moved event, because a tracking
/// area reacts to events, not to a warp alone. This needs the Accessibility permission the other
/// macOS journeys use for System Events; it runs in an `osascript` child, which costs about 100 ms
/// before the pointer moves.
#[cfg(target_os = "macos")]
pub fn move_pointer(x: f64, y: f64) -> Result<(), String> {
    let script = format!(
        "ObjC.import('CoreGraphics'); const p = {{ x: {x}, y: {y} }}; $.CGWarpMouseCursorPosition(p); $.CGAssociateMouseAndMouseCursorPosition(1); $.CGEventPost(0, $.CGEventCreateMouseEvent(null, 5, p, 0)); 'ok';"
    );
    let output = Command::new("osascript")
        .arg("-l")
        .arg("JavaScript")
        .arg("-e")
        .arg(&script)
        .output()
        .map_err(|e| format!("osascript: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "pointer move failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// Move the real cursor (Windows), then let hover handling see it.
#[cfg(windows)]
pub fn move_pointer(x: f64, y: f64) -> Result<(), String> {
    crate::desktop_win::move_to(x as i32, y as i32);
    Ok(())
}

/// No pointer move on this platform.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn move_pointer(_x: f64, _y: f64) -> Result<(), String> {
    Err("pointer control needs macOS or Windows".into())
}

// ---------------------------------------------------------------------------------------------
// Stills and sequences
// ---------------------------------------------------------------------------------------------

/// What to capture: the overlay windows by id (macOS) or one fixed screen region (Windows).
#[derive(Clone, Debug)]
pub struct Grabber {
    /// CoreGraphics window numbers (macOS).
    pub windows: Vec<u64>,
    /// The screen region (Windows).
    pub region: Rect,
    /// Backing scale of the region (Windows); 0 means 1.
    pub scale: f64,
}

impl Grabber {
    /// macOS: every window of `obs`. Windows: `fixed` (region and scale from [`fixed_region`]).
    pub fn new(obs: &[Obs], fixed: Option<(Rect, f64)>) -> Grabber {
        let (region, scale) = fixed.unwrap_or_else(|| {
            let first = obs.first().map(|o| o.rect).unwrap_or(Rect {
                x: 0.0,
                y: 0.0,
                w: 0.0,
                h: 0.0,
            });
            (first, 0.0)
        });
        Grabber {
            windows: obs.iter().map(|o| o.id).collect(),
            region,
            scale,
        }
    }

    /// One capture. macOS writes `path` for the first window and `<stem>-w<k>.png` for any further
    /// one; Windows writes `path`. Returns every file written.
    pub fn still(&self, path: &Path) -> Result<Vec<PathBuf>, String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        grab_once(self, path)
    }
}

#[cfg(target_os = "macos")]
fn numbered(path: &Path, k: usize) -> PathBuf {
    if k == 0 {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!("{stem}-w{k}.png"))
}
#[cfg(target_os = "macos")]
fn grab_window(id: u64, path: &Path) -> Result<(), String> {
    let output = Command::new("screencapture")
        .arg("-x")
        .arg("-o")
        .arg("-l")
        .arg(id.to_string())
        .arg(path)
        .output()
        .map_err(|e| format!("screencapture: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "screencapture -l {id} exited {}: {} (Screen Recording permission for the test runner?)",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() > 0 => Ok(()),
        _ => Err(format!(
            "screencapture -l {id} wrote no file (window closed, or no Screen Recording permission)"
        )),
    }
}

#[cfg(target_os = "macos")]
fn grab_once(grabber: &Grabber, path: &Path) -> Result<Vec<PathBuf>, String> {
    if grabber.windows.is_empty() {
        return Err("no overlay window to capture".into());
    }
    let mut files = Vec::new();
    for (k, id) in grabber.windows.iter().enumerate() {
        let file = numbered(path, k);
        grab_window(*id, &file)?;
        files.push(file);
    }
    Ok(files)
}

/// No capture on this platform.
#[cfg(not(any(target_os = "macos", windows)))]
fn grab_once(_grabber: &Grabber, _path: &Path) -> Result<Vec<PathBuf>, String> {
    Err("desktop capture needs macOS or Windows".into())
}

/// The PowerShell helper: a region capture in physical pixels. With `-Single 1` it writes `-Out` and
/// exits; otherwise it captures every `-IntervalMs` into `-Dir` as `frame-0001.png`..., logs
/// `index,file,epoch_ms_before,epoch_ms_after_copy` to `frames.csv`, signals `ready.txt` once set up
/// and stops when `stop.txt` appears.
#[cfg(windows)]
const GRAB_PS1: &str = r#"
param([int]$X, [int]$Y, [int]$W, [int]$H, [string]$Dir, [string]$Out, [int]$IntervalMs, [int]$Single)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition 'using System.Runtime.InteropServices; public static class QaDpi { [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }'
[void][QaDpi]::SetProcessDPIAware()
function Grab([string]$path) {
  $bmp = New-Object System.Drawing.Bitmap($W, $H)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.CopyFromScreen($X, $Y, 0, 0, $bmp.Size)
  $t = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  $g.Dispose()
  $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
  $bmp.Dispose()
  return $t
}
if ($Single -eq 1) { [void](Grab $Out); exit 0 }
$sw = [Diagnostics.Stopwatch]::StartNew()
$log = New-Object System.IO.StreamWriter((Join-Path $Dir 'frames.csv'))
$log.AutoFlush = $true
Set-Content -Path (Join-Path $Dir 'ready.txt') -Value 'ready'
$i = 0
while (-not (Test-Path (Join-Path $Dir 'stop.txt'))) {
  $i++
  $before = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  $name = 'frame-{0:0000}.png' -f $i
  $t = Grab (Join-Path $Dir $name)
  $log.WriteLine("$i,$name,$before,$t")
  $wait = ($i * $IntervalMs) - $sw.ElapsedMilliseconds
  if ($wait -gt 0) { Start-Sleep -Milliseconds $wait }
}
$log.Close()
"#;

#[cfg(windows)]
/// `pipe_stderr` keeps PowerShell's error text for a caller that waits with `wait_with_output`; a caller that
/// only polls the child must pass `false`, or a full pipe would block it.
fn run_ps1(dir: &Path, args: &[String], pipe_stderr: bool) -> Result<std::process::Child, String> {
    use std::os::windows::process::CommandExt;
    let script = dir.join("grab.ps1");
    std::fs::write(&script, GRAB_PS1).map_err(|e| format!("write grab.ps1: {e}"))?;
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&script)
        .args(args)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW: no console flashes on the owner's screen
        .stdout(std::process::Stdio::null())
        .stderr(if pipe_stderr {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .spawn()
        .map_err(|e| format!("powershell: {e}"))
}

/// Windows: region in physical pixels.
#[cfg(windows)]
fn physical(grabber: &Grabber) -> (i64, i64, i64, i64) {
    let s = if grabber.scale > 0.0 {
        grabber.scale
    } else {
        1.0
    };
    (
        (grabber.region.x * s).round() as i64,
        (grabber.region.y * s).round() as i64,
        (grabber.region.w * s).round() as i64,
        (grabber.region.h * s).round() as i64,
    )
}

#[cfg(windows)]
fn grab_once(grabber: &Grabber, path: &Path) -> Result<Vec<PathBuf>, String> {
    let (x, y, w, h) = physical(grabber);
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let args = vec![
        "-X".to_string(),
        x.to_string(),
        "-Y".to_string(),
        y.to_string(),
        "-W".to_string(),
        w.to_string(),
        "-H".to_string(),
        h.to_string(),
        "-Dir".to_string(),
        dir.to_string_lossy().into_owned(),
        "-Out".to_string(),
        path.to_string_lossy().into_owned(),
        "-IntervalMs".to_string(),
        "0".to_string(),
        "-Single".to_string(),
        "1".to_string(),
    ];
    let child = run_ps1(dir, &args, true)?;
    let output = child
        .wait_with_output()
        .map_err(|e| format!("powershell: {e}"))?;
    if !output.status.success() || !path.is_file() {
        return Err(format!(
            "region capture failed ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(vec![path.to_path_buf()])
}
/// One frame of a sequence.
#[derive(Clone, Debug)]
pub struct FrameRec {
    /// 1-based frame number.
    pub index: usize,
    /// File name inside the sequence folder (the first window's file on macOS).
    pub file: String,
    /// Milliseconds from the moment the trigger was sent to the capture's start (negative before it).
    pub start_ms: i64,
    /// Milliseconds from the moment the trigger was sent to the capture's end.
    pub end_ms: i64,
    /// Why this frame was not captured.
    pub error: Option<String>,
}

/// A captured sequence before verification.
pub struct SeqResult {
    /// Every frame attempted, in order.
    pub frames: Vec<FrameRec>,
    /// When the trigger was sent: always 0, the origin of the frame times.
    pub trigger_sent_ms: i64,
    /// When the trigger returned, in milliseconds from the moment it was sent.
    pub trigger_returned_ms: i64,
    /// The trigger's error, if it failed.
    pub trigger_error: Option<String>,
}

/// Capture frames at `fps` from `pre_ms` before the trigger to `post_ms` after it, and run `trigger`
/// between. `trigger` is called on the calling thread once `pre_ms` has passed. Frame times in the result
/// are relative to the moment the trigger was sent (negative before it).
#[cfg(target_os = "macos")]
pub fn sequence(
    grabber: &Grabber,
    dir: &Path,
    pre_ms: u64,
    post_ms: u64,
    fps: u32,
    trigger: impl FnOnce() -> Result<(), String>,
) -> Result<SeqResult, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    if grabber.windows.is_empty() {
        return Err("no overlay window to capture".into());
    }
    let start = Instant::now();
    let total = pre_ms + post_ms;
    let interval = 1000.0 / f64::from(fps.max(1));
    let frames: Arc<Mutex<Vec<FrameRec>>> = Arc::new(Mutex::new(Vec::new()));
    let windows = grabber.windows.clone();
    let dir_owned = dir.to_path_buf();
    let schedule_frames = Arc::clone(&frames);
    let scheduler = std::thread::spawn(move || {
        let mut workers = Vec::new();
        let mut index = 0usize;
        loop {
            let due_ms = (index as f64 * interval) as u64;
            if due_ms > total {
                break;
            }
            let due = start + Duration::from_millis(due_ms);
            let now = Instant::now();
            if due > now {
                std::thread::sleep(due - now);
            }
            index += 1;
            let windows = windows.clone();
            let dir = dir_owned.clone();
            let sink = Arc::clone(&schedule_frames);
            let this = index;
            workers.push(std::thread::spawn(move || {
                let started = start.elapsed().as_millis() as i64;
                let file = dir.join(format!("frame-{this:04}.png"));
                let mut error = None;
                for (k, id) in windows.iter().enumerate() {
                    if let Err(e) = grab_window(*id, &numbered(&file, k)) {
                        error = Some(e);
                        break;
                    }
                }
                let ended = start.elapsed().as_millis() as i64;
                sink.lock().unwrap().push(FrameRec {
                    index: this,
                    file: format!("frame-{this:04}.png"),
                    start_ms: started,
                    end_ms: ended,
                    error,
                });
            }));
        }
        for worker in workers {
            let _ = worker.join();
        }
    });

    std::thread::sleep(Duration::from_millis(pre_ms));
    let sent = start.elapsed().as_millis() as i64;
    let outcome = trigger();
    let returned = start.elapsed().as_millis() as i64;
    scheduler
        .join()
        .map_err(|_| "frame scheduler panicked".to_string())?;

    let mut frames = frames.lock().unwrap().clone();
    frames.sort_by_key(|f| f.index);
    for frame in &mut frames {
        frame.start_ms -= sent;
        frame.end_ms -= sent;
    }
    Ok(SeqResult {
        frames,
        trigger_sent_ms: 0,
        trigger_returned_ms: returned - sent,
        trigger_error: outcome.err(),
    })
}
/// Owns the PowerShell frame loop: on drop (including a panic in the trigger) it writes `stop.txt`,
/// waits a bounded time for the child to exit, then kills and reaps it.
#[cfg(windows)]
struct PsGuard {
    child: std::process::Child,
    stop: PathBuf,
}

#[cfg(windows)]
impl PsGuard {
    /// Ask the loop to stop and wait up to `bound` for it; kill it when it does not exit.
    fn stop_and_wait(&mut self, bound: Duration) {
        let _ = std::fs::write(&self.stop, "stop");
        self.wait_or_kill(bound);
    }

    fn wait_or_kill(&mut self, bound: Duration) {
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) if started.elapsed() < bound => {
                    std::thread::sleep(Duration::from_millis(50));
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return;
                }
            }
        }
    }
}

#[cfg(windows)]
impl Drop for PsGuard {
    fn drop(&mut self) {
        self.stop_and_wait(Duration::from_secs(5));
    }
}

#[cfg(windows)]
pub fn sequence(
    grabber: &Grabber,
    dir: &Path,
    pre_ms: u64,
    post_ms: u64,
    fps: u32,
    trigger: impl FnOnce() -> Result<(), String>,
) -> Result<SeqResult, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for stale in ["ready.txt", "stop.txt", "frames.csv"] {
        let _ = std::fs::remove_file(dir.join(stale));
    }
    let (x, y, w, h) = physical(grabber);
    let interval = (1000 / fps.max(1)) as i64;
    let args = vec![
        "-X".to_string(),
        x.to_string(),
        "-Y".to_string(),
        y.to_string(),
        "-W".to_string(),
        w.to_string(),
        "-H".to_string(),
        h.to_string(),
        "-Dir".to_string(),
        dir.to_string_lossy().into_owned(),
        "-Out".to_string(),
        // A placeholder, never an empty string: Windows PowerShell 5.1 can drop an empty argument
        // under -File. The script reads $Out only for a single still.
        "none".to_string(),
        "-IntervalMs".to_string(),
        interval.to_string(),
        "-Single".to_string(),
        "0".to_string(),
    ];
    let mut guard = PsGuard {
        child: run_ps1(dir, &args, false)?,
        stop: dir.join("stop.txt"),
    };
    // PowerShell compiles its helper before the first frame; start the clock only once it is ready.
    let ready = Instant::now();
    while !dir.join("ready.txt").is_file() {
        if ready.elapsed() > Duration::from_secs(30) {
            return Err("the capture script never became ready".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(Duration::from_millis(pre_ms));
    let sent_epoch = now_unix_ms() as i64;
    let sent = Instant::now();
    let outcome = trigger();
    let returned = sent.elapsed().as_millis() as i64;
    let elapsed = sent.elapsed();
    if elapsed < Duration::from_millis(post_ms) {
        std::thread::sleep(Duration::from_millis(post_ms) - elapsed);
    }
    if let Err(e) = std::fs::write(&guard.stop, "stop") {
        guard.wait_or_kill(Duration::ZERO);
        return Err(format!("stop file: {e}"));
    }
    guard.wait_or_kill(Duration::from_secs(15));
    let csv = std::fs::read_to_string(dir.join("frames.csv")).unwrap_or_default();
    let mut frames = Vec::new();
    for line in csv.lines() {
        let parts: Vec<&str> = line.trim().split(',').collect();
        if parts.len() < 4 {
            continue;
        }
        let (Ok(index), Ok(before), Ok(after)) = (
            parts[0].parse::<usize>(),
            parts[2].parse::<i64>(),
            parts[3].parse::<i64>(),
        ) else {
            continue;
        };
        frames.push(FrameRec {
            index,
            file: parts[1].to_string(),
            start_ms: before - sent_epoch,
            end_ms: after - sent_epoch,
            error: None,
        });
    }
    Ok(SeqResult {
        frames,
        trigger_sent_ms: 0,
        trigger_returned_ms: returned,
        trigger_error: outcome.err(),
    })
}

/// No sequence capture on this platform.
#[cfg(not(any(target_os = "macos", windows)))]
pub fn sequence(
    _grabber: &Grabber,
    _dir: &Path,
    _pre_ms: u64,
    _post_ms: u64,
    _fps: u32,
    _trigger: impl FnOnce() -> Result<(), String>,
) -> Result<SeqResult, String> {
    Err("desktop capture needs macOS or Windows".into())
}

/// Verified sequence: the manifest and the facts the journey asserts on.
pub struct SeqSummary {
    /// The manifest the caller writes beside the frames.
    pub manifest: Value,
    /// Every frame file that decoded as a PNG.
    pub files: Vec<PathBuf>,
    /// Frames that decoded and are not blank.
    pub valid_frames: usize,
    /// Frames that decoded but are blank ([`PngStats::is_blank`] with 2 colours).
    pub blank_frames: usize,
    /// Number of distinct frame file hashes.
    pub distinct_frames: usize,
    /// Frames per second over the valid frames' capture starts.
    pub achieved_fps: f64,
}

/// Decode every frame, hash it and write nothing; the caller writes `manifest` beside the frames.
pub fn summarise(
    name: &str,
    dir: &Path,
    result: &SeqResult,
    target_fps: u32,
    pre_ms: u64,
    post_ms: u64,
) -> SeqSummary {
    let mut rows = Vec::new();
    let mut files = Vec::new();
    let mut hashes = HashSet::new();
    let (mut valid, mut blank) = (0usize, 0usize);
    let mut starts = Vec::new();
    for frame in &result.frames {
        let path = dir.join(&frame.file);
        let mut row = json!({
            "index": frame.index,
            "file": frame.file,
            "ms_since_trigger": frame.start_ms,
            "capture_end_ms": frame.end_ms,
        });
        if let Some(error) = &frame.error {
            row["error"] = json!(error);
            rows.push(row);
            continue;
        }
        match std::fs::read(&path) {
            Ok(bytes) => {
                row["hash"] = json!(format!("{:016x}", fnv64(&bytes)));
                hashes.insert(fnv64(&bytes));
                match analyse_png(&bytes) {
                    Ok(stats) => {
                        row["png"] = stats.to_json();
                        if stats.is_blank(2) {
                            blank += 1;
                            row["blank"] = json!(true);
                        } else {
                            valid += 1;
                            starts.push(frame.start_ms);
                        }
                        files.push(path);
                    }
                    Err(e) => {
                        row["error"] = json!(format!("not a readable PNG: {e}"));
                    }
                }
            }
            Err(e) => row["error"] = json!(format!("read: {e}")),
        }
        rows.push(row);
    }
    let achieved_fps = if starts.len() >= 2 {
        let span = (starts[starts.len() - 1] - starts[0]).max(1) as f64;
        (starts.len() - 1) as f64 * 1000.0 / span
    } else {
        0.0
    };
    let manifest = json!({
        "name": name,
        "method": if cfg!(windows) { "powershell CopyFromScreen loop, fixed region" } else { "screencapture -l <window id>, one process per frame" },
        "target_fps": target_fps,
        "pre_ms": pre_ms,
        "post_ms": post_ms,
        "trigger_sent_ms": result.trigger_sent_ms,
        "trigger_returned_ms": result.trigger_returned_ms,
        "trigger_error": result.trigger_error,
        "note": "ms_since_trigger is the start of each capture relative to the moment the trigger was sent; the overlay state captured lies between that and capture_end_ms. Negative values precede the trigger.",
        "frames_attempted": result.frames.len(),
        "frames_valid": valid,
        "frames_blank": blank,
        "distinct_frames": hashes.len(),
        "achieved_fps": achieved_fps,
        "frames": rows,
    });
    SeqSummary {
        manifest,
        files,
        valid_frames: valid,
        blank_frames: blank,
        distinct_frames: hashes.len(),
        achieved_fps,
    }
}

/// FNV-1a 64 of `bytes`, to tell whether two captures are byte-identical.
pub fn fnv64(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Colour statistics of a decoded PNG.
#[derive(Clone, Debug)]
pub struct PngStats {
    /// Width in pixels.
    pub width: u32,
    /// Height in pixels.
    pub height: u32,
    /// Fraction of pixels with alpha above zero (1 for an image without alpha).
    pub visible_fraction: f64,
    /// Distinct RGBA values, counted up to 4096.
    pub distinct: usize,
}

impl PngStats {
    /// Blank: almost nothing visible, or fewer than `min_distinct` colours.
    pub fn is_blank(&self, min_distinct: usize) -> bool {
        self.visible_fraction < 0.02 || self.distinct < min_distinct
    }

    /// `{width, height, visible_fraction, distinct_colours}`.
    pub fn to_json(&self) -> Value {
        json!({
            "width": self.width,
            "height": self.height,
            "visible_fraction": self.visible_fraction,
            "distinct_colours": self.distinct,
        })
    }
}

/// Decode a PNG (through [`crate::media::decode_png_bytes`]: any bit depth or colour type the `png`
/// crate reads, 16-bit reduced to 8) and measure it. A PNG that does not decode is an error, never a pass.
pub fn analyse_png(bytes: &[u8]) -> Result<PngStats, String> {
    let (width, height, rgba) = crate::media::decode_png_bytes(bytes).map_err(|e| e.0)?;
    let total = (width as usize) * (height as usize);
    if total == 0 {
        return Err("PNG has no pixels".into());
    }
    let mut seen: HashSet<u32> = HashSet::new();
    let mut visible = 0usize;
    for px in rgba.as_chunks::<4>().0 {
        if px[3] > 0 {
            visible += 1;
        }
        if seen.len() < 4096 {
            seen.insert(u32::from_be_bytes([px[0], px[1], px[2], px[3]]));
        }
    }
    Ok(PngStats {
        width,
        height,
        visible_fraction: visible as f64 / total as f64,
        distinct: seen.len(),
    })
}
