//! Inspect sheets: storyboard contact sheets of rendered frames.
//!
//! [`plan_sheets`] is pure layout. It picks the frame times (uniform, explicit or from beats),
//! converts them to frame numbers and assigns every frame a tile on a sheet. The plan
//! serialises as the `rightkit.motion.sheets.v1` manifest: times, frame numbers and tile
//! positions. With feature `assemble`, `assemble` composes PNG sheets from frames the caller
//! decodes (through `rightkit-ffmpeg`); this crate never decodes media.
//!
//! # Defaults (to confirm)
//!
//! The defaults below come from the decision record and are marked **to confirm**: six tiles
//! per sheet in a 2 column by 3 row grid, each tile 960x540, and a minimum tile width of 480
//! px (the source reports that 330 px tiles miss defects). All are configurable on
//! [`SheetSpec`], except that a tile narrower than the minimum is always refused.

use crate::beats::Beat;
use serde::{Deserialize, Serialize};

/// Manifest schema tag.
pub const SHEETS_SCHEMA: &str = "rightkit.motion.sheets.v1";
/// Default tile width in px. To confirm.
pub const DEFAULT_TILE_WIDTH: u32 = 960;
/// Default tile height in px. To confirm.
pub const DEFAULT_TILE_HEIGHT: u32 = 540;
/// Default grid columns (2x3 grid, six tiles per sheet). To confirm.
pub const DEFAULT_COLUMNS: u32 = 2;
/// Default grid rows. To confirm.
pub const DEFAULT_ROWS: u32 = 3;
/// Smallest tile width accepted, in px. To confirm.
pub const DEFAULT_MIN_TILE_WIDTH: u32 = 480;

/// Which moments to show.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Selection {
    /// `n` frames at the centres of `n` equal time slices.
    Uniform(u32),
    /// Explicit times in ms.
    Times(Vec<u64>),
    /// The start of each beat.
    Beats(Vec<Beat>),
}

/// Inputs to [`plan_sheets`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetSpec {
    pub duration_ms: u64,
    pub fps: f64,
    pub selection: Selection,
    /// Tile width in px (default [`DEFAULT_TILE_WIDTH`], to confirm).
    pub tile_width: u32,
    /// Tile height in px (default [`DEFAULT_TILE_HEIGHT`], to confirm).
    pub tile_height: u32,
    /// Grid columns (default [`DEFAULT_COLUMNS`], to confirm).
    pub columns: u32,
    /// Grid rows (default [`DEFAULT_ROWS`], to confirm).
    pub rows: u32,
}

impl SheetSpec {
    /// Defaults (to confirm): 2x3 grid of 960x540 tiles.
    pub fn new(duration_ms: u64, fps: f64, selection: Selection) -> Self {
        Self {
            duration_ms,
            fps,
            selection,
            tile_width: DEFAULT_TILE_WIDTH,
            tile_height: DEFAULT_TILE_HEIGHT,
            columns: DEFAULT_COLUMNS,
            rows: DEFAULT_ROWS,
        }
    }
}

/// Planning errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SheetError {
    #[error("tile width {width} is below the {min} px minimum")]
    TileTooNarrow { width: u32, min: u32 },
    #[error("tile height must be non-zero")]
    ZeroTileHeight,
    #[error("grid must have at least one column and one row")]
    EmptyGrid,
    #[error("fps must be finite and positive")]
    BadFps,
    #[error("duration must be non-zero")]
    ZeroDuration,
    #[error("selection is empty")]
    EmptySelection,
    #[error("time {time_ms} ms is past the duration {duration_ms} ms")]
    TimeOutOfRange { time_ms: u64, duration_ms: u64 },
    #[error("frame is {got_w}x{got_h}, tile is {want_w}x{want_h}")]
    FrameSizeMismatch {
        got_w: u32,
        got_h: u32,
        want_w: u32,
        want_h: u32,
    },
    #[error("frame buffer length {got} does not match {want} bytes")]
    FrameBufferLength { got: usize, want: usize },
    #[error("png encode: {0}")]
    Encode(String),
}

/// One frame placed on one sheet.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TileSlot {
    pub sheet: usize,
    /// Position on the sheet, row-major.
    pub slot: usize,
    pub time_ms: u64,
    pub frame: u64,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// One sheet's tiles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetLayout {
    pub index: usize,
    pub tiles: Vec<TileSlot>,
}

/// The planned sheets; serialises as the `rightkit.motion.sheets.v1` manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SheetPlan {
    pub schema: String,
    pub fps: f64,
    pub duration_ms: u64,
    pub tile_width: u32,
    pub tile_height: u32,
    pub columns: u32,
    pub rows: u32,
    pub sheet_width: u32,
    pub sheet_height: u32,
    pub sheets: Vec<SheetLayout>,
}

impl SheetPlan {
    /// The manifest as pretty JSON.
    pub fn to_manifest_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("sheet plan serialises")
    }
}

/// Plan the sheets. Tiles fill each sheet row-major; the last sheet may be partial.
pub fn plan_sheets(spec: SheetSpec) -> Result<SheetPlan, SheetError> {
    if spec.tile_width < DEFAULT_MIN_TILE_WIDTH {
        return Err(SheetError::TileTooNarrow {
            width: spec.tile_width,
            min: DEFAULT_MIN_TILE_WIDTH,
        });
    }
    if spec.tile_height == 0 {
        return Err(SheetError::ZeroTileHeight);
    }
    if spec.columns == 0 || spec.rows == 0 {
        return Err(SheetError::EmptyGrid);
    }
    if !spec.fps.is_finite() || spec.fps <= 0.0 {
        return Err(SheetError::BadFps);
    }
    if spec.duration_ms == 0 {
        return Err(SheetError::ZeroDuration);
    }

    let times: Vec<u64> = match &spec.selection {
        Selection::Uniform(n) => (0..*n as u64)
            .map(|i| ((2 * i + 1) * spec.duration_ms) / (2 * *n as u64))
            .collect(),
        Selection::Times(t) => t.clone(),
        Selection::Beats(b) => b.iter().map(|b| b.start_ms).collect(),
    };
    if times.is_empty() {
        return Err(SheetError::EmptySelection);
    }
    for &t in &times {
        if t > spec.duration_ms {
            return Err(SheetError::TimeOutOfRange {
                time_ms: t,
                duration_ms: spec.duration_ms,
            });
        }
    }

    let total_frames = ((spec.duration_ms as f64 * spec.fps / 1000.0).floor() as u64).max(1);
    let per_sheet = (spec.columns * spec.rows) as usize;
    let mut sheets: Vec<SheetLayout> = Vec::new();
    for (n, &time_ms) in times.iter().enumerate() {
        let sheet = n / per_sheet;
        let slot = n % per_sheet;
        if slot == 0 {
            sheets.push(SheetLayout {
                index: sheet,
                tiles: Vec::new(),
            });
        }
        let frame = ((time_ms as f64 * spec.fps / 1000.0).round() as u64).min(total_frames - 1);
        let col = slot as u32 % spec.columns;
        let row = slot as u32 / spec.columns;
        sheets[sheet].tiles.push(TileSlot {
            sheet,
            slot,
            time_ms,
            frame,
            x: col * spec.tile_width,
            y: row * spec.tile_height,
            width: spec.tile_width,
            height: spec.tile_height,
        });
    }

    Ok(SheetPlan {
        schema: SHEETS_SCHEMA.to_string(),
        fps: spec.fps,
        duration_ms: spec.duration_ms,
        tile_width: spec.tile_width,
        tile_height: spec.tile_height,
        columns: spec.columns,
        rows: spec.rows,
        sheet_width: spec.columns * spec.tile_width,
        sheet_height: spec.rows * spec.tile_height,
        sheets,
    })
}

/// A decoded RGBA8 frame supplied by the caller, exactly tile-sized.
#[cfg(feature = "assemble")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, RGBA, straight alpha.
    pub rgba: Vec<u8>,
}

/// One composed sheet.
#[cfg(feature = "assemble")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssembledSheet {
    pub index: usize,
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

/// Compose PNG sheets. `frame_for` is called once per tile, in plan order, and must return a
/// frame exactly the tile's size (no resampling is done). Unused slots stay opaque black.
#[cfg(feature = "assemble")]
pub fn assemble<F>(plan: &SheetPlan, mut frame_for: F) -> Result<Vec<AssembledSheet>, SheetError>
where
    F: FnMut(&TileSlot) -> Frame,
{
    use image::{ImageFormat, Rgba, RgbaImage};
    use std::io::Cursor;

    let mut out = Vec::with_capacity(plan.sheets.len());
    for layout in &plan.sheets {
        let mut canvas =
            RgbaImage::from_pixel(plan.sheet_width, plan.sheet_height, Rgba([0, 0, 0, 255]));
        for tile in &layout.tiles {
            let frame = frame_for(tile);
            if frame.width != tile.width || frame.height != tile.height {
                return Err(SheetError::FrameSizeMismatch {
                    got_w: frame.width,
                    got_h: frame.height,
                    want_w: tile.width,
                    want_h: tile.height,
                });
            }
            let want = tile.width as usize * tile.height as usize * 4;
            if frame.rgba.len() != want {
                return Err(SheetError::FrameBufferLength {
                    got: frame.rgba.len(),
                    want,
                });
            }
            let img = RgbaImage::from_raw(frame.width, frame.height, frame.rgba)
                .ok_or(SheetError::FrameBufferLength { got: 0, want })?;
            image::imageops::replace(&mut canvas, &img, tile.x as i64, tile.y as i64);
        }
        let mut png = Vec::new();
        canvas
            .write_to(&mut Cursor::new(&mut png), ImageFormat::Png)
            .map_err(|e| SheetError::Encode(e.to_string()))?;
        out.push(AssembledSheet {
            index: layout.index,
            width: plan.sheet_width,
            height: plan.sheet_height,
            png,
        });
    }
    Ok(out)
}
