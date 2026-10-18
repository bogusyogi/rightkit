//! Product-neutral motion helpers for Right Suite video.
//!
//! - [`beats`]: parse a bracketed script (`"This is [important] stuff"`) and snap each
//!   `[span]` to word timings from `rightkit-transcript`, so animation cues land on spoken words.
//! - [`sheet`]: plan (and, with feature `assemble`, compose) storyboard inspect sheets of
//!   rendered frames, six tiles per sheet by default.
//!
//! - [`render`], [`encoder`], [`frame`] (feature `encode`): a per-frame seek-and-capture driver over
//!   an app-implemented [`render::FrameSource`], H.264/MPEG-4 encoder pick with the bits-per-pixel
//!   quality map, ProRes 4444 and VP9 alpha output with an encoder-availability probe, PNG to
//!   PPM/PAM conversion, and still and poster export.
//!
//! No browser, Typst or media decode lives here, and no webview: the frame source is a trait. Without
//! feature `encode` there is no I/O at all. With it, ffmpeg runs only through `rightkit-ffmpeg`
//! (an owned `rightkit-child-process` tree); fuzzy ASR matching stays in `rightkit-align`.
//!
//! # Take format (not built)
//!
//! The decision record (`docs/design/motion-owner.md`, section 2) also sketches a `take` module:
//! an append-only, checksummed frame container with a shapes-once producer. It is deliberately
//! absent. It stays gated until a producer is named (likely `rightkit-compose` `FrameDescription`
//! diffs), and its capability `motion-takes` is declaration-only until then.

pub mod beats;
#[cfg(feature = "encode")]
pub mod encoder;
#[cfg(feature = "encode")]
pub mod frame;
#[cfg(feature = "encode")]
pub mod render;
pub mod sheet;

pub use beats::{parse_script, snap, Beat, BeatError, BracketedScript, ScriptSpan, SnapConfig};
#[cfg(feature = "assemble")]
pub use sheet::{assemble, AssembledSheet, Frame};
pub use sheet::{
    plan_sheets, Selection, SheetError, SheetLayout, SheetPlan, SheetSpec, TileSlot,
    DEFAULT_COLUMNS, DEFAULT_MIN_TILE_WIDTH, DEFAULT_ROWS, DEFAULT_TILE_HEIGHT, DEFAULT_TILE_WIDTH,
    SHEETS_SCHEMA,
};

#[cfg(feature = "encode")]
pub use encoder::{
    build_command, is_init_failure, pick_alpha_encoder, pick_encoder, probe_inventory,
    target_bitrate, AlphaSupport, AudioInput, EncodeSpec, Encoder, EncoderKind, Quality,
};
#[cfg(feature = "encode")]
pub use frame::{png_dimensions, png_to_pam, png_to_ppm};
#[cfg(feature = "encode")]
pub use render::{
    capture_still, export_poster, export_still, poster_frame, render, render_with_fallback,
    FrameError, FrameSource, RenderError, RenderJob, RenderOutcome, RenderPhase, RenderProgress,
    FPS_RANGE, MAX_FRAMES, SIZE_RANGE,
};
