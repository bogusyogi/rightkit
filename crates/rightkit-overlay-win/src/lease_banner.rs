//! The stage logic of a "the agent has the screen" banner: which of the two stages a lease is in,
//! the text of the pill for it, the button it offers, and where the pill and the click-through
//! border strips go. It draws nothing and holds no window, so it works on every host; a shell
//! creates its own windows from these numbers and supplies its own copy and colours.
//!
//! * `Warning`: a pill at the top of the primary display, with a Cancel button.
//! * `Held`: the same pill with an End button, and a border framing the whole virtual desktop.

/// Border thickness in pixels.
pub const BORDER: i32 = 4;
/// Pill width in pixels.
pub const PILL_W: i32 = 460;
/// Pill height in pixels.
pub const PILL_H: i32 = 64;
/// Distance of the pill from the top of the primary display, in pixels.
pub const PILL_TOP: i32 = 40;

/// One stage of a foreground lease, as the engine reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseStage {
    /// The takeover is announced; `seconds` are left before it.
    Warning { app: String, seconds: u64 },
    /// The agent has the screen; `seconds` are left.
    Held { app: String, seconds: u64 },
}

impl LeaseStage {
    /// The stage for one reading of the engine's lease state. `None` when no lease is active
    /// (`active` is not `Some(true)`) or the state is not a known stage.
    pub fn from_parts(
        active: Option<bool>,
        state: Option<&str>,
        app: &str,
        seconds: u64,
    ) -> Option<LeaseStage> {
        if active != Some(true) {
            return None;
        }
        let app = app.to_string();
        match state {
            Some("warning") => Some(LeaseStage::Warning { app, seconds }),
            Some("held") => Some(LeaseStage::Held { app, seconds }),
            _ => None,
        }
    }

    /// Whether the agent holds the screen (the border shows).
    pub fn is_held(&self) -> bool {
        matches!(self, LeaseStage::Held { .. })
    }

    fn app_and_seconds(&self) -> (&str, u64) {
        match self {
            LeaseStage::Warning { app, seconds } | LeaseStage::Held { app, seconds } => {
                (app.as_str(), *seconds)
            }
        }
    }
}

/// The shell's own words: templates with two `{}` slots (the app, then the seconds), the name
/// shown when the engine gives none, and the two button labels.
#[derive(Clone, Copy, Debug)]
pub struct BannerCopy<'a> {
    pub warning_template: &'a str,
    pub held_template: &'a str,
    pub unnamed_app: &'a str,
    pub cancel_label: &'a str,
    pub end_label: &'a str,
}

/// The text of the pill for `stage`. `format` fills a template's `{}` slots in order (the
/// shell's translating formatter).
pub fn banner_line(
    stage: &LeaseStage,
    copy: &BannerCopy<'_>,
    format: impl Fn(&str, &[&str]) -> String,
) -> String {
    let (app, seconds) = stage.app_and_seconds();
    let app = if app.is_empty() {
        copy.unnamed_app
    } else {
        app
    };
    let template = if stage.is_held() {
        copy.held_template
    } else {
        copy.warning_template
    };
    format(template, &[app, &seconds.to_string()])
}

/// The label of the pill's one button: Cancel while warning, End while held.
pub fn button_label<'a>(stage: &LeaseStage, copy: &BannerCopy<'a>) -> &'a str {
    if stage.is_held() {
        copy.end_label
    } else {
        copy.cancel_label
    }
}

/// A window frame in physical pixels: left, top, width, height.
pub type Frame = (i32, i32, i32, i32);

/// The pill: top centre of a primary display `primary_width` pixels wide.
pub fn pill_frame(primary_width: i32) -> Frame {
    ((primary_width - PILL_W) / 2, PILL_TOP, PILL_W, PILL_H)
}

/// The four border strips (top, bottom, left, right) around the virtual desktop given by its
/// left, top, width and height. `None` when the desktop has no area.
pub fn border_strips(left: i32, top: i32, width: i32, height: i32) -> Option<[Frame; 4]> {
    if width < 1 || height < 1 {
        return None;
    }
    Some([
        (left, top, width, BORDER),
        (left, top + height - BORDER, width, BORDER),
        (left, top, BORDER, height),
        (left + width - BORDER, top, BORDER, height),
    ])
}
