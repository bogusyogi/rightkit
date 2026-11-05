//! The clipboard strip (the Windows half of the clipboard spec, section 7): a solid black
//! strip of cards that comes out of the notch on its neck on a global hot key (Alt+Shift+V),
//! from the Tools card or from the wheel, with search and its prefixes, the filter popover,
//! board tabs, a context menu, keyboard paste, delete with undo, the Space preview, and drag
//! of a card out to another app or onto a board tab. It works whenever its own switch is on,
//! with the launcher on or off.
//!
//! One thread owns it: a layered window drawn with the notch's canvas (as `launcher.rs` does),
//! shown and hidden, never destroyed, put through the card's emergence (`render.rs`: the neck,
//! the slide and the opening). Unlike the notch's cards it takes the foreground when it opens,
//! so keys arrive at once; the window that was in front is remembered and gets the foreground
//! back on close or paste, and the strip closes when another app takes the foreground or the
//! screen locks. It is excluded from screen capture.
//!
//! The picture is `draw.rs`, the popovers and the preview `popover.rs`, drag `drag.rs`, and the
//! view-shot renderer `shot.rs`.
//!
//! Nothing here logs item content.

pub mod drag;
mod draw;
pub mod kinds;
mod popover;

pub use kinds::{Place, draw as kind_mark};

use crate::host;
use crate::json;
use crate::model::{self as clip, Item, Kind, Snapshot, Status};
use crate::paste::{self as clip_paste, Choice};
use crate::search::{self as clip_search, Query, View};
use crate::{capture as clip_capture, img as clip_img};
use popover::{PREVIEW_LEAD, PopHit};
use rightkit_hotkeys::windows::GlobalHotkeys;
use rightkit_notch_ui::card::Tail;
use rightkit_notch_ui::layout::{self, Edge};
use rightkit_notch_ui::motion::{self, Spring};
use rightkit_notch_ui::render;
use rightkit_notch_ui::swell::Swell;
use rightkit_win_ui::canvas::{Canvas, Mask};
use rightkit_win_ui::raii::{ClassGuard, OwnedWindow, hwnd_from_key, hwnd_key};
use rightkit_win_ui::surface::{Surface, TextPainter};
use rightkit_win_ui::{fileicon, glyphs};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, POINT, SIZE, WPARAM};
use windows::Win32::Graphics::Gdi::{BLENDFUNCTION, ValidateRect};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, VIRTUAL_KEY, VK_CONTROL, VK_MENU, VK_SHIFT,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{PCWSTR, w};

/// Calls the `windows` crate does not need to grow a feature for.
#[allow(non_snake_case)]
mod ffi {
    use std::ffi::c_void;
    pub type Handle = *mut c_void;

    #[link(name = "user32")]
    unsafe extern "system" {
        pub fn SetFocus(window: Handle) -> Handle;
        pub fn SetCapture(window: Handle) -> Handle;
        pub fn ReleaseCapture() -> i32;
        pub fn AttachThreadInput(attach: u32, to: u32, on: i32) -> i32;
        pub fn GetDoubleClickTime() -> u32;
        pub fn SetWindowDisplayAffinity(window: Handle, affinity: u32) -> i32;
        pub fn CreatePopupMenu() -> Handle;
        pub fn AppendMenuW(menu: Handle, flags: u32, id: usize, text: *const u16) -> i32;
        pub fn DestroyMenu(menu: Handle) -> i32;
        pub fn TrackPopupMenu(
            menu: Handle,
            flags: u32,
            x: i32,
            y: i32,
            reserved: i32,
            window: Handle,
            rect: *const c_void,
        ) -> i32;
    }

    #[link(name = "wtsapi32")]
    unsafe extern "system" {
        pub fn WTSRegisterSessionNotification(window: Handle, flags: u32) -> i32;
    }
}

const CLASS: PCWSTR = w!("RightKitClipStrip");
/// The preview's window class: a picture the pointer goes through, with a proc of its own.
const PREVIEW_CLASS: PCWSTR = w!("RightKitClipPreview");
const WM_SYNC: u32 = WM_APP + 1;
const WM_TOGGLE: u32 = WM_APP + 2;
/// Opens the strip with one item selected (the launcher's Ctrl+Return); the id is in `PICK`.
const WM_OPEN_AT: u32 = WM_APP + 3;
const WM_WTSSESSION_CHANGE: u32 = 0x02B1;
const WTS_SESSION_LOCK: usize = 7;
const ANIM_TIMER: usize = 1;
const CLOCK_TIMER: usize = 2;
/// Redraws the strip when a message on its footer has had its time.
const TOAST_TIMER: usize = 3;
const ANIM_MS: u32 = 16;
/// Relative times are drawn again this often while the strip is open (spec 7.13).
const CLOCK_MS: u32 = 30_000;
const WDA_EXCLUDEFROMCAPTURE: u32 = 0x11;

/// The display affinity the strip and its preview take: `WDA_EXCLUDEFROMCAPTURE` while
/// `clipboardHideFromCapture` is on (the default), else none (0). Read on each show.
fn capture_affinity() -> u32 {
    if host::switch_on("clipboardHideFromCapture") {
        WDA_EXCLUDEFROMCAPTURE
    } else {
        0
    }
}
/// The left button is down in a mouse message's key flags.
const MK_LBUTTON: usize = 0x1;

// Menu flags and the ids of its commands.
const MF_GRAYED: u32 = 0x1;
const MF_CHECKED: u32 = 0x8;
const MF_POPUP: u32 = 0x10;
const MF_SEPARATOR: u32 = 0x800;
const TPM_RIGHTBUTTON: u32 = 0x2;
const TPM_RETURNCMD: u32 = 0x100;
const CMD_PASTE: u32 = 1;
const CMD_PLAIN: u32 = 2;
const CMD_COPY: u32 = 3;
const CMD_PREVIEW: u32 = 4;
const CMD_PIN: u32 = 5;
const CMD_UNBOARD: u32 = 6;
const CMD_LANE: u32 = 7;
const CMD_OPEN: u32 = 8;
const CMD_DELETE: u32 = 9;
const CMD_NEW_BOARD: u32 = 99;
const CMD_BOARD: u32 = 100;
const CMD_RENAME: u32 = 200;
const CMD_COLOUR: u32 = 201;
const CMD_LEFT: u32 = 202;
const CMD_RIGHT: u32 = 203;
const CMD_DEL_BOARD: u32 = 204;

const VK_BACK: u32 = 0x08;
const VK_TAB: u32 = 0x09;
const VK_RETURN: u32 = 0x0D;
const VK_ESCAPE: u32 = 0x1B;
const VK_END: u32 = 0x23;
const VK_HOME: u32 = 0x24;
const VK_LEFT: u32 = 0x25;
const VK_UP: u32 = 0x26;
const VK_RIGHT: u32 = 0x27;
const VK_DOWN: u32 = 0x28;
const VK_DELETE: u32 = 0x2E;
const VK_1: u32 = 0x31;
const VK_9: u32 = 0x39;
const VK_A: u32 = 0x41;
const VK_B: u32 = 0x42;
const VK_C: u32 = 0x43;
const VK_F: u32 = 0x46;
const VK_P: u32 = 0x50;
const VK_Y: u32 = 0x59;
const VK_Z: u32 = 0x5A;

// Sizes in DIPs at Medium (spec 7.2); all are multiplied by the display's DPI scale and the
// notch size scale clamped to 0.8 to 1.25.
const CARD_SIZES: [f32; 3] = [156.0, 196.0, 244.0];
const SPACING: f32 = 10.0;
const PAD: f32 = 11.0;
const CORNER: f32 = 18.0;
const CARD_CORNER: f32 = 12.0;
const HEADER: f32 = 30.0;
const HEADER_GAP: f32 = 8.0;
const FOOTER: f32 = 18.0;
const BAND: f32 = 3.0;
const EDGE_MARGIN: f32 = 24.0;
const TAB_HEIGHT: f32 = 22.0;
const SEARCH_WIDTH: f32 = 220.0;
/// The strip is at least this long on a row (the Mac's 420).
const MIN_LENGTH: f32 = 420.0;
/// A hovered card grows to this on `motion::EMPHASIS` (spec 7.7).
const HOVER_SCALE: f32 = 1.04;
/// A press that moves this far (device pixels at 100%) becomes a drag.
const DRAG_DISTANCE: f32 = 8.0;

const INK: u32 = 0xFF_FFFF;
const ALERT: u32 = 0xE5_675F;
const GREY: u32 = 0x8E_8E93;
const CONTENT: f32 = 0.88;
const META: f32 = 0.68;
const TOAST_MS: u64 = 6000;

/// The kinds, in the filter popover's order.
const KINDS: [Kind; 7] = [
    Kind::Text,
    Kind::Link,
    Kind::Colour,
    Kind::Code,
    Kind::File,
    Kind::Image,
    Kind::Rich,
];

static THREAD_ID: AtomicU32 = AtomicU32::new(0);
/// The strip window while it is shown (a handle value; 0 when hidden), for the key hook.
static SHOWN_HWND: AtomicIsize = AtomicIsize::new(0);

/// Whether the strip is shown and the foreground window: the key hook leaves Alt+Shift+digits
/// to it then (they paste as plain text), instead of taking Alt+Shift+4 and 5 for screenshots.
pub fn is_foreground() -> bool {
    let shown = SHOWN_HWND.load(Ordering::Relaxed);
    // SAFETY: a plain query.
    shown != 0 && hwnd_key(unsafe { GetForegroundWindow() }) == shown
}
/// The item the launcher asked the strip to open on.
static PICK: Mutex<Option<String>> = Mutex::new(None);
/// What stands in the hot key's way, for the hub.
static STATUS: Mutex<&'static str> = Mutex::new("");

/// The text the hub shows under the strip's shortcut row, or empty.
pub fn hotkey_status() -> &'static str {
    *STATUS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn set_status(next: &'static str) {
    let mut status = STATUS.lock().unwrap_or_else(PoisonError::into_inner);
    if *status != next {
        *status = next;
        drop(status);
        host::wake_ui();
    }
}

/// Where the notch sits on a display: its edge, its window (as
/// `left, top, right, bottom`), and the notch size scale.
#[derive(Clone, Copy, Debug)]
pub struct NotchSpot {
    pub edge: Edge,
    pub notch: (i32, i32, i32, i32),
    pub scale: f32,
}

/// The history is on and something can open it: the strip, or the launcher's clip mode.
pub fn can_open() -> bool {
    host::switch_on("launcherClipboard")
        && (host::switch_on("clipboardStripEnabled") || host::switch_on("launcherEnabled"))
}

pub fn start() {
    let spawned = std::thread::Builder::new()
        .name("rightkit-clip-strip".to_string())
        .spawn(thread_main);
    if spawned.is_err() {
        host::log_info("clipboard_strip", &[("result", "no_thread")]);
    }
}

/// The switches or the shortcut changed: hold or give back the hot key.
pub fn sync() {
    post(WM_SYNC);
}

/// The Tools card's and the wheel's Clipboard item (7.11.3): the strip when its switch is on,
/// else the launcher's clip mode when the launcher is on, else nothing.
pub fn open_from_tool() {
    if !host::switch_on("launcherClipboard") {
        return;
    }
    if host::switch_on("clipboardStripEnabled") {
        post(WM_TOGGLE);
    } else if host::switch_on("launcherEnabled") {
        host::open_launcher_clip();
    }
}

/// Opens the strip with the item `id` selected (the launcher's Ctrl+Return).
pub fn open_at(id: &str) {
    if host::switch_on("launcherClipboard") && host::switch_on("clipboardStripEnabled") {
        *PICK.lock().unwrap_or_else(PoisonError::into_inner) = Some(id.to_string());
        post(WM_OPEN_AT);
    }
}

fn post(message: u32) {
    let id = THREAD_ID.load(Ordering::Relaxed);
    if id != 0 {
        // Failure means the thread is gone or has no queue yet; nothing to recover.
        let _ = unsafe { PostThreadMessageW(id, message, WPARAM(0), LPARAM(0)) };
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

// ---- state --------------------------------------------------------------------------------

/// How a piece of text is set.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Face {
    Regular,
    Bold,
    /// Consolas, for code and hex values.
    Mono,
}

/// The card size setting (spec 7.2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum CardSize {
    Small,
    Medium,
    Large,
}

impl CardSize {
    fn named(name: &str) -> CardSize {
        match name {
            "small" => CardSize::Small,
            "large" => CardSize::Large,
            _ => CardSize::Medium,
        }
    }

    fn side(self) -> f32 {
        CARD_SIZES[self as usize]
    }

    /// How many lines of text a card holds.
    fn lines(self) -> usize {
        match self {
            CardSize::Small => 7,
            CardSize::Medium => 9,
            CardSize::Large => 12,
        }
    }
}

/// One card in the strip: the item (an index into the snapshot) and how many screenshots its
/// stack holds (1 when it is not a stack).
#[derive(Clone, Copy)]
struct Card {
    item: usize,
    stack: usize,
}

/// What a click or key on the header means.
#[derive(Clone, PartialEq)]
enum Tab {
    View(View),
    New,
    Search,
    Filter,
}

#[derive(Clone, Copy, Default)]
struct Rect {
    x: f32,
    y: f32,
    w: f32,
    h: f32,
}

impl Rect {
    fn has(&self, x: f32, y: f32) -> bool {
        x >= self.x && x < self.x + self.w && y >= self.y && y < self.y + self.h
    }
}

/// A board's name being typed (a new one, or a rename).
struct Naming {
    text: Vec<char>,
    /// The board to rename; none for a new one.
    rename: Option<String>,
    /// The item a new board takes on at once.
    add: Option<String>,
}

/// A question the strip is waiting to have answered with Return or Escape.
enum Confirm {
    /// Delete these items (a whole stack).
    Delete(Vec<String>),
    /// Delete this board; its items stay.
    Board(String),
}

/// The date the filter popover limits to (spec 5.3).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum DatePick {
    #[default]
    Any,
    Today,
    Yesterday,
    Last7,
    Last30,
}

impl DatePick {
    const ALL: [DatePick; 5] = [
        DatePick::Any,
        DatePick::Today,
        DatePick::Yesterday,
        DatePick::Last7,
        DatePick::Last30,
    ];

    fn label(self) -> &'static str {
        match self {
            DatePick::Any => host::t("Any time"),
            DatePick::Today => host::t("Today"),
            DatePick::Yesterday => host::t("Yesterday"),
            DatePick::Last7 => host::t("Last 7 days"),
            DatePick::Last30 => host::t("Last 30 days"),
        }
    }

    /// The window in Unix seconds: from (included) and to (excluded), on local days.
    fn window(self, now: f64, offset: i64) -> (Option<f64>, Option<f64>) {
        let today = ((now as i64 + offset).div_euclid(86_400) * 86_400 - offset) as f64;
        let days = |n: f64| today - n * 86_400.0;
        match self {
            DatePick::Any => (None, None),
            DatePick::Today => (Some(today), None),
            DatePick::Yesterday => (Some(days(1.0)), Some(today)),
            DatePick::Last7 => (Some(days(6.0)), None),
            DatePick::Last30 => (Some(days(29.0)), None),
        }
    }
}

/// The filter popover's choices (spec 5.3).
#[derive(Clone, Default, PartialEq)]
struct Panel {
    kinds: Vec<Kind>,
    /// Source app ids.
    apps: Vec<String>,
    date: DatePick,
}

impl Panel {
    fn active(&self) -> bool {
        !self.kinds.is_empty() || !self.apps.is_empty() || self.date != DatePick::Any
    }
}

/// What floats over the strip.
#[derive(Clone)]
enum Popover {
    Filters,
    /// A board's colour: its id and the hex field's text.
    Colour {
        board: String,
        hex: String,
    },
    /// Ctrl+B: the text typed and the row chosen.
    Picker {
        query: Vec<char>,
        selected: usize,
    },
    /// The card's menu, drawn in a picture (a live one is the system's own).
    #[cfg_attr(not(feature = "qa-views"), allow(dead_code))]
    Context,
}

/// The Space preview: its picture and where it hangs.
struct Preview {
    picture: Canvas,
    pose: render::Body,
    edge: Edge,
}

/// What a context menu is for.
#[derive(Clone)]
enum MenuFor {
    Card(String),
    Tab(String),
}

/// One line of a context menu.
struct Entry {
    id: u32,
    label: String,
    checked: bool,
    disabled: bool,
    separator: bool,
    children: Vec<Entry>,
}

impl Entry {
    fn line(id: u32, label: impl Into<String>) -> Entry {
        Entry {
            id,
            label: label.into(),
            checked: false,
            disabled: false,
            separator: false,
            children: Vec::new(),
        }
    }

    fn rule() -> Entry {
        Entry {
            separator: true,
            ..Entry::line(0, "")
        }
    }
}

/// What a key or click asks for once the strip's state is no longer borrowed.
enum Do {
    Nothing,
    Redraw,
    Menu(MenuFor, (i32, i32)),
    /// A card pulled out of the strip (its index): the drag runs outside the borrow.
    Drag(usize),
}

/// What a button press began on, for a drag.
enum PressOn {
    Card(usize),
    Tab(String),
}

/// A decoded thumbnail: width, height and premultiplied pixels (`None` when it would not decode).
type Thumb = Option<(usize, usize, Arc<Vec<u32>>)>;

struct Strip {
    window: OwnedWindow,
    /// The preview's window: a picture the pointer goes through.
    preview_window: OwnedWindow,
    painter: TextPainter,
    keys: Option<GlobalHotkeys>,
    hot: Option<rightkit_hotkeys::Chord>,
    hot_text: String,
    shown: bool,
    closing: bool,
    in_menu: bool,
    /// A card is out on a drag: losing the foreground to its target does not close the strip.
    dragging: bool,
    /// The window that was in front when the strip opened (a handle value).
    previous: isize,
    snap: Snapshot,
    view: View,
    query: Vec<char>,
    searching: bool,
    select_all: bool,
    panel: Panel,
    popover: Option<Popover>,
    /// Where the popover is drawn, and what its controls are.
    popover_rect: Option<Rect>,
    hits: Vec<(Rect, PopHit)>,
    cards: Vec<Card>,
    /// How many items the view, query and filters list (a stack counts each screenshot).
    total: usize,
    /// The stack open on its own (its id), after Return on a stack card.
    stack_open: Option<String>,
    selected: usize,
    scroll: f32,
    /// Where a glide of the scroll is going.
    scroll_goal: Option<f32>,
    scroll_spring: Spring,
    hovered: Option<usize>,
    /// How far each card has grown toward its hover size (0 to 1).
    hover_scale: Vec<Spring>,
    /// The selection ring's place along the strip, on its own spring.
    ring: Spring,
    naming: Option<Naming>,
    confirm: Option<Confirm>,
    /// A line for the footer: its text, whether it is an alert, when it came and how long it stays.
    message: Option<(String, bool, Instant, u64)>,
    undo: Option<clip::Undo>,
    last_click: Option<(Instant, usize)>,
    last_tab: Option<(Instant, String)>,
    press: Option<(PressOn, f32, f32)>,
    /// A board tab pulled along the header: its id.
    tab_drag: Option<String>,
    // Geometry of the open strip, in device pixels.
    vertical: bool,
    k: f32,
    dpi: u32,
    card_px: f32,
    size_class: CardSize,
    numbers: bool,
    /// The picture's size.
    size: (usize, usize),
    /// The notch the strip hangs from (None: along the bottom of the display), the display's
    /// work area, and how the layout was asked for.
    spot: Option<NotchSpot>,
    work: (i32, i32, i32, i32),
    bottom: bool,
    edge: Option<Edge>,
    /// The strip's body and window when it hangs from a notch.
    pose: Option<render::Body>,
    /// Where the picture sits in the window (when it hangs from a notch).
    origin: (i32, i32),
    /// The window's place when the strip is along the bottom.
    place: (i32, i32),
    tabs: Vec<(Rect, Tab, Option<String>)>,
    tab_scroll: f32,
    card_rects: Vec<Rect>,
    action_rect: Option<Rect>,
    /// How far the strip is out of the notch and how far open (the card's two springs), and the
    /// same for the preview.
    slide: Swell,
    grow: Swell,
    pslide: Swell,
    pgrow: Swell,
    preview_wanted: bool,
    preview: Option<Preview>,
    last_tick: Option<Instant>,
    frame: Option<Canvas>,
    thumbs: HashMap<String, Thumb>,
    paths: HashMap<String, Vec<String>>,
    colours: HashMap<String, u32>,
    colours_loaded: bool,
    /// Stand-in pictures by item id, for the view shots.
    arts: HashMap<String, String>,
    offset: i64,
    // What the view shots fix: the time, the footer and the empty state's words.
    fixed_now: Option<f64>,
    fixed_footer: Option<String>,
    fixed_empty: Option<(String, Option<String>, Option<String>)>,
    /// Drawn as the Mac's stills draw it (the view shots): the cards are laid out and slid, not
    /// scrolled, a run longer than its room overflows both ends equally, and nothing clips them
    /// but the strip's own body.
    still: bool,
}

thread_local! {
    static STRIP: RefCell<Option<Strip>> = const { RefCell::new(None) };
}

/// Runs `f` on the strip unless it is already in use further up the stack.
fn with_strip<R>(f: impl FnOnce(&mut Strip) -> R) -> Option<R> {
    STRIP.with(|cell| {
        let mut guard = cell.try_borrow_mut().ok()?;
        guard.as_mut().map(f)
    })
}

fn rgb_of(hex: &str) -> Option<u32> {
    let digits = hex.strip_prefix('#')?;
    if digits.len() < 6 || !digits.is_char_boundary(6) {
        return None;
    }
    u32::from_str_radix(&digits[..6], 16).ok()
}

impl Strip {
    /// A strip with nothing in it on the windows given (the view shots give none).
    fn blank(window: OwnedWindow, preview_window: OwnedWindow, painter: TextPainter) -> Strip {
        Strip {
            window,
            preview_window,
            painter,
            keys: None,
            hot: None,
            hot_text: String::new(),
            shown: false,
            closing: false,
            in_menu: false,
            dragging: false,
            previous: 0,
            snap: Snapshot {
                items: Vec::new(),
                boards: Vec::new(),
                status: Status::Ready,
                pinned_mb: None,
                last_board: None,
            },
            view: View::History,
            query: Vec::new(),
            searching: false,
            select_all: false,
            panel: Panel::default(),
            popover: None,
            popover_rect: None,
            hits: Vec::new(),
            cards: Vec::new(),
            total: 0,
            stack_open: None,
            selected: 0,
            scroll: 0.0,
            scroll_goal: None,
            scroll_spring: Spring::at(0.0),
            hovered: None,
            hover_scale: Vec::new(),
            ring: Spring::at(0.0),
            naming: None,
            confirm: None,
            message: None,
            undo: None,
            last_click: None,
            last_tab: None,
            press: None,
            tab_drag: None,
            vertical: false,
            k: 1.0,
            dpi: 96,
            card_px: CARD_SIZES[1],
            size_class: CardSize::Medium,
            numbers: true,
            size: (1, 1),
            spot: None,
            work: (0, 0, 0, 0),
            bottom: false,
            edge: None,
            pose: None,
            origin: (0, 0),
            place: (0, 0),
            tabs: Vec::new(),
            tab_scroll: 0.0,
            card_rects: Vec::new(),
            action_rect: None,
            slide: Swell::new(),
            grow: Swell::new(),
            pslide: Swell::new(),
            pgrow: Swell::new(),
            preview_wanted: false,
            preview: None,
            last_tick: None,
            frame: None,
            thumbs: HashMap::new(),
            paths: HashMap::new(),
            colours: HashMap::new(),
            colours_loaded: false,
            arts: HashMap::new(),
            offset: 0,
            fixed_now: None,
            fixed_footer: None,
            fixed_empty: None,
            still: false,
        }
    }

    // ---- the cards ----------------------------------------------------------------------

    fn now(&self) -> f64 {
        self.fixed_now.unwrap_or_else(clip::now)
    }

    fn text(&self) -> String {
        self.query.iter().collect()
    }

    /// The query's prefixes and words with the popover's choices joined to them (spec 5.3:
    /// several kinds or apps picked are alternatives; the date narrows what the prefixes ask).
    fn query(&self) -> Query {
        let mut query = clip_search::parse_query(&self.text(), self.offset);
        for kind in &self.panel.kinds {
            if !query.filters.types.contains(kind) {
                query.filters.types.push(*kind);
            }
        }
        query.filters.app_ids = self.panel.apps.clone();
        let (from, to) = self.panel.date.window(self.now(), self.offset);
        if let Some(from) = from {
            query.filters.after = Some(query.filters.after.map_or(from, |at| at.max(from)));
        }
        if let Some(to) = to {
            query.filters.before = Some(query.filters.before.map_or(to, |at| at.min(to)));
        }
        query
    }

    /// The cards the view and the query list, screenshot runs collapsed into one card each
    /// (unless a query or filter is set, or one stack is open on its own).
    fn recompute(&mut self) {
        let found = clip_search::search(
            &self.snap.items,
            &self.snap.boards,
            &self.view,
            &self.query(),
            self.now(),
            usize::MAX,
        );
        self.total = found.len();
        let grouping = self.text().trim().is_empty() && !self.panel.active();
        let mut cards: Vec<Card> = Vec::with_capacity(found.len());
        for index in found {
            let item = &self.snap.items[index];
            if let Some(open) = &self.stack_open {
                if item.stack_id() == Some(open.as_str()) {
                    cards.push(Card {
                        item: index,
                        stack: 1,
                    });
                }
                continue;
            }
            match item.stack_id().filter(|_| item.screenshot && grouping) {
                Some(stack) => {
                    let same = cards.iter_mut().find(|card| {
                        self.snap.items[card.item].screenshot
                            && self.snap.items[card.item].stack_id() == Some(stack)
                    });
                    match same {
                        Some(card) => card.stack += 1,
                        None => cards.push(Card {
                            item: index,
                            stack: 1,
                        }),
                    }
                }
                None => cards.push(Card {
                    item: index,
                    stack: 1,
                }),
            }
        }
        self.cards = cards;
        self.selected = self.selected.min(self.cards.len().saturating_sub(1));
        self.hover_scale.resize(self.cards.len(), Spring::at(0.0));
        if self.hovered.is_some_and(|at| at >= self.cards.len()) {
            self.hovered = None;
        }
    }

    fn selected_item(&self) -> Option<&Item> {
        self.cards
            .get(self.selected)
            .map(|card| &self.snap.items[card.item])
    }

    fn refresh(&mut self) {
        self.snap = clip::snapshot();
        self.recompute();
    }

    /// A line for the footer for `seconds`.
    fn say(&mut self, text: &str) {
        self.flash(text, false, TOAST_MS / 1000);
    }

    fn flash(&mut self, text: &str, alert: bool, seconds: u64) {
        self.message = Some((text.to_string(), alert, Instant::now(), seconds * 1000));
        if self.shown {
            // The footer takes room the strip had not: its height follows.
            self.relayout();
            // SAFETY: a timer on this thread's own window; it only redraws.
            let _ = unsafe {
                SetTimer(
                    Some(self.window.hwnd()),
                    TOAST_TIMER,
                    (seconds * 1000) as u32 + 50,
                    None,
                )
            };
        }
    }

    /// The apps the history has seen, most used first (spec 5.3).
    fn apps_seen(&self) -> Vec<(String, String)> {
        let mut uses: Vec<(String, String, u64)> = Vec::new();
        for item in &self.snap.items {
            let Some(app) = &item.source else {
                continue;
            };
            match uses.iter_mut().find(|(id, ..)| *id == app.id) {
                Some(entry) => entry.2 += 1 + item.uses,
                None => uses.push((app.id.clone(), app.name.clone(), 1 + item.uses)),
            }
        }
        uses.sort_by(|a, b| {
            b.2.cmp(&a.2)
                .then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase()))
        });
        uses.into_iter().map(|(id, name, _)| (id, name)).collect()
    }

    // ---- what the footer and the empty state say ----------------------------------------

    /// Whether the footer row is there: it has a line to say.
    fn footer_shown(&self) -> bool {
        self.footer_text().is_some()
    }

    /// The footer's line and whether it is an alert: a question first, then the lock, what has
    /// just happened, the pinned items over the cap, then the count of a search (the Mac's
    /// order).
    fn footer_text(&self) -> Option<(String, bool)> {
        if let Some(text) = &self.fixed_footer {
            return Some((text.clone(), false));
        }
        if self.still {
            // A still has the footer its fixture gives and no other.
            return None;
        }
        match &self.confirm {
            Some(Confirm::Delete(ids)) => {
                return Some((
                    host::tf(
                        "Delete {} screenshots? Press Return to delete, Esc to keep.",
                        &[&ids.len().to_string()],
                    ),
                    false,
                ));
            }
            Some(Confirm::Board(id)) => {
                let board = self.snap.boards.iter().find(|b| b.id == *id)?;
                let count = self
                    .snap
                    .items
                    .iter()
                    .filter(|item| item.boards.contains(id))
                    .count();
                let text = if count == 0 {
                    host::tf(
                        "Delete board \u{201C}{}\u{201D}? Press Return to delete, Esc to keep.",
                        &[&board.name],
                    )
                } else {
                    host::tf(
                        "Delete board \u{201C}{}\u{201D}? Its {} items stay in History. Return deletes, Esc keeps.",
                        &[&board.name, &count.to_string()],
                    )
                };
                return Some((text, false));
            }
            None => {}
        }
        if self.snap.status == Status::Locked {
            return Some((
                host::t("Another Pulse is keeping history").to_string(),
                true,
            ));
        }
        if let Some((text, alert, at, millis)) = &self.message
            && at.elapsed().as_millis() < u128::from(*millis)
        {
            return Some((text.clone(), *alert));
        }
        if let Some(mb) = self.snap.pinned_mb {
            return Some((
                host::tf("Pinned items use {} MB", &[&mb.to_string()]),
                false,
            ));
        }
        if !self.text().trim().is_empty() || self.panel.active() || self.stack_open.is_some() {
            return Some((host::tf("{} items", &[&self.total.to_string()]), false));
        }
        None
    }

    /// The empty strip's words: a title, a detail line and an action's label (spec 7.9).
    fn empty_lines(&self) -> (String, Option<String>, Option<String>) {
        if let Some(fixed) = &self.fixed_empty {
            return fixed.clone();
        }
        let (title, detail, action) = match self.snap.status {
            Status::Newer => (host::t("This history needs a newer Pulse"), None, None),
            Status::Unreadable => (
                host::t("Couldn't read clipboard history"),
                None,
                Some(host::t("Show folder")),
            ),
            _ if !self.text().trim().is_empty() || self.panel.active() => (
                host::t("No matches"),
                Some(host::t("Esc clears the search").to_string()),
                None,
            ),
            _ => match &self.view {
                View::History => (
                    host::t("Nothing copied yet."),
                    Some(host::t("Copy something and it lands here.").to_string()),
                    None,
                ),
                View::Pinned => (
                    host::t("Nothing pinned yet"),
                    Some(host::t("Press Ctrl P on a card").to_string()),
                    None,
                ),
                View::Board(id) => {
                    let name = self
                        .snap
                        .boards
                        .iter()
                        .find(|b| b.id == *id)
                        .map_or(host::t("this board"), |b| b.name.as_str());
                    return (
                        host::tf("Nothing on {} yet", &[name]),
                        Some(host::t("Drag a card here or press Ctrl B").to_string()),
                        None,
                    );
                }
            },
        };
        (title.to_string(), detail, action.map(str::to_string))
    }

    // ---- geometry -----------------------------------------------------------------------

    /// Where the strip goes and how big it is, for the display it was asked for on (spec 7.1,
    /// 7.2). Hung from a notch it gets the card's window: the picture, the neck's room and the
    /// far margin the opening spring overshoots into.
    fn layout(
        &mut self,
        spot: Option<NotchSpot>,
        work: (i32, i32, i32, i32),
        dpi: u32,
        bottom: bool,
    ) {
        self.spot = spot;
        self.work = work;
        self.bottom = bottom;
        self.dpi = dpi;
        let s = layout::scale(dpi);
        let attached = spot.filter(|_| !bottom);
        let notch_scale = attached.map_or(1.0, |spot| spot.scale).clamp(0.8, 1.25);
        let k = s * notch_scale;
        self.k = k;
        self.card_px = (self.size_class.side() * k).round();
        let edge = attached.map(|spot| spot.edge);
        self.edge = edge;
        self.vertical = matches!(edge, Some(Edge::Left | Edge::Right));
        let margin = (EDGE_MARGIN * s).round();
        let (wl, wt, wr, wb) = work;
        let footer = if self.footer_shown() { FOOTER * k } else { 0.0 };
        let (pad, header, gap, spacing) = (PAD * k, HEADER * k, HEADER_GAP * k, SPACING * k);
        let across = (if self.vertical {
            2.0 * pad + self.card_px
        } else {
            2.0 * pad + header + gap + self.card_px + footer
        })
        .round();
        let screen_length = (if self.vertical { wb - wt } else { wr - wl }) as f32;
        let available = (screen_length - 2.0 * margin).max(120.0 * s);
        let n = self.cards.len() as f32;
        let natural = 2.0 * pad + n * self.card_px + (n - 1.0).max(0.0) * spacing;
        let length = (if self.vertical {
            available
        } else {
            natural.max(MIN_LENGTH * k).min(available)
        })
        .round();
        self.size = if self.vertical {
            (across.max(1.0) as usize, length.max(1.0) as usize)
        } else {
            (length.max(1.0) as usize, across.max(1.0) as usize)
        };
        let (lo, hi) = if self.vertical {
            (wt as f32, wb as f32)
        } else {
            (wl as f32, wr as f32)
        };
        let middle = match attached {
            Some(spot) if self.vertical => (spot.notch.1 + spot.notch.3) as f32 / 2.0,
            Some(spot) => (spot.notch.0 + spot.notch.2) as f32 / 2.0,
            None => (lo + hi) / 2.0,
        };
        let centre = middle
            .max(lo + margin + length / 2.0)
            .min(hi - margin - length / 2.0);
        match attached {
            Some(spot) => {
                let slack = (12.0 * s).ceil() as i32;
                let pose = render::emerging_body(
                    spot.edge,
                    spot.notch,
                    (centre, length, across),
                    middle,
                    slack,
                    dpi,
                );
                let tail = Tail {
                    edge: spot.edge,
                    offset: 0,
                };
                let at = render::picture_origin(tail, &pose, self.size, dpi);
                self.pose = Some(pose);
                self.origin = at;
            }
            None => {
                self.pose = None;
                self.origin = (0, 0);
                let (w, h) = (self.size.0 as i32, self.size.1 as i32);
                self.place = (
                    (centre - w as f32 / 2.0).round() as i32,
                    wb - h - (8.0 * s).round() as i32,
                );
            }
        }
    }

    /// Lays the strip out again where it is (the cards or the footer changed its length).
    fn relayout(&mut self) {
        let (spot, work, dpi, bottom) = (self.spot, self.work, self.dpi, self.bottom);
        self.layout(spot, work, dpi, bottom);
    }

    fn tail(&self) -> Option<Tail> {
        self.edge.map(|edge| Tail { edge, offset: 0 })
    }

    /// A window-local point in the picture's own coordinates.
    fn local(&self, x: f32, y: f32) -> (f32, f32) {
        (x - self.origin.0 as f32, y - self.origin.1 as f32)
    }

    /// The picture's top-left on the screen.
    fn screen_origin(&self) -> (i32, i32) {
        match &self.pose {
            Some(pose) => (pose.window.0 + self.origin.0, pose.window.1 + self.origin.1),
            None => self.place,
        }
    }

    /// The length of the cards' run along the strip, and the room there is for it.
    fn run_and_room(&self) -> (f32, f32) {
        let k = self.k;
        let n = self.cards.len() as f32;
        let run = n * self.card_px + (n - 1.0).max(0.0) * SPACING * k;
        let along = if self.vertical {
            self.size.1 as f32
        } else {
            self.size.0 as f32
        };
        let footer = if self.footer_shown() { FOOTER * k } else { 0.0 };
        let room = if self.vertical {
            along - (PAD * 2.0 + HEADER + HEADER_GAP) * k - footer
        } else {
            along - 2.0 * PAD * k
        };
        (run, room)
    }

    /// How far a still's run of cards is pulled back along the strip: half of what it overflows
    /// its room by (the Mac's frame centres what it cannot fit).
    fn overflow_shift(&self) -> f32 {
        if !self.still {
            return 0.0;
        }
        let (run, room) = self.run_and_room();
        -(run - room).max(0.0) / 2.0
    }

    fn clamp_scroll(&mut self) {
        if self.still {
            return;
        }
        let (run, room) = self.run_and_room();
        let most = (run - room).max(0.0);
        self.scroll = self.scroll.clamp(0.0, most);
        if let Some(goal) = self.scroll_goal.as_mut() {
            *goal = goal.clamp(0.0, most);
        }
    }

    /// Brings the selected card fully into view: at once while the strip is not yet on show,
    /// else on a glide.
    fn reveal(&mut self) {
        let step = self.card_px + SPACING * self.k;
        let start = self.selected as f32 * step;
        let (_, room) = self.run_and_room();
        let current = self.scroll_goal.unwrap_or(self.scroll);
        let mut goal = current;
        if start < current {
            goal = start;
        } else if start + self.card_px > current + room {
            goal = start + self.card_px - room;
        }
        let (run, room) = self.run_and_room();
        goal = goal.clamp(0.0, (run - room).max(0.0));
        if (goal - current).abs() < 0.5 {
            return;
        }
        if self.shown && !self.closing {
            self.scroll_goal = Some(goal);
            self.scroll_spring = Spring::at(self.scroll);
            self.wake();
        } else {
            self.scroll = goal;
            self.scroll_goal = None;
        }
    }

    /// Starts the animation timer when something is moving (a glide, a hover, a ring).
    fn wake(&self) {
        if self.shown {
            // SAFETY: a timer on this thread's own window.
            let _ = unsafe { SetTimer(Some(self.window.hwnd()), ANIM_TIMER, ANIM_MS, None) };
        }
    }

    /// Where the ring is for the selected card, along the strip.
    fn ring_goal(&self) -> f32 {
        self.selected as f32 * (self.card_px + SPACING * self.k)
    }

    // ---- drawing helpers ----------------------------------------------------------------

    /// The font's pixel size for `size` points in `face`.
    fn face_px(&self, size: f32, face: Face) -> i32 {
        // Consolas sets 0.55 em to a character and the Mac's SF Mono 0.602: the same advance.
        let size = if face == Face::Mono {
            size * 0.602 / 0.55
        } else {
            size
        };
        (size * self.k).round() as i32
    }

    fn mask(&mut self, text: &str, size: f32, face: Face) -> Option<Mask> {
        let px = self.face_px(size, face);
        match face {
            Face::Regular => self.painter.render(text, px, false),
            Face::Bold => self.painter.render(text, px, true),
            Face::Mono => self.painter.render_mono(text, px),
        }
    }

    /// Draws `mask` no wider than `room` pixels, its box centred on `mid_y`.
    fn put(
        canvas: &mut Canvas,
        mask: &Mask,
        x: f32,
        mid_y: f32,
        room: f32,
        color: u32,
        alpha: f32,
    ) {
        let width = mask.width.min(room.max(0.0) as usize);
        if width == 0 || mask.height == 0 {
            return;
        }
        let y = (mid_y - mask.height as f32 / 2.0).round() as i32;
        if width == mask.width {
            canvas.draw_mask(mask, x.round() as i32, y, color, alpha);
            return;
        }
        let mut cut = Mask {
            width,
            height: mask.height,
            coverage: Vec::with_capacity(width * mask.height),
        };
        for row in 0..mask.height {
            let start = row * mask.width;
            cut.coverage
                .extend_from_slice(&mask.coverage[start..start + width]);
        }
        canvas.draw_mask(&cut, x.round() as i32, y, color, alpha);
    }

    /// One line of text at `x`, centred on `mid_y`; returns its drawn width.
    #[allow(clippy::too_many_arguments)]
    fn line(
        &mut self,
        canvas: &mut Canvas,
        text: &str,
        size: f32,
        face: Face,
        at: (f32, f32),
        room: f32,
        color: u32,
        alpha: f32,
    ) -> f32 {
        match self.mask(text, size, face) {
            Some(mask) => {
                let width = mask.width.min(room.max(0.0) as usize) as f32;
                Self::put(canvas, &mask, at.0, at.1, room, color, alpha);
                width
            }
            None => 0.0,
        }
    }

    /// `text` broken into at most `lines` lines no wider than `room` pixels; a cut text ends in
    /// an ellipsis.
    fn wrap(&mut self, text: &str, size: f32, face: Face, room: f32, lines: usize) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut cut = false;
        // Only what could fit is measured: a long text is not worked through.
        let limit = lines * 90;
        let text: String = text.chars().take(limit).collect();
        'paragraphs: for paragraph in text.replace('\t', "    ").split('\n') {
            let mut current = String::new();
            // Leading spaces are kept: they are indentation.
            let body = paragraph.trim_start_matches(' ');
            let indent = " ".repeat(paragraph.len() - body.len());
            for (n, word) in body.split(' ').enumerate() {
                let word = &if n == 0 {
                    format!("{indent}{word}")
                } else {
                    word.to_string()
                };
                let candidate = if current.is_empty() {
                    word.to_string()
                } else {
                    format!("{current} {word}")
                };
                if self.width_of(&candidate, size, face) <= room {
                    current = candidate;
                    continue;
                }
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                    if out.len() >= lines {
                        cut = true;
                        break 'paragraphs;
                    }
                }
                // A word longer than the room is broken where it stops fitting.
                let mut piece = String::new();
                for c in word.chars() {
                    piece.push(c);
                    if self.width_of(&piece, size, face) > room && piece.chars().count() > 1 {
                        piece.pop();
                        out.push(std::mem::take(&mut piece));
                        piece.push(c);
                        if out.len() >= lines {
                            cut = true;
                            break 'paragraphs;
                        }
                    }
                }
                current = piece;
            }
            out.push(current);
            if out.len() >= lines {
                cut = out.len() > lines || paragraph_has_more(&text, &out);
                out.truncate(lines);
                break;
            }
        }
        if out.len() > lines {
            out.truncate(lines);
            cut = true;
        }
        if cut && let Some(last) = out.last_mut() {
            while last.chars().count() > 1
                && self.width_of(&format!("{last}\u{2026}"), size, face) > room
            {
                last.pop();
            }
            last.push('\u{2026}');
        }
        out
    }

    fn width_of(&mut self, text: &str, size: f32, face: Face) -> f32 {
        let px = self.face_px(size, face);
        (match face {
            Face::Regular => self.painter.measure(text, px, false),
            Face::Bold => self.painter.measure(text, px, true),
            Face::Mono => self.painter.measure_mono(text, px),
        }) as f32
    }

    // ---- thumbnails and icons -----------------------------------------------------------

    fn thumb(&mut self, id: &str) -> Option<(usize, usize, Arc<Vec<u32>>)> {
        if let Some(found) = self.thumbs.get(id) {
            return found.clone();
        }
        if self.thumbs.len() > 96 {
            self.thumbs.clear();
        }
        let path = clip::root().map(|root| root.join("thumbs").join(format!("{id}.png")));
        let loaded = path
            .and_then(|path| crate::seal::read(&path))
            .and_then(|png| {
                crate::seal::with_plain_file(&png, "png", |plain| {
                    clip_img::decode_file(plain, Some(clip_img::THUMB_SIDE))
                })
                .flatten()
            })
            .map(|bitmap| {
                (
                    bitmap.width as usize,
                    bitmap.height as usize,
                    Arc::new(clip_img::premultiplied(&bitmap)),
                )
            });
        self.thumbs.insert(id.to_string(), loaded.clone());
        loaded
    }

    /// A file item's paths, read once.
    fn paths_of(&mut self, id: &str) -> Vec<String> {
        if let Some(paths) = self.paths.get(id) {
            return paths.clone();
        }
        let paths = clip::with(|store| store.payload(id)).map_or_else(Vec::new, |p| p.paths);
        self.paths.insert(id.to_string(), paths.clone());
        paths
    }

    /// The band colour of a source app (spec 3.6): the dominant colour of its icon, cached.
    fn app_colour(&mut self, item: &Item) -> u32 {
        let Some(app) = &item.source else {
            return GREY;
        };
        if !self.colours_loaded {
            self.colours_loaded = true;
            if let Some(bytes) =
                clip::root().and_then(|r| crate::seal::read(&r.join("appcolours.json")))
                && let Some(json::Value::Object(members)) = json::parse(&bytes, 1 << 20)
            {
                for (id, value) in members {
                    if let Some(rgb) = value.as_str().and_then(rgb_of) {
                        self.colours.insert(id, rgb);
                    }
                }
            }
        }
        if let Some(colour) = self.colours.get(&app.id) {
            return *colour;
        }
        let Some(path) = clip_capture::app_path(&app.id) else {
            return GREY;
        };
        let colour = fileicon::of(&path).map_or(GREY, |icon| dominant(&icon.pixels));
        self.colours.insert(app.id.clone(), colour);
        self.save_colours();
        colour
    }

    fn save_colours(&self) {
        let Some(root) = clip::root() else {
            return;
        };
        let mut body = String::from("{");
        for (index, (id, rgb)) in self.colours.iter().enumerate() {
            if index > 0 {
                body.push(',');
            }
            body.push_str(&format!("{}:\"#{rgb:06X}\"", json_text(id)));
        }
        body.push('}');
        let _ = crate::seal::write(&root.join("appcolours.json"), body.as_bytes());
    }

    // ---- putting it on the screen -------------------------------------------------------

    /// Draws the strip again and puts it on the screen where the opening has got to.
    fn draw(&mut self) {
        // The ring, a hover or a glide may have a new goal; the timer rests itself when idle.
        self.wake();
        self.frame = Some(self.render());
        self.paint_frame();
    }

    fn paint_frame(&mut self) {
        let hwnd = self.window.hwnd();
        let Some(frame) = self.frame.as_ref() else {
            return;
        };
        let (slide, open) = (self.slide.spring.value, self.grow.spring.value);
        let result = match (&self.pose, self.tail()) {
            (Some(pose), Some(tail)) => {
                let canvas = render::render_emerging_picture(
                    frame,
                    tail,
                    pose,
                    (slide, open),
                    CORNER * self.k,
                    self.dpi,
                );
                present_alpha(hwnd, &canvas, (pose.window.0, pose.window.1), 255)
            }
            _ => {
                // Along the bottom there is no neck: the strip rises on the bulb's spring.
                let rise = (1.0 - slide.clamp(0.0, 1.2))
                    * (self.size.1 as f32 + 40.0 * layout::scale(self.dpi));
                let alpha = (slide.clamp(0.0, 1.0) * 255.0).round() as u8;
                present_alpha(
                    hwnd,
                    frame,
                    (self.place.0, self.place.1 + rise.round() as i32),
                    alpha,
                )
            }
        };
        if let Err(error) = result {
            host::log_error("UpdateLayeredWindow", &error, "clipboard_strip");
        }
    }

    // ---- opening and closing ------------------------------------------------------------

    fn open(&mut self, pick: Option<String>) {
        if !host::switch_on("launcherClipboard") {
            host::note("Clipboard history is off", true);
            return;
        }
        let mut point = POINT::default();
        let _ = unsafe { GetCursorPos(&mut point) };
        let Some(monitor) = rightkit_overlay::windows::monitor_at(point.x, point.y) else {
            return;
        };
        let dpi = monitor.dpi;
        let work = (
            monitor.work.left as i32,
            monitor.work.top as i32,
            monitor.work.right as i32,
            monitor.work.bottom as i32,
        );
        self.previous = hwnd_key(unsafe { GetForegroundWindow() });
        self.offset = rightkit_win_ui::clock::local_offset_secs();
        self.snap = clip::snapshot();
        self.size_class = CardSize::named(&host::card_size());
        self.numbers = host::switch_on("clipboardShowNumbers");
        // Filters reset when the strip closes; the board stays for the session, and across
        // sessions when asked.
        if !self.shown
            && host::switch_on("clipboardRememberBoard")
            && let Some(id) = self.snap.last_board.clone()
            && self.snap.boards.iter().any(|b| b.id == id)
        {
            self.view = View::Board(id);
        }
        if let View::Board(id) = &self.view
            && !self.snap.boards.iter().any(|b| b.id == *id)
        {
            self.view = View::History;
        }
        self.query.clear();
        self.searching = false;
        self.select_all = false;
        self.panel = Panel::default();
        self.popover = None;
        self.preview_wanted = false;
        self.preview = None;
        self.stack_open = None;
        self.naming = None;
        self.confirm = None;
        self.message = None;
        self.scroll = 0.0;
        self.scroll_goal = None;
        self.tab_scroll = 0.0;
        self.selected = 0;
        self.hovered = None;
        self.press = None;
        self.tab_drag = None;
        self.dragging = false;
        self.recompute();
        if let Some(id) = pick
            && let Some(at) = self
                .cards
                .iter()
                .position(|c| self.snap.items[c.item].id == id)
        {
            self.selected = at;
        }
        let spot = host::notch_spot_at((point.x, point.y));
        self.layout(spot, work, dpi, host::placement() == "bottom");
        // The first scroll is not a glide.
        self.shown = false;
        self.closing = false;
        self.reveal();
        self.shown = true;
        self.ring.settle(self.ring_goal());
        // The strip slides out on its neck, and opens a beat later.
        self.slide = Swell::new();
        self.grow = Swell::new();
        self.pslide = Swell::new();
        self.pgrow = Swell::new();
        self.slide.aim(1.0);
        self.grow.aim_after(1.0);
        self.last_tick = None;
        self.frame = Some(self.render());
        self.paint_frame();
        let hwnd = self.window.hwnd();
        // Opened from a hot key or the wheel, this thread has no right to the foreground of its
        // own: sharing the front window's input queue for the call gives it one.
        let front = unsafe { GetWindowThreadProcessId(hwnd_from_key(self.previous), None) };
        let own = unsafe { GetCurrentThreadId() };
        let joined =
            front != 0 && front != own && unsafe { ffi::AttachThreadInput(own, front, 1) } != 0;
        unsafe {
            ffi::SetWindowDisplayAffinity(hwnd.0, capture_affinity());
            ffi::SetWindowDisplayAffinity(self.preview_window.hwnd().0, capture_affinity());
            SHOWN_HWND.store(hwnd_key(hwnd), Ordering::Relaxed);
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
            ffi::SetFocus(hwnd.0);
        }
        if joined {
            // SAFETY: undoes the attachment made just above.
            unsafe { ffi::AttachThreadInput(own, front, 0) };
        }
        let _ = unsafe { SetTimer(Some(hwnd), ANIM_TIMER, ANIM_MS, None) };
        let _ = unsafe { SetTimer(Some(hwnd), CLOCK_TIMER, CLOCK_MS, None) };
    }

    /// Closes the strip. `animate` plays the opening backwards (the strip closes, then slides
    /// back into the notch); otherwise the contents go at once (a paste does not wait).
    /// `give_back` returns the foreground to the window that had it; a paste does that itself.
    fn close(&mut self, animate: bool, give_back: bool) {
        if !self.shown {
            return;
        }
        self.popover = None;
        self.press = None;
        if self.tab_drag.take().is_some() {
            // SAFETY: lets go of the capture this thread took for the drag.
            unsafe { ffi::ReleaseCapture() };
        }
        if animate && !self.closing {
            self.closing = true;
            self.hide_preview(true);
            self.grow.aim(0.0);
            self.slide.aim_after(0.0);
            self.pgrow.aim(0.0);
            self.pslide.aim_after(0.0);
            self.wake();
            if give_back {
                self.give_back();
            }
            return;
        }
        self.finish_close();
        if give_back {
            self.give_back();
        }
    }

    fn finish_close(&mut self) {
        self.shown = false;
        self.closing = false;
        let hwnd = self.window.hwnd();
        unsafe {
            let _ = KillTimer(Some(hwnd), ANIM_TIMER);
            let _ = KillTimer(Some(hwnd), CLOCK_TIMER);
            let _ = KillTimer(Some(hwnd), TOAST_TIMER);
            SHOWN_HWND.store(0, Ordering::Relaxed);
            let _ = ShowWindow(hwnd, SW_HIDE);
            let _ = ShowWindow(self.preview_window.hwnd(), SW_HIDE);
        }
        if host::switch_on("clipboardRememberBoard") {
            let id = match &self.view {
                View::Board(id) => Some(id.clone()),
                _ => None,
            };
            clip::with(|store| store.remember_board(id.as_deref()));
        }
        // What was drawn is not kept: the previews go with the window.
        self.frame = None;
        self.preview = None;
        self.preview_wanted = false;
        self.snap.items.clear();
        self.cards.clear();
        self.thumbs.clear();
        self.paths.clear();
        self.undo = None;
    }

    fn give_back(&self) {
        let previous = hwnd_from_key(self.previous);
        if self.previous != 0 && unsafe { IsWindow(Some(previous)) }.as_bool() {
            let _ = unsafe { SetForegroundWindow(previous) };
        }
    }

    /// One tick of the opening, the closing, a glide or a hover.
    fn animate(&mut self) {
        let now = Instant::now();
        let seconds = self.last_tick.map_or(ANIM_MS as f32 / 1000.0, |at| {
            now.duration_since(at).as_secs_f32().min(0.05)
        });
        self.last_tick = Some(now);
        let mut busy = self.slide.step();
        busy |= self.grow.step();
        if self.closing && !busy {
            self.finish_close();
            return;
        }
        busy |= self.pslide.step();
        busy |= self.pgrow.step();
        // The cards: hovers growing, the ring and the scroll gliding.
        let mut moving = false;
        for (index, spring) in self.hover_scale.iter_mut().enumerate() {
            let target = f32::from(u8::from(self.hovered == Some(index)));
            if !spring.resting_at(target) {
                spring.step(target, motion::EMPHASIS, seconds);
                moving = true;
            }
        }
        let ring_goal = self.ring_goal();
        if !self.ring.resting_within(ring_goal, 0.5, 5.0) {
            self.ring.step(ring_goal, motion::GLIDE, seconds);
            moving = true;
        } else if self.ring.value != ring_goal {
            self.ring.settle(ring_goal);
            moving = true;
        }
        if let Some(goal) = self.scroll_goal {
            self.scroll_spring.step(goal, motion::GLIDE, seconds);
            self.scroll = self.scroll_spring.value;
            if self.scroll_spring.resting_within(goal, 0.5, 5.0) {
                self.scroll = goal;
                self.scroll_goal = None;
            }
            moving = true;
        }
        if moving {
            self.frame = Some(self.render());
        }
        self.paint_frame();
        self.paint_preview();
        if !busy && !moving {
            // Nothing moves: the timer rests until something does.
            let _ = unsafe { KillTimer(Some(self.window.hwnd()), ANIM_TIMER) };
            if !self.preview_wanted && self.preview.is_some() {
                self.preview = None;
                let _ = unsafe { ShowWindow(self.preview_window.hwnd(), SW_HIDE) };
            }
        }
    }

    // ---- the preview --------------------------------------------------------------------

    /// Space and Ctrl+Y: the preview of the selected card, or away.
    fn toggle_preview(&mut self) {
        if self.preview_wanted {
            self.hide_preview(true);
        } else {
            self.preview_wanted = true;
            self.show_preview();
        }
    }

    fn hide_preview(&mut self, animate: bool) {
        self.preview_wanted = false;
        if animate && self.preview.is_some() {
            self.pgrow.aim(0.0);
            self.pslide.aim_after(0.0);
            self.wake();
        } else {
            self.preview = None;
            let _ = unsafe { ShowWindow(self.preview_window.hwnd(), SW_HIDE) };
        }
    }

    /// The preview follows the selected card: drawn again when the selection or the cards
    /// change.
    fn refresh_preview(&mut self) {
        if self.preview_wanted {
            self.show_preview();
        }
    }

    fn show_preview(&mut self) {
        let Some(item) = self.selected_item().cloned() else {
            self.hide_preview(false);
            return;
        };
        // The item's full text, or its picture; the strip's own cards hold only a preview.
        let payload = clip::with(|store| store.payload(&item.id));
        let text = match (&payload, item.kind) {
            (Some(payload), Kind::File) => payload.paths.join("\n"),
            (Some(payload), _) if !payload.text.is_empty() => payload.text.clone(),
            _ => item.preview.clone(),
        };
        let image = (item.kind == Kind::Image)
            .then(|| payload.as_ref().and_then(|p| p.png.as_ref()))
            .flatten()
            .and_then(|path| clip_img::decode_file(path, Some(560)))
            .map(|bitmap| {
                (
                    bitmap.width as usize,
                    bitmap.height as usize,
                    Arc::new(clip_img::premultiplied(&bitmap)),
                )
            });
        let picture = self.preview_picture(&item, &text, image);
        self.hang_preview(picture);
    }

    /// Where a preview `picture` hangs: on a neck from the strip's far side, centred on the
    /// strip and kept in from the display's ends.
    fn preview_pose(&self, picture: &Canvas) -> (render::Body, Edge) {
        let s = layout::scale(self.dpi);
        let (sx, sy) = self.screen_origin();
        let (sw, sh) = (self.size.0 as i32, self.size.1 as i32);
        let edge = self.edge.unwrap_or(Edge::Bottom);
        let lead = (PREVIEW_LEAD * s).round() as i32;
        // The strip, pulled in by the lead on the side the preview hangs from.
        let notch = match edge {
            Edge::Top => (sx, sy, sx + sw, sy + sh - lead),
            Edge::Bottom => (sx, sy + lead, sx + sw, sy + sh),
            Edge::Left => (sx, sy, sx + sw - lead, sy + sh),
            Edge::Right => (sx + lead, sy, sx + sw, sy + sh),
        };
        let vertical = matches!(edge, Edge::Left | Edge::Right);
        let (across, length) = if vertical {
            (picture.width as f32, picture.height as f32)
        } else {
            (picture.height as f32, picture.width as f32)
        };
        let strip_centre = if vertical {
            sy as f32 + sh as f32 / 2.0
        } else {
            sx as f32 + sw as f32 / 2.0
        };
        let (wl, wt, wr, wb) = self.work;
        let (lo, hi) = if vertical {
            (wt as f32, wb as f32)
        } else {
            (wl as f32, wr as f32)
        };
        let margin = (EDGE_MARGIN * s).round();
        let centre = strip_centre
            .max(lo + margin + length / 2.0)
            .min((hi - margin - length / 2.0).max(lo + margin + length / 2.0));
        let slack = (12.0 * s).ceil() as i32;
        let pose = render::emerging_body(
            edge,
            notch,
            (centre, length, across),
            strip_centre,
            slack,
            self.dpi,
        );
        (pose, edge)
    }

    /// Hangs `picture` from the strip's far side on a neck of its own.
    fn hang_preview(&mut self, picture: Canvas) {
        let (pose, edge) = self.preview_pose(&picture);
        let fresh = self.preview.is_none();
        if fresh {
            self.pslide = Swell::new();
            self.pgrow = Swell::new();
            self.pslide.aim(1.0);
            self.pgrow.aim_after(1.0);
        } else {
            self.pslide.aim(1.0);
            self.pgrow.aim(1.0);
        }
        self.preview = Some(Preview {
            picture,
            pose,
            edge,
        });
        self.paint_preview();
        if fresh {
            // SAFETY: calls on this thread's own window.
            unsafe {
                ffi::SetWindowDisplayAffinity(self.preview_window.hwnd().0, capture_affinity())
            };
            // SAFETY: shows this thread's own window without taking the foreground.
            let _ = unsafe { ShowWindow(self.preview_window.hwnd(), SW_SHOWNOACTIVATE) };
        }
        self.wake();
    }

    fn paint_preview(&mut self) {
        let Some(preview) = &self.preview else {
            return;
        };
        let tail = Tail {
            edge: preview.edge,
            offset: 0,
        };
        let canvas = render::render_emerging_picture(
            &preview.picture,
            tail,
            &preview.pose,
            (self.pslide.spring.value, self.pgrow.spring.value),
            (CARD_CORNER * self.k) + 2.0,
            self.dpi,
        );
        if let Err(error) = present_alpha(
            self.preview_window.hwnd(),
            &canvas,
            (preview.pose.window.0, preview.pose.window.1),
            255,
        ) {
            host::log_error("UpdateLayeredWindow", &error, "clipboard_preview");
        }
    }

    // ---- choosing -----------------------------------------------------------------------

    fn choose(&mut self, choice: Choice) {
        let Some(item) = self.selected_item() else {
            return;
        };
        let (id, kind) = (item.id.clone(), item.kind);
        // An image has no plain text: the shortcut pastes it as it is.
        let choice = if choice == Choice::Plain && kind == Kind::Image {
            Choice::Paste
        } else {
            choice
        };
        let previous = self.previous;
        // The contents go at once; the paste follows on its own thread.
        self.close(false, false);
        clip_paste::choose(&id, choice, previous);
    }

    /// Return: paste as copied, or as plain text when the setting swaps the two.
    fn choose_default(&mut self, shift: bool) {
        let swap = host::switch_on("clipboardPlainDefault");
        self.choose(if shift != swap {
            Choice::Plain
        } else {
            Choice::Paste
        });
    }

    fn delete_selected(&mut self) {
        let Some(card) = self.cards.get(self.selected).copied() else {
            return;
        };
        let id = self.snap.items[card.item].id.clone();
        if card.stack > 1 {
            let ids = clip::with(|store| store.stack_of(&id));
            self.confirm = Some(Confirm::Delete(ids));
            self.relayout();
            return;
        }
        self.delete_ids(&[id]);
    }

    fn delete_ids(&mut self, ids: &[String]) {
        let mut last = None;
        for id in ids {
            last = clip::with(|store| store.delete(id, clip::now())).or(last);
        }
        crate::win::clear_drag_copies();
        self.undo = if ids.len() == 1 { last } else { None };
        if self.undo.is_some() {
            self.flash(host::t("Deleted. Press Ctrl Z to undo."), false, 6);
        } else {
            self.say(host::t("Deleted"));
        }
        self.stack_open = None;
        self.refresh();
        self.after_change();
    }

    fn undo_delete(&mut self) {
        if let Some(undo) = self.undo.take()
            && clip::now() - undo.at <= clip::UNDO_SECONDS
        {
            clip::with(|store| store.restore(undo));
            self.message = None;
            self.refresh();
            self.after_change();
        }
    }

    /// The list changed: the strip's size, the scroll and the preview follow it.
    fn after_change(&mut self) {
        self.relayout();
        self.reveal();
        self.refresh_preview();
    }

    // ---- keys ---------------------------------------------------------------------------

    fn move_by(&mut self, delta: isize) -> Do {
        if self.cards.is_empty() {
            return Do::Nothing;
        }
        let last = self.cards.len() as isize - 1;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        self.reveal();
        self.refresh_preview();
        self.wake();
        Do::Redraw
    }

    fn set_view(&mut self, view: View) -> Do {
        self.view = view;
        self.stack_open = None;
        self.scroll = 0.0;
        self.scroll_goal = None;
        self.selected = 0;
        self.popover = None;
        self.recompute();
        self.after_change();
        Do::Redraw
    }

    /// The views in tab order: History, Pinned, then the boards.
    fn views(&self) -> Vec<View> {
        let mut views = vec![View::History, View::Pinned];
        views.extend(self.snap.boards.iter().map(|b| View::Board(b.id.clone())));
        views
    }

    /// The query or a filter changed: the list starts again from the first card.
    fn edited(&mut self) -> Do {
        self.select_all = false;
        self.selected = 0;
        self.scroll = 0.0;
        self.scroll_goal = None;
        self.stack_open = None;
        self.recompute();
        self.after_change();
        Do::Redraw
    }

    fn typed(&mut self, c: char) -> Do {
        if c < ' ' || c == '\u{7f}' || self.closing {
            return Do::Nothing;
        }
        if let Some(naming) = self.naming.as_mut() {
            if naming.text.len() < 40 {
                naming.text.push(c);
            }
            return Do::Redraw;
        }
        if self.confirm.is_some() {
            return Do::Nothing;
        }
        match self.popover.as_mut() {
            Some(Popover::Colour { hex, .. }) => {
                if (c == '#' || c.is_ascii_hexdigit()) && hex.chars().count() < 7 {
                    hex.push(c);
                }
                return Do::Redraw;
            }
            Some(Popover::Picker { query, selected }) => {
                query.push(c);
                *selected = 0;
                return Do::Redraw;
            }
            _ => {}
        }
        // Space with nothing typed is the preview's key.
        if c == ' ' && self.query.is_empty() && !self.searching {
            self.toggle_preview();
            return Do::Redraw;
        }
        if self.select_all {
            self.query.clear();
        }
        if self.query.len() < 200 {
            self.query.push(c);
        }
        self.searching = true;
        self.popover = None;
        self.edited()
    }

    /// Escape, in the order of what is open: a question, a board's name, a popover, the
    /// preview, the search and filters (the board stays), the strip.
    fn escape(&mut self) -> Do {
        if self.confirm.take().is_some() {
            self.relayout();
            return Do::Redraw;
        }
        if self.naming.take().is_some() {
            return Do::Redraw;
        }
        if self.popover.take().is_some() {
            return Do::Redraw;
        }
        if self.preview_wanted {
            self.hide_preview(true);
            return Do::Redraw;
        }
        if self.stack_open.is_some() {
            self.stack_open = None;
            self.recompute();
            self.after_change();
            return Do::Redraw;
        }
        if !self.query.is_empty() || self.searching || self.panel.active() {
            self.query.clear();
            self.searching = false;
            self.panel = Panel::default();
            return self.edited();
        }
        self.close(true, true);
        Do::Nothing
    }

    fn key(&mut self, vk: u32, ctrl: bool, shift: bool, alt: bool) -> Do {
        if self.closing {
            return Do::Nothing;
        }
        if vk == VK_ESCAPE {
            return self.escape();
        }
        if self.naming.is_some() {
            return self.naming_key(vk);
        }
        if let Some(confirm) = self.confirm.take() {
            if vk == VK_RETURN {
                match confirm {
                    Confirm::Delete(ids) => self.delete_ids(&ids),
                    Confirm::Board(id) => {
                        clip::with(|store| store.board_delete(&id));
                        if self.view == View::Board(id) {
                            self.view = View::History;
                        }
                        self.refresh();
                        self.recompute();
                        self.after_change();
                    }
                }
                self.relayout();
            } else {
                self.confirm = Some(confirm);
            }
            return Do::Redraw;
        }
        if matches!(self.popover, Some(Popover::Colour { .. })) {
            return self.hex_key(vk);
        }
        if matches!(self.popover, Some(Popover::Picker { .. })) && !ctrl {
            return self.picker_key(vk);
        }
        // Alt+Shift+1 to 9 paste that card as plain text. (Ctrl+Alt+digit is AltGr on many
        // layouts and types a character, so it is not a chord.)
        if alt && shift && !ctrl && (VK_1..=VK_9).contains(&vk) {
            let n = (vk - VK_1) as usize;
            if n < self.cards.len() {
                self.selected = n;
                self.choose(Choice::Plain);
            }
            return Do::Nothing;
        }
        // Ctrl+Alt is AltGr to Windows: only the reorder chords are taken, and every other
        // key reaches search typing through WM_CHAR.
        if ctrl && alt {
            return self.reorder(vk);
        }
        if ctrl {
            return self.ctrl_key(vk, shift);
        }
        match vk {
            VK_LEFT | VK_UP => self.move_by(-1),
            VK_RIGHT | VK_DOWN => self.move_by(1),
            VK_HOME => self.move_by(-(self.cards.len() as isize)),
            VK_END => self.move_by(self.cards.len() as isize),
            VK_TAB => {
                // Tab and Shift+Tab step through the views, as Ctrl+Tab does.
                self.step_view(if shift { -1 } else { 1 })
            }
            VK_RETURN if alt => {
                self.choose(Choice::CopyOnly);
                Do::Nothing
            }
            VK_RETURN => {
                let stack = self.cards.get(self.selected).is_some_and(|c| c.stack > 1);
                if stack && self.stack_open.is_none() {
                    let id = self
                        .selected_item()
                        .and_then(|i| i.stack_id().map(str::to_string));
                    self.stack_open = id;
                    self.selected = 0;
                    self.recompute();
                    self.after_change();
                    return Do::Redraw;
                }
                self.choose_default(shift);
                Do::Nothing
            }
            VK_BACK | VK_DELETE => {
                if self.query.is_empty() {
                    self.delete_selected();
                    Do::Redraw
                } else {
                    if self.select_all {
                        self.query.clear();
                    } else {
                        self.query.pop();
                    }
                    self.edited()
                }
            }
            _ => Do::Nothing,
        }
    }

    /// The next or previous view in tab order, wrapping around.
    fn step_view(&mut self, delta: isize) -> Do {
        let views = self.views();
        let at = views.iter().position(|v| *v == self.view).unwrap_or(0) as isize;
        let next = (at + delta).rem_euclid(views.len() as isize) as usize;
        self.set_view(views[next].clone())
    }

    fn ctrl_key(&mut self, vk: u32, shift: bool) -> Do {
        match vk {
            VK_1..=VK_9 => {
                let n = (vk - VK_1) as usize;
                if shift {
                    // Ctrl+Shift+digit does nothing: boards move on Ctrl+Tab.
                    return Do::Nothing;
                }
                if n < self.cards.len() {
                    self.selected = n;
                    self.choose_default(false);
                }
                Do::Nothing
            }
            VK_TAB => self.step_view(if shift { -1 } else { 1 }),
            VK_RETURN | VK_C => {
                self.choose(Choice::CopyOnly);
                Do::Nothing
            }
            VK_P => {
                if let Some(item) = self.selected_item() {
                    let (id, pinned) = (item.id.clone(), item.pinned);
                    clip::with(|store| store.set_pinned(&id, !pinned));
                    self.refresh();
                }
                Do::Redraw
            }
            VK_B => {
                if self.selected_item().is_some() {
                    self.popover = Some(Popover::Picker {
                        query: Vec::new(),
                        selected: 0,
                    });
                }
                Do::Redraw
            }
            VK_Y => {
                self.toggle_preview();
                Do::Redraw
            }
            VK_F => {
                self.searching = true;
                Do::Redraw
            }
            VK_A => {
                self.select_all = !self.query.is_empty();
                Do::Redraw
            }
            VK_Z => {
                self.undo_delete();
                Do::Redraw
            }
            VK_BACK => {
                self.delete_selected();
                Do::Redraw
            }
            _ => Do::Nothing,
        }
    }

    /// Ctrl+Alt+Left and Right: a board among the tabs, or a card inside a board.
    fn reorder(&mut self, vk: u32) -> Do {
        let delta = match vk {
            VK_LEFT => -1,
            VK_RIGHT => 1,
            _ => return Do::Nothing,
        };
        if let View::Board(id) = self.view.clone()
            && let Some(item) = self.selected_item()
        {
            let item_id = item.id.clone();
            clip::with(|store| store.board_item_move(&item_id, &id, delta));
            self.refresh();
            if let Some(at) = self
                .cards
                .iter()
                .position(|c| self.snap.items[c.item].id == item_id)
            {
                self.selected = at;
            }
            self.reveal();
        }
        Do::Redraw
    }

    fn naming_key(&mut self, vk: u32) -> Do {
        match vk {
            VK_BACK => {
                if let Some(naming) = self.naming.as_mut() {
                    naming.text.pop();
                }
            }
            VK_RETURN => {
                let Some(naming) = self.naming.take() else {
                    return Do::Nothing;
                };
                let name: String = naming.text.iter().collect();
                let result = match &naming.rename {
                    Some(id) => {
                        clip::with(|store| store.board_rename(id, name.trim()).map(|()| id.clone()))
                    }
                    None => clip::with(|store| store.board_create(name.trim(), clip::now())),
                };
                match result {
                    Ok(id) => {
                        if let Some(item) = &naming.add {
                            clip::with(|store| store.board_toggle(item, &id));
                        }
                        self.refresh();
                        self.after_change();
                    }
                    Err(why) => {
                        // The name stays to be fixed.
                        self.flash(host::t(why), true, 3);
                        self.naming = Some(naming);
                    }
                }
            }
            _ => return Do::Nothing,
        }
        Do::Redraw
    }

    /// Keys in a board colour's hex field (typed characters come through `typed`).
    fn hex_key(&mut self, vk: u32) -> Do {
        let Some(Popover::Colour { board, hex }) = self.popover.as_mut() else {
            return Do::Nothing;
        };
        match vk {
            VK_BACK => {
                hex.pop();
            }
            VK_RETURN => {
                let digits: String = hex.chars().filter(|c| *c != '#').collect();
                if digits.len() == 6 && digits.chars().all(|c| c.is_ascii_hexdigit()) {
                    let colour = format!("#{}", digits.to_uppercase());
                    let board = board.clone();
                    clip::with(|store| store.board_colour(&board, &colour));
                    self.popover = None;
                    self.refresh();
                    self.after_change();
                } else {
                    self.flash(host::t("Colours are #RRGGBB"), true, 3);
                }
            }
            _ => return Do::Nothing,
        }
        Do::Redraw
    }

    /// Keys in the board picker: the rows by arrows, Return chooses, Backspace edits.
    fn picker_key(&mut self, vk: u32) -> Do {
        let Some(Popover::Picker { query, selected }) = self.popover.as_mut() else {
            return Do::Nothing;
        };
        match vk {
            VK_UP => *selected = selected.saturating_sub(1),
            VK_DOWN => *selected += 1,
            VK_BACK => {
                query.pop();
                *selected = 0;
            }
            VK_RETURN => {
                let at = *selected;
                return self.pick_board(at);
            }
            _ => return Do::Nothing,
        }
        // The row count bounds the choice.
        let text: String = match &self.popover {
            Some(Popover::Picker { query, .. }) => query.iter().collect(),
            _ => String::new(),
        };
        let rows = self.picker_rows(&text).len();
        if let Some(Popover::Picker { selected, .. }) = self.popover.as_mut() {
            *selected = (*selected).min(rows.saturating_sub(1));
        }
        Do::Redraw
    }

    /// Puts the selected card on the picker's row `index`, or on a new board of the typed name.
    fn pick_board(&mut self, index: usize) -> Do {
        let Some(Popover::Picker { query, .. }) = &self.popover else {
            return Do::Nothing;
        };
        let text: String = query.iter().collect();
        let rows = self.picker_rows(&text);
        let (Some(row), Some(item)) = (rows.get(index), self.selected_item()) else {
            return Do::Nothing;
        };
        let item_id = item.id.clone();
        if row.new {
            let title = row.title.clone();
            match clip::with(|store| store.board_create(&title, clip::now())) {
                Ok(board) => {
                    clip::with(|store| store.board_toggle(&item_id, &board));
                }
                Err(why) => {
                    self.flash(host::t(why), true, 3);
                    return Do::Redraw;
                }
            }
        } else {
            let board = row.id.clone();
            clip::with(|store| store.board_toggle(&item_id, &board));
        }
        self.popover = None;
        self.refresh();
        self.after_change();
        Do::Redraw
    }

    // ---- the mouse ----------------------------------------------------------------------

    fn card_at(&self, x: f32, y: f32) -> Option<usize> {
        if x < 0.0 || y < 0.0 || x >= self.size.0 as f32 || y >= self.size.1 as f32 {
            return None;
        }
        self.card_rects.iter().position(|rect| rect.has(x, y))
    }

    /// The pointer pressed at `(x, y)` in the picture.
    fn press(&mut self, x: f32, y: f32) -> Do {
        if self.naming.is_some() || self.confirm.is_some() || self.closing {
            return Do::Nothing;
        }
        // A popover's own controls come first.
        if let Some(frame) = self.popover_rect
            && frame.has(x, y)
        {
            let hit = self
                .hits
                .iter()
                .find(|(rect, _)| rect.has(x, y))
                .map(|(_, hit)| hit.clone());
            return match hit {
                Some(hit) => self.apply_hit(hit),
                None => Do::Nothing,
            };
        }
        let hit = self
            .tabs
            .iter()
            .find(|(rect, _, _)| rect.has(x, y))
            .map(|(_, tab, board)| (tab.clone(), board.clone()));
        if let Some((tab, board)) = hit {
            return match tab {
                Tab::View(view) => {
                    if let Some(id) = &board {
                        // A second press on a board's tab renames it; a drag moves it.
                        let double = self.last_tab.as_ref().is_some_and(|(at, was)| {
                            was == id
                                && at.elapsed()
                                    < Duration::from_millis(u64::from(unsafe {
                                        ffi::GetDoubleClickTime()
                                    }))
                        });
                        self.last_tab = Some((Instant::now(), id.clone()));
                        self.press = Some((PressOn::Tab(id.clone()), x, y));
                        if double {
                            self.last_tab = None;
                            self.press = None;
                            return self.begin_rename(id);
                        }
                    }
                    self.set_view(view)
                }
                Tab::New => {
                    self.popover = None;
                    self.naming = Some(Naming {
                        text: Vec::new(),
                        rename: None,
                        add: None,
                    });
                    Do::Redraw
                }
                Tab::Search => {
                    self.searching = true;
                    Do::Redraw
                }
                Tab::Filter => {
                    self.popover = match self.popover {
                        Some(Popover::Filters) => None,
                        _ => Some(Popover::Filters),
                    };
                    Do::Redraw
                }
            };
        }
        if let Some(rect) = self.action_rect
            && rect.has(x, y)
        {
            // "Show folder" under an unreadable index.
            if let Some(root) = clip::root() {
                open_path(&root);
            }
            return Do::Nothing;
        }
        match self.card_at(x, y) {
            Some(index) => {
                let changed = self.selected != index;
                self.selected = index;
                self.press = Some((PressOn::Card(index), x, y));
                let double = self.last_click.is_some_and(|(at, was)| {
                    was == index
                        && at.elapsed()
                            < Duration::from_millis(u64::from(unsafe { ffi::GetDoubleClickTime() }))
                });
                self.last_click = Some((Instant::now(), index));
                if double {
                    self.last_click = None;
                    self.press = None;
                    return self.key(VK_RETURN, false, false, false);
                }
                if changed {
                    self.refresh_preview();
                    self.wake();
                }
                Do::Redraw
            }
            None => Do::Nothing,
        }
    }

    fn begin_rename(&mut self, id: &str) -> Do {
        let name = self
            .snap
            .boards
            .iter()
            .find(|b| b.id == id)
            .map_or(String::new(), |b| b.name.clone());
        self.popover = None;
        self.naming = Some(Naming {
            text: name.chars().collect(),
            rename: Some(id.to_string()),
            add: None,
        });
        Do::Redraw
    }

    /// A popover's control was pressed.
    fn apply_hit(&mut self, hit: PopHit) -> Do {
        match hit {
            PopHit::Kind(None) => self.panel.kinds.clear(),
            PopHit::Kind(Some(kind)) => {
                let multi = modifier(VK_CONTROL) || modifier(VK_SHIFT);
                if multi {
                    match self.panel.kinds.iter().position(|k| *k == kind) {
                        Some(at) => {
                            self.panel.kinds.remove(at);
                        }
                        None => self.panel.kinds.push(kind),
                    }
                } else if self.panel.kinds == [kind] {
                    self.panel.kinds.clear();
                } else {
                    self.panel.kinds = vec![kind];
                }
            }
            PopHit::App(id) => match self.panel.apps.iter().position(|a| *a == id) {
                Some(at) => {
                    self.panel.apps.remove(at);
                }
                None => self.panel.apps.push(id),
            },
            PopHit::Date(date) => self.panel.date = date,
            PopHit::Swatch(n) => {
                if let Some(Popover::Colour { board, .. }) = self.popover.clone() {
                    let hex = clip::PALETTE[n].1;
                    clip::with(|store| store.board_colour(&board, hex));
                    self.refresh();
                    self.after_change();
                }
                return Do::Redraw;
            }
            PopHit::Hex => return Do::Redraw,
            PopHit::PickerRow(n) => return self.pick_board(n),
        }
        // A filter changed: the list starts again.
        self.edited()
    }

    fn right_click(&mut self, x: f32, y: f32, screen: (i32, i32)) -> Do {
        if self.naming.is_some() || self.closing {
            return Do::Nothing;
        }
        let tab = self
            .tabs
            .iter()
            .find(|(rect, _, board)| board.is_some() && rect.has(x, y))
            .and_then(|(_, _, board)| board.clone());
        if let Some(id) = tab {
            return Do::Menu(MenuFor::Tab(id), screen);
        }
        match self.card_at(x, y) {
            Some(index) => {
                self.selected = index;
                let id = self.snap.items[self.cards[index].item].id.clone();
                Do::Menu(MenuFor::Card(id), screen)
            }
            None => Do::Nothing,
        }
    }

    fn wheel(&mut self, x: f32, y: f32, delta: i32) -> Do {
        let k = self.k;
        let step = delta as f32 / 120.0;
        // Over the header the tabs move; elsewhere the cards.
        if y < (PAD + HEADER + HEADER_GAP / 2.0) * k && x < self.size.0 as f32 {
            self.tab_scroll -= step * 60.0 * k;
            return Do::Redraw;
        }
        let half = (self.card_px + SPACING * k) / 2.0;
        self.scroll_goal = None;
        self.scroll -= step * half;
        self.clamp_scroll();
        self.scroll_spring = Spring::at(self.scroll);
        Do::Redraw
    }

    /// The pointer moved to `(x, y)` with the left button `down`.
    fn moved(&mut self, x: f32, y: f32, down: bool) -> Do {
        if down && let Some((on, px, py)) = &self.press {
            let far = DRAG_DISTANCE * layout::scale(self.dpi);
            if (x - px).hypot(y - py) > far {
                match on {
                    PressOn::Card(index) => {
                        let index = *index;
                        self.press = None;
                        return Do::Drag(index);
                    }
                    PressOn::Tab(id) => {
                        if self.tab_drag.is_none() {
                            self.tab_drag = Some(id.clone());
                            // SAFETY: takes the mouse for this thread's own window until the
                            // button is let go.
                            unsafe { ffi::SetCapture(self.window.hwnd().0) };
                        }
                    }
                }
            }
        }
        if !down {
            self.press = None;
        }
        let now = self.card_at(x, y);
        if now != self.hovered {
            self.hovered = now;
            self.wake();
        }
        Do::Nothing
    }

    /// The button came up at `(x, y)`: a board tab dropped on another moves it there.
    fn released(&mut self, x: f32, y: f32) -> Do {
        self.press = None;
        let Some(id) = self.tab_drag.take() else {
            return Do::Nothing;
        };
        // SAFETY: lets go of the capture taken for the drag.
        unsafe { ffi::ReleaseCapture() };
        let target = self
            .tabs
            .iter()
            .find(|(rect, _, board)| board.is_some() && rect.has(x, y))
            .and_then(|(_, _, board)| board.clone());
        if let Some(target) = target
            && target != id
        {
            let order: Vec<&str> = self.snap.boards.iter().map(|b| b.id.as_str()).collect();
            if let (Some(from), Some(to)) = (
                order.iter().position(|b| *b == id),
                order.iter().position(|b| *b == target),
            ) {
                let delta = to as i32 - from as i32;
                clip::with(|store| store.board_move(&id, delta));
                self.refresh();
                self.after_change();
                return Do::Redraw;
            }
        }
        Do::Nothing
    }

    // ---- drag ---------------------------------------------------------------------------

    /// What a drag of card `index` carries and where it can land inside the strip (screen
    /// coordinates), or nothing when the card cannot be read.
    fn begin_drag(&mut self, index: usize) -> Option<(drag::Drag, String)> {
        let item = self.snap.items.get(self.cards.get(index)?.item)?.clone();
        let formats = drag::formats_for(&item)?;
        let (ox, oy) = self.screen_origin();
        let shift = |rect: &Rect| {
            [
                ox + rect.x.round() as i32,
                oy + rect.y.round() as i32,
                ox + (rect.x + rect.w).round() as i32,
                oy + (rect.y + rect.h).round() as i32,
            ]
        };
        let mut tabs: Vec<([i32; 4], String)> = Vec::new();
        for (rect, tab, board) in &self.tabs {
            match (tab, board) {
                (_, Some(id)) => tabs.push((shift(rect), id.clone())),
                (Tab::View(View::History), _) => tabs.push((shift(rect), "history".to_string())),
                (Tab::View(View::Pinned), _) => tabs.push((shift(rect), "pinned".to_string())),
                _ => {}
            }
        }
        // Cards take a drop only in a board's own view.
        let cards = if matches!(self.view, View::Board(_)) {
            self.card_rects
                .iter()
                .enumerate()
                .map(|(n, rect)| (shift(rect), n))
                .collect()
        } else {
            Vec::new()
        };
        self.dragging = true;
        Some((
            drag::Drag {
                formats,
                tabs,
                cards,
            },
            item.id,
        ))
    }

    /// The drag is over: what it landed on inside the strip is done here; one accepted by
    /// another app closes the strip.
    fn end_drag(&mut self, outcome: drag::Outcome, item_id: &str, from: usize) {
        self.dragging = false;
        match outcome.landing {
            Some(drag::Landing::Tab(tab)) => {
                match tab.as_str() {
                    "history" => {}
                    "pinned" => clip::with(|store| store.set_pinned(item_id, true)),
                    board => {
                        let on = self
                            .snap
                            .items
                            .iter()
                            .find(|i| i.id == item_id)
                            .is_some_and(|i| i.boards.iter().any(|b| b == board));
                        if !on {
                            clip::with(|store| store.board_toggle(item_id, board));
                        }
                    }
                }
                // A drop on a board tab keeps the strip open.
                self.refresh();
                self.after_change();
            }
            Some(drag::Landing::Card(to)) => {
                if let View::Board(board) = self.view.clone()
                    && to != from
                {
                    let delta = to as i32 - from as i32;
                    clip::with(|store| store.board_item_move(item_id, &board, delta));
                    self.refresh();
                    self.after_change();
                }
            }
            None if outcome.accepted => self.close(true, false),
            None => {}
        }
    }

    // ---- menus --------------------------------------------------------------------------

    fn menu_entries(&self, what: &MenuFor) -> Vec<Entry> {
        let boards_menu = |item: Option<&Item>| -> Vec<Entry> {
            let mut list: Vec<Entry> = self
                .snap
                .boards
                .iter()
                .enumerate()
                .map(|(n, board)| {
                    let mut entry = Entry::line(CMD_BOARD + n as u32, board.name.clone());
                    entry.checked = item.is_some_and(|i| i.boards.contains(&board.id));
                    entry
                })
                .collect();
            if !list.is_empty() {
                list.push(Entry::rule());
            }
            list.push(Entry::line(CMD_NEW_BOARD, host::t("New board...")));
            list
        };
        match what {
            MenuFor::Card(id) => {
                let Some(item) = self.snap.items.iter().find(|i| i.id == *id) else {
                    return Vec::new();
                };
                let mut entries = vec![Entry::line(
                    CMD_PASTE,
                    format!("{}\tReturn", host::t("Paste")),
                )];
                if item.kind != Kind::Image {
                    entries.push(Entry::line(
                        CMD_PLAIN,
                        format!("{}\tShift+Return", host::t("Paste as plain text")),
                    ));
                }
                entries.push(Entry::line(
                    CMD_COPY,
                    format!("{}\tCtrl+C", host::t("Copy")),
                ));
                entries.push(Entry::line(
                    CMD_PREVIEW,
                    format!("{}\tCtrl+Y", host::t("Preview")),
                ));
                entries.push(Entry::rule());
                entries.push(Entry::line(
                    CMD_PIN,
                    format!(
                        "{}\tCtrl+P",
                        if item.pinned {
                            host::t("Unpin")
                        } else {
                            host::t("Pin")
                        }
                    ),
                ));
                let mut add = Entry::line(0, host::t("Add to board"));
                add.children = boards_menu(Some(item));
                entries.push(add);
                if matches!(self.view, View::Board(_)) {
                    entries.push(Entry::line(CMD_UNBOARD, host::t("Remove from board")));
                }
                if item.kind == Kind::Image {
                    entries.push(Entry::line(
                        CMD_LANE,
                        if item.screenshot {
                            host::t("Not a screenshot")
                        } else {
                            host::t("Keep as screenshot")
                        },
                    ));
                }
                entries.push(Entry::rule());
                match item.kind {
                    Kind::Link => entries.push(Entry::line(CMD_OPEN, host::t("Open"))),
                    Kind::File => entries.push(Entry::line(CMD_OPEN, host::t("Reveal"))),
                    _ => {}
                }
                entries.push(Entry::line(
                    CMD_DELETE,
                    format!("{}\tCtrl+Backspace", host::t("Delete")),
                ));
                entries
            }
            MenuFor::Tab(_) => vec![
                Entry::line(CMD_RENAME, host::t("Rename")),
                Entry::line(CMD_COLOUR, host::t("Colour")),
                Entry::line(CMD_LEFT, format!("{}\tCtrl+Alt+Left", host::t("Move left"))),
                Entry::line(
                    CMD_RIGHT,
                    format!("{}\tCtrl+Alt+Right", host::t("Move right")),
                ),
                Entry::rule(),
                Entry::line(CMD_DEL_BOARD, host::t("Delete board")),
            ],
        }
    }

    /// Does what a chosen menu line says.
    fn menu_command(&mut self, what: &MenuFor, command: u32) {
        match what {
            MenuFor::Card(id) => match command {
                CMD_PASTE => self.choose(Choice::Paste),
                CMD_PLAIN => self.choose(Choice::Plain),
                CMD_COPY => self.choose(Choice::CopyOnly),
                CMD_PREVIEW => {
                    if !self.preview_wanted {
                        self.preview_wanted = true;
                    }
                    self.show_preview();
                    return;
                }
                CMD_PIN => {
                    let pinned = self
                        .snap
                        .items
                        .iter()
                        .find(|i| i.id == *id)
                        .is_some_and(|i| i.pinned);
                    clip::with(|store| store.set_pinned(id, !pinned));
                }
                CMD_UNBOARD => {
                    if let View::Board(board) = self.view.clone() {
                        clip::with(|store| store.board_toggle(id, &board));
                    }
                }
                CMD_LANE => {
                    let screenshot = self
                        .snap
                        .items
                        .iter()
                        .find(|i| i.id == *id)
                        .is_some_and(|i| i.screenshot);
                    clip::with(|store| store.set_lane(id, !screenshot));
                }
                CMD_OPEN => clip_paste::open_item(id),
                CMD_DELETE => {
                    self.delete_selected();
                    return;
                }
                CMD_NEW_BOARD => {
                    self.naming = Some(Naming {
                        text: Vec::new(),
                        rename: None,
                        add: Some(id.clone()),
                    });
                    return;
                }
                n if (CMD_BOARD..CMD_BOARD + 100).contains(&n) => {
                    if let Some(board) = self.snap.boards.get((n - CMD_BOARD) as usize) {
                        let board = board.id.clone();
                        clip::with(|store| store.board_toggle(id, &board));
                    }
                }
                _ => return,
            },
            MenuFor::Tab(board) => match command {
                CMD_RENAME => {
                    self.begin_rename(board);
                    return;
                }
                CMD_COLOUR => {
                    self.popover = Some(Popover::Colour {
                        board: board.clone(),
                        hex: String::new(),
                    });
                    return;
                }
                CMD_LEFT => clip::with(|store| store.board_move(board, -1)),
                CMD_RIGHT => clip::with(|store| store.board_move(board, 1)),
                CMD_DEL_BOARD => {
                    self.confirm = Some(Confirm::Board(board.clone()));
                    self.relayout();
                    return;
                }
                _ => return,
            },
        }
        if self.shown {
            self.refresh();
            self.after_change();
        }
    }

    // ---- the hot key --------------------------------------------------------------------

    fn sync_hotkey(&mut self) {
        // Held only while the history and the strip are both on: a key is never taken from other
        // programs for someone who has not turned the clipboard on.
        let want = (host::switch_on("launcherClipboard")
            && host::switch_on("clipboardStripEnabled"))
        .then(|| host::hotkey_text().to_string());
        let wanted_text = want.clone().unwrap_or_default();
        if wanted_text == self.hot_text
            && (want.is_none() || self.hot.is_some() || !hotkey_status().is_empty())
        {
            if want.is_none() && self.shown {
                self.close(false, true);
            }
            return;
        }
        if let (Some(keys), Some(old)) = (&self.keys, self.hot.take()) {
            let _ = keys.unregister(&old);
        }
        self.hot_text = wanted_text;
        let Some(text) = want else {
            set_status("");
            if self.shown {
                self.close(false, true);
            }
            return;
        };
        let Some(chord_text) = clip::parse_hotkey(&text).map(|h| h.kit_text()) else {
            set_status("");
            return;
        };
        if self.keys.is_none() {
            // One press, one toggle: the loop's thread only posts to this one.
            self.keys = GlobalHotkeys::new(|_| post(WM_TOGGLE)).ok();
        }
        let asked = rightkit_hotkeys::parse_chord(&chord_text).ok();
        let outcome = match (&self.keys, &asked) {
            (Some(keys), Some(asked)) => keys.register(asked).map_err(|error| error.message()),
            _ => Err("no hot key loop".to_string()),
        };
        let taken = outcome.is_ok();
        if let Err(why) = &outcome {
            host::log_info(
                "clipboard_hotkey_refused",
                &[("chord", chord_text.as_str()), ("why", why.as_str())],
            );
        }
        self.hot = asked.filter(|_| taken);
        set_status(if taken {
            ""
        } else {
            host::t("In use by another program")
        });
    }
}

/// How the strip is toggled by the hot key: closes when open, else opens (history off is a
/// toast, never a strip).
fn toggle() {
    let was_open = with_strip(|strip| strip.shown && !strip.closing).unwrap_or(false);
    if was_open {
        with_strip(|strip| strip.close(true, true));
    } else {
        with_strip(|strip| strip.open(None));
    }
}

/// Opens the clipboard history folder in Explorer; the scope is the folder itself.
fn open_path(path: &std::path::Path) {
    if let Ok(scope) = rightkit_child_process::reveal::PathScope::new([path]) {
        let _ = rightkit_child_process::reveal::open_folder(path, &scope);
    }
}

fn json_text(text: &str) -> String {
    let mut out = String::from("\"");
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push(' '),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The path part of a link, for the card (what follows the host).
fn link_path(url: &str) -> String {
    let rest = url.split_once("://").map_or(url, |(_, rest)| rest);
    match rest.find(['/', '?', '#']) {
        Some(at) => rest[at..].to_string(),
        None => String::new(),
    }
}

/// The dominant colour of an icon (spec 3.6): pixels with alpha of at least 0.5 and a
/// lightness between 0.1 and 0.95 are sorted into 24 hue buckets; the fullest bucket's pixels
/// are averaged. Grey when none qualify.
fn dominant(pixels: &[u32]) -> u32 {
    let mut buckets = [(0u32, 0u32, 0u32, 0u32); 24];
    for pixel in pixels {
        let alpha = (pixel >> 24) & 0xFF;
        if alpha < 128 {
            continue;
        }
        // The image is premultiplied: the colour is taken back to straight.
        let straight = |shift: u32| (((pixel >> shift) & 0xFF) * 255 / alpha).min(255);
        let (r, g, b) = (straight(16), straight(8), straight(0));
        let (max, min) = (
            r.max(g).max(b) as f32 / 255.0,
            r.min(g).min(b) as f32 / 255.0,
        );
        let lightness = (max + min) / 2.0;
        if !(0.1..=0.95).contains(&lightness) {
            continue;
        }
        let delta = max - min;
        let hue = if delta == 0.0 {
            0.0
        } else if (max - r as f32 / 255.0).abs() < f32::EPSILON {
            ((g as f32 - b as f32) / 255.0 / delta).rem_euclid(6.0)
        } else if (max - g as f32 / 255.0).abs() < f32::EPSILON {
            (b as f32 - r as f32) / 255.0 / delta + 2.0
        } else {
            (r as f32 - g as f32) / 255.0 / delta + 4.0
        };
        let bucket = ((hue / 6.0 * 24.0) as usize).min(23);
        let slot = &mut buckets[bucket];
        slot.0 += r;
        slot.1 += g;
        slot.2 += b;
        slot.3 += 1;
    }
    match buckets.iter().max_by_key(|slot| slot.3) {
        Some(&(r, g, b, n)) if n > 0 => ((r / n) << 16) | ((g / n) << 8) | (b / n),
        _ => GREY,
    }
}

/// Whether the text after the cut line still has words (so the last shown line gets an
/// ellipsis).
fn paragraph_has_more(text: &str, shown: &[String]) -> bool {
    let joined: usize = shown.iter().map(|line| line.chars().count() + 1).sum();
    text.chars().count() > joined
}

/// Publishes `canvas` as the window's contents with per-pixel alpha and an overall `alpha`
/// (the fade), the window's top-left at `at`; the window takes the canvas's size.
fn present_alpha(
    hwnd: HWND,
    canvas: &Canvas,
    at: (i32, i32),
    alpha: u8,
) -> Result<(), windows::core::Error> {
    let mut surface = Surface::new(canvas.width, canvas.height)
        .ok_or_else(|| windows::core::Error::from(windows::Win32::Foundation::E_FAIL))?;
    surface.copy_from(canvas);
    let size = SIZE {
        cx: canvas.width as i32,
        cy: canvas.height as i32,
    };
    let source = POINT { x: 0, y: 0 };
    let destination = POINT { x: at.0, y: at.1 };
    let blend = BLENDFUNCTION {
        BlendOp: 0, // AC_SRC_OVER
        BlendFlags: 0,
        SourceConstantAlpha: alpha,
        AlphaFormat: 1, // AC_SRC_ALPHA: the bitmap carries premultiplied alpha
    };
    // SAFETY: all pointers refer to locals that outlive the synchronous call; the source DC
    // holds the DIB just filled.
    unsafe {
        UpdateLayeredWindow(
            hwnd,
            None,
            Some(&destination as *const POINT),
            Some(&size),
            Some(surface.dc()),
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        )
    }
}

// ---- the menu, outside the strip's borrow -------------------------------------------------

fn build_menu(entries: &[Entry]) -> ffi::Handle {
    // SAFETY: plain menu calls; every text is NUL-terminated and outlives its call. A submenu
    // belongs to its parent, which destroys it with itself.
    unsafe {
        let menu = ffi::CreatePopupMenu();
        for entry in entries {
            if entry.separator {
                ffi::AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                continue;
            }
            let text = wide(&entry.label);
            let mut flags = 0;
            if entry.checked {
                flags |= MF_CHECKED;
            }
            if entry.disabled {
                flags |= MF_GRAYED;
            }
            if entry.children.is_empty() {
                ffi::AppendMenuW(menu, flags, entry.id as usize, text.as_ptr());
            } else {
                let sub = build_menu(&entry.children);
                ffi::AppendMenuW(menu, flags | MF_POPUP, sub as usize, text.as_ptr());
            }
        }
        menu
    }
}

/// Shows the menu for `what` at the pointer (or `at`) and does what is chosen.
fn run_menu(what: MenuFor, at: (i32, i32)) {
    let Some((entries, hwnd)) = with_strip(|strip| {
        strip.in_menu = true;
        (strip.menu_entries(&what), strip.window.hwnd())
    }) else {
        return;
    };
    let mut command = 0u32;
    if !entries.is_empty() {
        let menu = build_menu(&entries);
        // SAFETY: the menu is ours; the window is this thread's. The call returns the chosen
        // command because of TPM_RETURNCMD, and the menu is destroyed after.
        unsafe {
            command = ffi::TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                at.0,
                at.1,
                0,
                hwnd.0,
                std::ptr::null(),
            ) as u32;
            ffi::DestroyMenu(menu);
        }
    }
    with_strip(|strip| {
        strip.in_menu = false;
        if command != 0 {
            strip.menu_command(&what, command);
        }
        if strip.shown {
            strip.draw();
        }
    });
}

fn carry_out(action: Do) {
    match action {
        Do::Nothing => {}
        Do::Redraw => {
            with_strip(|strip| {
                if strip.shown {
                    strip.draw();
                }
            });
        }
        Do::Menu(what, at) => run_menu(what, at),
        Do::Drag(index) => {
            let Some(Some((carried, id))) = with_strip(|strip| strip.begin_drag(index)) else {
                return;
            };
            // The drag runs its own message loop: the strip is not borrowed while it does.
            let outcome = drag::run(carried);
            with_strip(|strip| {
                strip.end_drag(outcome, &id, index);
                if strip.shown {
                    strip.draw();
                }
            });
        }
    }
}

// ---- the window ---------------------------------------------------------------------------

fn modifier(key: VIRTUAL_KEY) -> bool {
    // SAFETY: a plain query of the key's state.
    let state = unsafe { GetKeyState(i32::from(key.0)) };
    state < 0
}

/// The preview window draws nothing itself: its picture is put on it by the strip.
extern "system" fn preview_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_ERASEBKGND => LRESULT(1),
        WM_PAINT => {
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            LRESULT(0)
        }
        // SAFETY: default processing of the unchanged message.
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

extern "system" fn strip_proc(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    let vk = wparam.0 as u32;
    let x = (lparam.0 & 0xFFFF) as i16 as f32;
    let y = ((lparam.0 >> 16) & 0xFFFF) as i16 as f32;
    match message {
        WM_ERASEBKGND => return LRESULT(1),
        WM_PAINT => {
            let _ = unsafe { ValidateRect(Some(hwnd), None) };
            return LRESULT(0);
        }
        WM_ACTIVATE if wparam.0 & 0xFFFF == 0 => {
            // Another app took the foreground: it keeps it. A menu of ours does not count, and
            // neither does a card on its way to another app.
            with_strip(|strip| {
                if !strip.in_menu && !strip.dragging {
                    strip.close(false, false);
                }
            });
            return LRESULT(0);
        }
        WM_WTSSESSION_CHANGE if wparam.0 == WTS_SESSION_LOCK => {
            with_strip(|strip| strip.close(false, false));
            return LRESULT(0);
        }
        WM_CLOSE => {
            with_strip(|strip| strip.close(false, true));
            return LRESULT(0);
        }
        WM_CHAR => {
            if let Some(c) = char::from_u32(vk) {
                let action = with_strip(|strip| strip.typed(c));
                carry_out(action.unwrap_or(Do::Nothing));
            }
            return LRESULT(0);
        }
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let (ctrl, shift) = (modifier(VK_CONTROL), modifier(VK_SHIFT));
            let alt = message == WM_SYSKEYDOWN || modifier(VK_MENU);
            let action = with_strip(|strip| strip.key(vk, ctrl, shift, alt));
            carry_out(action.unwrap_or(Do::Nothing));
            return LRESULT(0);
        }
        // No menu to open and no beep for a letter typed with Alt.
        WM_SYSCHAR => return LRESULT(0),
        WM_LBUTTONDOWN => {
            let action = with_strip(|strip| {
                let (x, y) = strip.local(x, y);
                strip.press(x, y)
            });
            carry_out(action.unwrap_or(Do::Nothing));
            return LRESULT(0);
        }
        WM_LBUTTONUP => {
            let action = with_strip(|strip| {
                let (x, y) = strip.local(x, y);
                strip.released(x, y)
            });
            carry_out(action.unwrap_or(Do::Nothing));
            return LRESULT(0);
        }
        WM_RBUTTONUP => {
            let mut screen = POINT::default();
            let _ = unsafe { GetCursorPos(&mut screen) };
            let action = with_strip(|strip| {
                let (x, y) = strip.local(x, y);
                strip.right_click(x, y, (screen.x, screen.y))
            });
            carry_out(action.unwrap_or(Do::Nothing));
            return LRESULT(0);
        }
        WM_MOUSEMOVE => {
            let down = wparam.0 & MK_LBUTTON != 0;
            let action = with_strip(|strip| {
                let (x, y) = strip.local(x, y);
                strip.moved(x, y, down)
            });
            carry_out(action.unwrap_or(Do::Nothing));
            return LRESULT(0);
        }
        WM_MOUSEWHEEL => {
            // The wheel's point is in screen coordinates.
            let delta = ((wparam.0 >> 16) & 0xFFFF) as i16;
            let mut at = POINT {
                x: x as i32,
                y: y as i32,
            };
            let _ = unsafe { windows::Win32::Graphics::Gdi::ScreenToClient(hwnd, &mut at) };
            let action = with_strip(|strip| {
                let (x, y) = strip.local(at.x as f32, at.y as f32);
                strip.wheel(x, y, i32::from(delta))
            });
            carry_out(action.unwrap_or(Do::Nothing));
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == ANIM_TIMER => {
            with_strip(Strip::animate);
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == CLOCK_TIMER => {
            with_strip(|strip| {
                if strip.shown {
                    strip.draw();
                }
            });
            return LRESULT(0);
        }
        WM_TIMER if wparam.0 == TOAST_TIMER => {
            with_strip(|strip| {
                let _ = unsafe { KillTimer(Some(strip.window.hwnd()), TOAST_TIMER) };
                if strip.shown {
                    strip.relayout();
                    strip.draw();
                }
            });
            return LRESULT(0);
        }
        _ => {}
    }
    // SAFETY: default processing of the unchanged message.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

fn thread_main() {
    let Some(instance) = unsafe { GetModuleHandleW(None) }.ok().map(HINSTANCE::from) else {
        return;
    };
    // Declared before the window so it is unregistered after the window is gone.
    let _class = match ClassGuard::register(CLASS, Some(strip_proc), instance) {
        Ok(class) => class,
        Err(error) => {
            host::log_error("RegisterClassW", &error, "clipboard_strip");
            return;
        }
    };
    let _preview_class = match ClassGuard::register(PREVIEW_CLASS, Some(preview_proc), instance) {
        Ok(class) => class,
        Err(error) => {
            host::log_error("RegisterClassW", &error, "clipboard_preview");
            return;
        }
    };
    // A tool window (no taskbar button), above the others, drawn with per-pixel alpha. It is
    // not NOACTIVATE: it takes the keys when it opens.
    let window = match OwnedWindow::overlay(
        CLASS,
        "RightKit Clipboard",
        (0, 0, 0, 0),
        rightkit_overlay::OverlayStyle::KEYBOARD,
        instance,
    ) {
        Ok(window) => window,
        Err(error) => {
            host::log_error("CreateWindowExW", &error, "clipboard_strip");
            return;
        }
    };
    // The preview: a picture the pointer goes through and the keys never reach.
    let preview_window = match OwnedWindow::overlay(
        PREVIEW_CLASS,
        "RightKit Clipboard Preview",
        (0, 0, 0, 0),
        rightkit_overlay::OverlayStyle::CLICK_THROUGH,
        instance,
    ) {
        Ok(window) => window,
        Err(error) => {
            host::log_error("CreateWindowExW", &error, "clipboard_preview");
            return;
        }
    };
    // Never in a screenshot or a screen share, and closed when the screen locks.
    // SAFETY: calls on this thread's own windows.
    unsafe {
        ffi::SetWindowDisplayAffinity(window.hwnd().0, capture_affinity());
        ffi::SetWindowDisplayAffinity(preview_window.hwnd().0, capture_affinity());
        ffi::WTSRegisterSessionNotification(window.hwnd().0, 0);
    }
    let Some(painter) = TextPainter::new() else {
        return;
    };
    clip_img::init_com();
    drag::init();
    STRIP.with(|cell| {
        *cell.borrow_mut() = Some(Strip::blank(window, preview_window, painter));
    });
    let mut message = MSG::default();
    // Creates this thread's message queue before the id is published.
    let _ = unsafe { PeekMessageW(&mut message, None, WM_USER, WM_USER, PM_NOREMOVE) };
    THREAD_ID.store(unsafe { GetCurrentThreadId() }, Ordering::Relaxed);
    with_strip(Strip::sync_hotkey);
    while unsafe { GetMessageW(&mut message, None, 0, 0) }.0 > 0 {
        if message.hwnd.0.is_null() {
            match message.message {
                WM_SYNC => {
                    with_strip(Strip::sync_hotkey);
                }
                WM_TOGGLE => toggle(),
                WM_OPEN_AT => {
                    let pick = PICK.lock().unwrap_or_else(PoisonError::into_inner).take();
                    with_strip(|strip| strip.open(pick));
                }
                _ => {}
            }
        } else {
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
    THREAD_ID.store(0, Ordering::Relaxed);
    STRIP.with(|cell| drop(cell.borrow_mut().take()));
}
