//! Tray state presentation: a per-state icon set read from the bundle, an alert tooltip override,
//! and a click listener for a tray whose left click the host handles itself
//! (`TraySpec::left_click = LeftClick::Emit`).
//!
//! Donor: HeardRight `tray.rs`. The mapping from an app's own state to a state name, the menu, its
//! labels and what a click does stay with the host; this module is the mechanism.
//!
//! Bounds: a state name is `[A-Za-z0-9_-]{1,32}`; an icon file name pattern and sub-directory are
//! relative and free of separators and `..`; an icon file is opened once and read through a
//! [`MAX_ICON_BYTES`] cap from that handle (never a metadata check followed by a reopen), and its
//! PNG or ICO header dimensions must be within [`MAX_ICON_DIMENSION`] and [`MAX_ICON_PIXELS`]
//! before any decoder sees the bytes; a tray event payload larger than [`MAX_EVENT_PAYLOAD`] is
//! ignored.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::image::Image;
use tauri::{AppHandle, Listener, Manager, Runtime};

use crate::hub_window::EventSink;

/// Largest icon file read from disk.
pub const MAX_ICON_BYTES: u64 = 1 << 20;
/// Largest icon width or height, in pixels (the ICO format's own maximum).
pub const MAX_ICON_DIMENSION: u32 = 256;
/// Most pixels an icon may declare.
pub const MAX_ICON_PIXELS: u64 = 65_536;
/// Largest tray event payload parsed.
pub const MAX_EVENT_PAYLOAD: usize = 4096;
const MAX_STATE_LEN: usize = 32;
const PLACEHOLDER: &str = "{state}";

/// Where a tray's per-state icons live and how they are named. `{state}` stands for the state
/// name in each pattern (once).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconSet {
    /// Sub-directory under the resource directory and beside the executable (`icons/tray`).
    pub subdir: String,
    /// File name pattern on macOS (`tray-{state}-32@2x.png`).
    pub file_pattern_macos: String,
    /// File name pattern elsewhere (`tray-{state}-32.ico`).
    pub file_pattern_other: String,
}

fn state_ok(state: &str) -> bool {
    !state.is_empty()
        && state.len() <= MAX_STATE_LEN
        && state
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
}

fn component_ok(part: &str) -> bool {
    !part.is_empty()
        && part != ".."
        && part != "."
        && part.len() <= 64
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'))
}

impl IconSet {
    /// Check the sub-directory and both patterns.
    pub fn validate(&self) -> Result<(), String> {
        if self.subdir.is_empty() || !self.subdir.split('/').all(component_ok) {
            return Err(format!("invalid icon sub-directory {:?}", self.subdir));
        }
        for pattern in [&self.file_pattern_macos, &self.file_pattern_other] {
            let (head, tail) = pattern
                .split_once(PLACEHOLDER)
                .ok_or_else(|| format!("icon pattern {pattern:?} lacks {PLACEHOLDER}"))?;
            let rest_ok = !tail.contains(PLACEHOLDER);
            let chars_ok = head
                .chars()
                .chain(tail.chars())
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@'));
            if !rest_ok || !chars_ok || pattern.contains("..") {
                return Err(format!("invalid icon pattern {pattern:?}"));
            }
        }
        Ok(())
    }

    /// The file name for `state` on this OS, or `None` when the set or the state is invalid.
    pub fn file_name(&self, state: &str) -> Option<String> {
        if !state_ok(state) || self.validate().is_err() {
            return None;
        }
        let pattern = if cfg!(target_os = "macos") {
            &self.file_pattern_macos
        } else {
            &self.file_pattern_other
        };
        Some(pattern.replace(PLACEHOLDER, state))
    }

    /// The first existing icon file for `state`: resource directory, then beside the executable.
    /// This is a path probe only; the file is opened later, once, by [`IconSet::read`] or
    /// [`IconSet::load`], and nothing here keeps a result of the probe.
    pub fn find<R: Runtime>(&self, app: &AppHandle<R>, state: &str) -> Option<PathBuf> {
        let file = self.file_name(state)?;
        let resource_dir = app
            .path()
            .resource_dir()
            .ok()
            .map(|d| d.join(&self.subdir).join(&file));
        let exe_sibling = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .map(|d| d.join(&self.subdir).join(&file));
        [resource_dir, exe_sibling]
            .into_iter()
            .flatten()
            .find(|p| p.exists())
    }

    /// The bytes of the icon file for `state`, for a tray spec that takes bytes. The file is opened
    /// once and read through a [`MAX_ICON_BYTES`] cap from that handle, then its PNG or ICO header
    /// dimensions are checked against [`MAX_ICON_DIMENSION`] and [`MAX_ICON_PIXELS`]. An oversize,
    /// unreadable, symlinked or out-of-bounds file is reported as `tray_icon_load_failed` and gives
    /// `None`.
    pub fn read<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        state: &str,
        sink: EventSink,
    ) -> Option<Vec<u8>> {
        let path = self.find(app, state)?;
        match read_checked(&path) {
            Ok(bytes) => Some(bytes),
            Err(err) => {
                sink(
                    "tray_icon_load_failed",
                    &format!("{}: {err}", path.display()),
                );
                None
            }
        }
    }

    /// Load the icon for `state`: [`IconSet::read`]'s bytes, decoded in memory. Nothing is decoded
    /// until the byte cap and the dimension and pixel bounds have passed. A file that fails any
    /// bound or does not decode is reported as `tray_icon_load_failed` (`<path>: <error>`) and gives
    /// `None`.
    pub fn load<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        state: &str,
        sink: EventSink,
    ) -> Option<Image<'static>> {
        let path = self.find(app, state)?;
        let bytes = match read_checked(&path) {
            Ok(bytes) => bytes,
            Err(err) => {
                sink(
                    "tray_icon_load_failed",
                    &format!("{}: {err}", path.display()),
                );
                return None;
            }
        };
        match Image::from_bytes(&bytes) {
            Ok(img) => Some(img),
            Err(err) => {
                sink(
                    "tray_icon_load_failed",
                    &format!("{}: {err}", path.display()),
                );
                None
            }
        }
    }
}

/// Open `path` once (no-follow, regular file: `rightkit_fs::handle::open_read_nofollow(path,
/// &ReadOptions::new())`), read at most [`MAX_ICON_BYTES`] from that handle (`Read::take` of the cap
/// plus one byte, so an oversize file is detected without reading it), and bound the image's
/// declared dimensions before anything decodes it. There is no metadata call and no second open.
fn read_checked(path: &std::path::Path) -> Result<Vec<u8>, String> {
    use std::io::Read;

    let file =
        rightkit_fs::handle::open_read_nofollow(path, &rightkit_fs::handle::ReadOptions::new())
            .map_err(|err| err.to_string())?;
    let mut bytes = Vec::new();
    file.take(MAX_ICON_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| err.to_string())?;
    if bytes.len() as u64 > MAX_ICON_BYTES {
        return Err(format!("larger than {MAX_ICON_BYTES} bytes"));
    }
    check_dimensions(&bytes)?;
    Ok(bytes)
}

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
/// Most image entries an ICO may declare.
const MAX_ICO_ENTRIES: usize = 64;

fn dimensions_ok(width: u64, height: u64) -> Result<(), String> {
    if width == 0
        || height == 0
        || width > u64::from(MAX_ICON_DIMENSION)
        || height > u64::from(MAX_ICON_DIMENSION)
        || width * height > MAX_ICON_PIXELS
    {
        return Err(format!(
            "icon is {width}x{height}; at most {MAX_ICON_DIMENSION} per side and {MAX_ICON_PIXELS} pixels"
        ));
    }
    Ok(())
}

fn le_u32(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    Some(u32::from_le_bytes(bytes.get(at..end)?.try_into().ok()?))
}

fn png_dimensions(bytes: &[u8]) -> Result<(u64, u64), String> {
    if bytes.len() < 24 || bytes[12..16] != *b"IHDR" {
        return Err("PNG header is truncated".into());
    }
    let be =
        |at: usize| u32::from_be_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]]);
    Ok((u64::from(be(16)), u64::from(be(20))))
}

/// Check the dimensions a PNG or ICO declares, before any decoder allocates for them. PNG: the
/// IHDR width and height. ICO: at most 64 entries; each entry's directory size, and the header of
/// the image it points at (an embedded PNG's IHDR, or a BMP's `BITMAPINFOHEADER`, whose height
/// counts the mask and so is halved), must be within bounds; an entry whose data lies outside the
/// file is refused. Any other format is refused.
fn check_dimensions(bytes: &[u8]) -> Result<(), String> {
    if bytes.starts_with(&PNG_SIGNATURE) {
        let (w, h) = png_dimensions(bytes)?;
        return dimensions_ok(w, h);
    }
    if bytes.len() >= 6 && bytes[..4] == [0u8, 0, 1, 0] {
        let count = usize::from(u16::from_le_bytes([bytes[4], bytes[5]]));
        if count == 0 || count > MAX_ICO_ENTRIES || bytes.len() < 6 + 16 * count {
            return Err("ICO directory is invalid".into());
        }
        for index in 0..count {
            let entry = 6 + 16 * index;
            // A directory size byte of 0 means 256.
            let dir = |b: u8| if b == 0 { 256 } else { u64::from(b) };
            dimensions_ok(dir(bytes[entry]), dir(bytes[entry + 1]))?;
            let size = le_u32(bytes, entry + 8).ok_or("ICO entry is truncated")? as usize;
            let offset = le_u32(bytes, entry + 12).ok_or("ICO entry is truncated")? as usize;
            let end = offset
                .checked_add(size)
                .filter(|end| *end <= bytes.len() && size >= 16)
                .ok_or("ICO entry data lies outside the file")?;
            let data = &bytes[offset..end];
            if data.starts_with(&PNG_SIGNATURE) {
                let (w, h) = png_dimensions(data)?;
                dimensions_ok(w, h)?;
            } else {
                // BITMAPINFOHEADER: size, width (i32), height (i32, twice the image height).
                let header = le_u32(data, 0).ok_or("ICO bitmap header is truncated")?;
                let width = le_u32(data, 4).ok_or("ICO bitmap header is truncated")? as i32;
                let height = le_u32(data, 8).ok_or("ICO bitmap header is truncated")? as i32;
                if header < 40 {
                    return Err("ICO bitmap header is not a BITMAPINFOHEADER".into());
                }
                dimensions_ok(
                    u64::from(width.unsigned_abs()),
                    u64::from(height.unsigned_abs() / 2),
                )?;
            }
        }
        return Ok(());
    }
    Err("icon is neither PNG nor ICO".into())
}

/// Show the icon for `state` on the tray `tray_id` (macOS: not as a template, so the colour
/// shows). `false` when no icon was found or the tray does not exist; a missing icon is reported as
/// `tray_icon_missing`.
pub fn set_state_icon<R: Runtime>(
    app: &AppHandle<R>,
    tray_id: &str,
    set: &IconSet,
    state: &str,
    sink: EventSink,
) -> bool {
    let Some(img) = set.load(app, state, sink) else {
        sink(
            "tray_icon_missing",
            &format!("no tray icon found for state={state}"),
        );
        return false;
    };
    let Some(tray) = app.tray_by_id(tray_id) else {
        return false;
    };
    #[cfg(target_os = "macos")]
    let _ = tray.set_icon_with_as_template(Some(img), false);
    #[cfg(not(target_os = "macos"))]
    let _ = tray.set_icon(Some(img));
    true
}

/// A tooltip override that is on while a condition holds (for example "access to a device is
/// blocked"), plus the latch that makes a change apply once. `const`-constructible for a `static`.
pub struct TrayAlert {
    active: AtomicBool,
    event: &'static str,
    normal_tooltip: &'static str,
    alert_tooltip: &'static str,
}

impl TrayAlert {
    pub const fn new(
        event: &'static str,
        normal_tooltip: &'static str,
        alert_tooltip: &'static str,
    ) -> Self {
        Self {
            active: AtomicBool::new(false),
            event,
            normal_tooltip,
            alert_tooltip,
        }
    }

    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }

    /// Set the state. A change is reported as `<event>` (`blocked=<bool>`) and applied to the tray
    /// tooltip; setting the state it already has does nothing.
    pub fn set<R: Runtime>(
        &self,
        app: &AppHandle<R>,
        tray_id: &str,
        active: bool,
        sink: EventSink,
    ) {
        if self.active.swap(active, Ordering::SeqCst) == active {
            return;
        }
        sink(self.event, &format!("blocked={active}"));
        if let Some(tray) = app.tray_by_id(tray_id) {
            let _ = tray.set_tooltip(Some(if active {
                self.alert_tooltip
            } else {
                self.normal_tooltip
            }));
        }
    }
}

/// Run `on_click` for each left click on the tray `tray_id`, read from the shell's tray event
/// (`EVENT_TRAY`: `{"kind":"click","trayId":..}`). Pair it with `LeftClick::Emit`, which opens no
/// menu and touches no window, so `on_click` is the only effect. A payload over
/// [`MAX_EVENT_PAYLOAD`] bytes or that does not parse is ignored. The listener lives until the
/// returned id is passed to `unlisten` or the app exits.
pub fn listen_clicks<R: Runtime>(
    app: &AppHandle<R>,
    tray_id: &str,
    on_click: impl Fn(&AppHandle<R>) + Send + Sync + 'static,
) -> tauri::EventId {
    let handle = app.clone();
    let tray_id = tray_id.to_owned();
    app.listen(crate::EVENT_TRAY, move |event| {
        if event.payload().len() > MAX_EVENT_PAYLOAD {
            return;
        }
        let Ok(payload) = serde_json::from_str::<serde_json::Value>(event.payload()) else {
            return;
        };
        if payload["kind"] == "click" && payload["trayId"] == tray_id.as_str() {
            on_click(&handle);
        }
    })
}
