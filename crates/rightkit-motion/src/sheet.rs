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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_six_tiles_2x3_at_960x540() {
        let plan = plan_sheets(SheetSpec::new(12_000, 30.0, Selection::Uniform(6))).unwrap();
        assert_eq!(plan.schema, SHEETS_SCHEMA);
        assert_eq!(plan.sheets.len(), 1);
        assert_eq!(plan.sheets[0].tiles.len(), 6);
        assert_eq!((plan.sheet_width, plan.sheet_height), (1920, 1620));
        let t = &plan.sheets[0].tiles;
        assert_eq!((t[1].x, t[1].y), (960, 0));
        assert_eq!((t[2].x, t[2].y), (0, 540));
        assert_eq!((t[5].x, t[5].y), (960, 1080));
    }

    #[test]
    fn uniform_times_are_slice_centres() {
        let plan = plan_sheets(SheetSpec::new(6_000, 30.0, Selection::Uniform(3))).unwrap();
        let times: Vec<u64> = plan.sheets[0].tiles.iter().map(|t| t.time_ms).collect();
        assert_eq!(times, vec![1_000, 3_000, 5_000]);
    }

    #[test]
    fn seven_tiles_spill_to_a_second_sheet() {
        let plan = plan_sheets(SheetSpec::new(7_000, 25.0, Selection::Uniform(7))).unwrap();
        assert_eq!(plan.sheets.len(), 2);
        assert_eq!(plan.sheets[1].tiles.len(), 1);
        assert_eq!(plan.sheets[1].tiles[0].sheet, 1);
        assert_eq!(plan.sheets[1].tiles[0].slot, 0);
    }

    #[test]
    fn frame_numbers_round_and_clamp() {
        let spec = SheetSpec::new(1_000, 30.0, Selection::Times(vec![0, 500, 1_000]));
        let plan = plan_sheets(spec).unwrap();
        let frames: Vec<u64> = plan.sheets[0].tiles.iter().map(|t| t.frame).collect();
        assert_eq!(frames, vec![0, 15, 29]);
    }

    #[test]
    fn beats_selection_uses_beat_starts() {
        let beats = vec![
            Beat {
                span: 0,
                start_ms: 400,
                end_ms: 900,
                word_start: 0,
                word_end: 1,
            },
            Beat {
                span: 1,
                start_ms: 2_000,
                end_ms: 2_500,
                word_start: 3,
                word_end: 4,
            },
        ];
        let plan = plan_sheets(SheetSpec::new(3_000, 30.0, Selection::Beats(beats))).unwrap();
        let times: Vec<u64> = plan.sheets[0].tiles.iter().map(|t| t.time_ms).collect();
        assert_eq!(times, vec![400, 2_000]);
    }

    #[test]
    fn narrow_tiles_are_refused_and_wider_grids_work() {
        let mut spec = SheetSpec::new(1_000, 30.0, Selection::Uniform(2));
        spec.tile_width = 330;
        assert_eq!(
            plan_sheets(spec.clone()),
            Err(SheetError::TileTooNarrow {
                width: 330,
                min: DEFAULT_MIN_TILE_WIDTH
            })
        );
        spec.tile_width = 480;
        spec.tile_height = 270;
        spec.columns = 3;
        spec.rows = 2;
        let plan = plan_sheets(spec).unwrap();
        assert_eq!((plan.sheet_width, plan.sheet_height), (1440, 540));
    }

    #[test]
    fn invalid_inputs_are_typed_errors() {
        let ok = SheetSpec::new(1_000, 30.0, Selection::Uniform(1));
        let mut s = ok.clone();
        s.fps = 0.0;
        assert_eq!(plan_sheets(s), Err(SheetError::BadFps));
        let mut s = ok.clone();
        s.columns = 0;
        assert_eq!(plan_sheets(s), Err(SheetError::EmptyGrid));
        let mut s = ok.clone();
        s.duration_ms = 0;
        assert_eq!(plan_sheets(s), Err(SheetError::ZeroDuration));
        let mut s = ok.clone();
        s.selection = Selection::Times(vec![]);
        assert_eq!(plan_sheets(s), Err(SheetError::EmptySelection));
        let mut s = ok;
        s.selection = Selection::Times(vec![2_000]);
        assert_eq!(
            plan_sheets(s),
            Err(SheetError::TimeOutOfRange {
                time_ms: 2_000,
                duration_ms: 1_000
            })
        );
    }

    #[test]
    fn manifest_round_trips() {
        let plan = plan_sheets(SheetSpec::new(4_000, 24.0, Selection::Uniform(4))).unwrap();
        let back: SheetPlan = serde_json::from_str(&plan.to_manifest_json()).unwrap();
        assert_eq!(back, plan);
        assert!(plan
            .to_manifest_json()
            .contains("rightkit.motion.sheets.v1"));
    }

    #[cfg(feature = "assemble")]
    #[test]
    fn assembles_png_sheet_with_tile_colours() {
        let plan = plan_sheets(SheetSpec::new(2_000, 30.0, Selection::Uniform(2))).unwrap();
        let sheets = assemble(&plan, |t| Frame {
            width: t.width,
            height: t.height,
            rgba: [if t.slot == 0 { 255 } else { 0 }, 0, 0, 255]
                .repeat((t.width * t.height) as usize),
        })
        .unwrap();
        assert_eq!(sheets.len(), 1);
        let img = image::load_from_memory(&sheets[0].png).unwrap().to_rgba8();
        assert_eq!(img.dimensions(), (plan.sheet_width, plan.sheet_height));
        assert_eq!(img.get_pixel(10, 10).0, [255, 0, 0, 255]);
        assert_eq!(img.get_pixel(970, 10).0, [0, 0, 0, 255]);
        assert_eq!(img.get_pixel(10, 1000).0, [0, 0, 0, 255]);
    }

    #[cfg(feature = "assemble")]
    #[test]
    fn assemble_refuses_wrong_frame_size() {
        let plan = plan_sheets(SheetSpec::new(1_000, 30.0, Selection::Uniform(1))).unwrap();
        let err = assemble(&plan, |_| Frame {
            width: 10,
            height: 10,
            rgba: vec![0; 400],
        })
        .unwrap_err();
        assert!(matches!(err, SheetError::FrameSizeMismatch { .. }));
    }
}
