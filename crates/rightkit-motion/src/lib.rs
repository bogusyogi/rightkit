//! Product-neutral motion helpers for Right Suite video.
//!
//! - [`beats`]: parse a bracketed script (`"This is [important] stuff"`) and snap each
//!   `[span]` to word timings from `rightkit-transcript`, so animation cues land on spoken words.
//! - [`sheet`]: plan (and, with feature `assemble`, compose) storyboard inspect sheets of
//!   rendered frames, six tiles per sheet by default.
//!
//! No browser, Typst, media decode or I/O lives here. Frame decode and encode stay in
//! `rightkit-ffmpeg`; fuzzy ASR matching stays in `rightkit-align`.
//!
//! # Take format (not built)
//!
//! The decision record (`docs/design/motion-owner.md`, section 2) also sketches a `take` module:
//! an append-only, checksummed frame container with a shapes-once producer. It is deliberately
//! absent. It stays gated until a producer is named (likely `rightkit-compose` `FrameDescription`
//! diffs), and its capability `motion-takes` is declaration-only until then.

pub mod beats;
pub mod sheet;

pub use beats::{parse_script, snap, Beat, BeatError, BracketedScript, ScriptSpan, SnapConfig};
#[cfg(feature = "assemble")]
pub use sheet::{assemble, AssembledSheet, Frame};
pub use sheet::{
    plan_sheets, Selection, SheetError, SheetLayout, SheetPlan, SheetSpec, TileSlot,
    DEFAULT_COLUMNS, DEFAULT_MIN_TILE_WIDTH, DEFAULT_ROWS, DEFAULT_TILE_HEIGHT, DEFAULT_TILE_WIDTH,
    SHEETS_SCHEMA,
};
