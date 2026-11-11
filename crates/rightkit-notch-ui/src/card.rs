//! Hover card content: the key numbers behind each ring (same information as the Mac
//! hover cards). Pure data; `render.rs` draws it. Unknown values read `--`.

use crate::layout::Edge;
use rightkit_win_ui::glyphs::{Glyph, Symbol, Tile};

/// The icon beside a card's title (the Mac's provider glyph); drawn by `render.rs`. Which
/// glyph a card carries is the host's choice.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mark {
    /// No icon.
    #[default]
    None,
    /// This glyph.
    Glyph(Glyph),
}

/// The speech-bubble tail: the notch's `edge` (the tail leaves the card on the side facing
/// it) and how far, in device pixels along the card's own axis, it sits from the card's
/// middle so its point stays on the hovered ring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Tail {
    pub edge: Edge,
    pub offset: i32,
}

/// Colour of a tinted line or a status dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// Amber: a caveat.
    Warning,
    /// Green: all well.
    Good,
    /// Red: stopped.
    Critical,
}

/// The large icon a card leads with, beside its title block.
#[derive(Clone, Debug, PartialEq)]
pub enum Lead {
    /// A colour icon the renderer draws itself.
    Tile(Tile),
    /// The shell's icon for this file when it has one (live cards), else the package tile.
    File(String),
    /// A symbol on a dim disc (a transfer card); `problem` draws it in the warning colour.
    Disc { symbol: Symbol, problem: bool },
    /// A ring's own glyph (the provider's mark, the drive), in red when `problem`.
    Glyph { glyph: Glyph, problem: bool },
}

/// A round button in the header's top right.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Head {
    /// Look again. No card builds it now (the Send picker has a "Look again" pill); kept
    /// until `render.rs` drops its arm for it.
    #[allow(dead_code)]
    Refresh,
    /// Sync (SF Symbols' `arrow.triangle.2.circlepath`, as on the Mac).
    Sync,
    /// A scan is running: a spinner where Refresh would be.
    Scanning,
    /// The round close.
    Close,
    /// A bare small cross with no disc (the alert card's dismiss).
    Dismiss,
}

/// One pill of a button row.
#[derive(Clone, Debug, PartialEq)]
pub struct Button {
    pub label: String,
    pub symbol: Option<Symbol>,
    /// A card glyph (a mark, not a symbol) in the symbol's place: the drive alert's "Open
    /// Storage" leads with the drive. `symbol` wins when both are set.
    pub glyph: Option<Glyph>,
    /// A spinner where the symbol would be, and no press: the Mac's "Look again" while a scan
    /// runs. `render.rs` draws it (animated) in place of `symbol`.
    pub busy: bool,
}

impl Button {
    pub fn new(label: impl Into<String>, symbol: Symbol) -> Self {
        Self {
            label: label.into(),
            symbol: Some(symbol),
            glyph: None,
            busy: false,
        }
    }
}

/// What the pointer is over on a card: a header button, or a row's button (index 0 for a
/// row that is one button itself, such as a device).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    Head(usize),
    Row(usize, usize),
}

/// Which of a card's buttons are live (have an action): per header button, and per row per
/// button. A dead one reads dimmed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Live {
    pub head: Vec<bool>,
    pub rows: Vec<Vec<bool>>,
}

impl Live {
    pub fn row(&self, row: usize, button: usize) -> bool {
        self.rows
            .get(row)
            .and_then(|buttons| buttons.get(button))
            .copied()
            .unwrap_or(false)
    }
}

/// One pill of a `Row::Pills`.
#[derive(Clone, Debug, PartialEq)]
pub struct Pill {
    pub symbol: Symbol,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    /// Label on the left, quieter value on the right (a device on the Send card is one such
    /// row with an action).
    Pair { label: String, value: String },
    /// A full-width button plate, the Mac's "Copy last" and "Paste clipboard" rows: an optional
    /// symbol, the label, and a quieter detail at the right. Without a detail the symbol and
    /// label sit in the plate's middle. Whatever does not fit is cut with an ellipsis.
    Button {
        symbol: Option<Symbol>,
        label: String,
        detail: String,
    },
    /// A row of the Tools card's pill grid: one or two equal pills, each a symbol and a label
    /// centred in its plate (the Mac's `sendBarButton` in a two-column grid).
    Pills(Vec<Pill>),
    /// One metered window: label and trailing text on a line, a bar (none without a share),
    /// then the summary line.
    Meter {
        label: String,
        trailing: String,
        fraction: Option<f32>,
        summary: String,
        /// A usage window: its bar follows the colour transition setting.
        ramp: bool,
    },
    /// A model's or feature's own limits (the Mac's grouped box): its title in bold over a
    /// rounded inset box holding `Meter` rows.
    Group { title: String, rows: Vec<Row> },
    /// Secondary-ink paragraph.
    Note(String),
    /// Primary-ink paragraph.
    Text(String),
    /// Critical-ink line that says the subject is stopped (a spent limit).
    Alert(String),
    /// A line in a tone (the amber caveat under a disk image's detail).
    Tinted { text: String, tone: Tone },
    /// Side-by-side pills, then an optional round close.
    Buttons { buttons: Vec<Button>, close: bool },
    /// A bare progress bar; `None` is indeterminate.
    Progress(Option<f32>),
    /// A nearby device: its symbol, its name in bold, its model beneath and a send arrow.
    Device {
        symbol: Symbol,
        alias: String,
        model: String,
    },
    /// A file or picture on a card: its thumbnail (`the host thumbnail`, else the shell icon
    /// `fileicon::of`) and its name. A received file on the Send card's shelf (press, move to drag
    /// it out; a double-click opens it; on the shelf `name` carries the whole detail line, `"name · size · from <device> · <age>"`) and an image in the copy burst's hover preview.
    File { name: String, path: String },
    /// A spinner with a line beside it.
    Waiting(String),
    /// The Send hover card's tiles (#471): three to a row. Each slot is one tile: a received
    /// image as its thumbnail, text as a short preview, any other file as its icon, or a
    /// "Paste and send to" box for a computer.
    Tiles(Vec<Item>),
}

/// One received item in a `Row::Tiles`.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    /// A file on disk.
    File(String),
    /// A received message.
    Text(String),
    /// A "Paste and send to" box: a computer's symbol, name and "Default · Nearby" style line;
    /// dimmed when it cannot be pressed.
    Device {
        symbol: Symbol,
        name: String,
        detail: String,
        enabled: bool,
        /// The default computer: drawn with the 1 px light outline.
        default: bool,
        /// Not reachable: drawn at 40% like the Mac's.
        offline: bool,
    },
}

impl Row {
    /// How many things on the row can be pressed: its buttons, or the row itself.
    pub fn slots(&self) -> usize {
        match self {
            Row::Buttons { buttons, close } => buttons.len() + usize::from(*close),
            Row::Pills(pills) => pills.len(),
            Row::Tiles(items) => items.len(),
            _ => 1,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct CardContent {
    pub title: String,
    /// The large icon beside the title block: makes the card a banner (icon, then title,
    /// detail and the buttons at the bottom), like the Mac's disk image and update cards.
    pub lead: Option<Lead>,
    /// Round buttons at the header's top right, left to right.
    pub head: Vec<Head>,
    /// The wide card of the disk image and update cards.
    pub wide: bool,
    /// The detail line reads in the warning colour.
    pub problem: bool,
    /// The Mac's fixed card height, in DIPs, when it has one.
    pub height: Option<f32>,
    /// Quiet text on the title's line, at the right.
    pub accessory: Option<String>,
    /// Quiet line under the title (such as an account's plan).
    pub subtitle: Option<String>,
    pub mark: Mark,
    /// The header's round buttons sit centred on the title row without making the header
    /// taller (the Mac's provider cards, whose sync button shares the row with the title).
    pub inline_head: bool,
    /// The Send hover card: laid out in the Send metrics with whole-pixel heights.
    pub send: bool,
    pub tail: Option<Tail>,
    /// The update card's heavier progress bar (the Mac draws it 16 design px tall).
    pub heavy_bar: bool,
    pub rows: Vec<Row>,
    /// What is left of a toast that goes by itself, 1 to 0: drawn as the life ring round its
    /// × (liquid-motion pass §4). `None` for a card that stays until answered.
    pub life: Option<f32>,
    /// The Mac's `.working` mark: draw the small animated spinner as the toast's inline glyph in
    /// place of `lead`'s icon (`lead` stays `Some`, which keeps the toast layout).
    pub spinner: bool,
}

impl CardContent {
    /// A card with a title and rows and nothing else.
    pub fn plain(title: impl Into<String>, rows: Vec<Row>) -> Self {
        Self {
            title: title.into(),
            rows,
            ..Self::default()
        }
    }
}
