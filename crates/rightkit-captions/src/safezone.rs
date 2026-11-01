//! Per-platform safe zones, shipped as JSON (`data/safe_zones.json`).
//!
//! A zone is a margin box (percent of each edge) plus permille exclusion
//! rectangles where the platform draws its own UI (action rails, bottom
//! description stack). Captions are placed inside the margin box and outside
//! every exclusion.

use serde::{Deserialize, Serialize};

const DATA: &str = include_str!("../data/safe_zones.json");

/// Pixel rectangle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Rect {
    pub fn right(&self) -> i64 {
        self.x + self.w
    }
    pub fn bottom(&self) -> i64 {
        self.y + self.h
    }
    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }
}

/// A UI exclusion region, permille of the frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exclusion {
    pub name: String,
    pub x: i64,
    pub y: i64,
    pub w: i64,
    pub h: i64,
}

impl Exclusion {
    pub fn rect(&self, w: i64, h: i64) -> Rect {
        Rect {
            x: self.x * w / 1000,
            y: self.y * h / 1000,
            w: self.w * w / 1000,
            h: self.h * h / 1000,
        }
    }
}

/// One platform's safe zone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SafeZone {
    pub id: String,
    pub name: String,
    /// Native delivery size of the platform.
    pub width: u32,
    pub height: u32,
    pub top_pct: f64,
    pub bottom_pct: f64,
    pub left_pct: f64,
    pub right_pct: f64,
    pub exclusions: Vec<Exclusion>,
}

impl SafeZone {
    /// The margin box captions must stay inside, for a `w`x`h` frame.
    pub fn content_box(&self, w: i64, h: i64) -> Rect {
        let l = (w as f64 * self.left_pct / 100.0).round() as i64;
        let r = (w as f64 * self.right_pct / 100.0).round() as i64;
        let t = (h as f64 * self.top_pct / 100.0).round() as i64;
        let b = (h as f64 * self.bottom_pct / 100.0).round() as i64;
        Rect {
            x: l,
            y: t,
            w: (w - l - r).max(0),
            h: (h - t - b).max(0),
        }
    }

    /// Why `rect` is not safe (empty when it is): outside the margin box or
    /// overlapping a named exclusion.
    pub fn violations(&self, rect: &Rect, w: i64, h: i64) -> Vec<String> {
        let c = self.content_box(w, h);
        let mut v = Vec::new();
        if rect.x < c.x || rect.y < c.y || rect.right() > c.right() || rect.bottom() > c.bottom() {
            v.push("outside the margin box".to_string());
        }
        for e in &self.exclusions {
            if rect.intersects(&e.rect(w, h)) {
                v.push(format!("overlaps {}", e.name));
            }
        }
        v
    }
}

/// The shipped safe-zone catalogue.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct SafeZoneCatalog {
    pub schema: String,
    pub platforms: Vec<SafeZone>,
}

impl SafeZoneCatalog {
    pub fn builtin() -> Self {
        serde_json::from_str(DATA).expect("data/safe_zones.json is valid")
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn get(&self, id: &str) -> Option<&SafeZone> {
        self.platforms.iter().find(|p| p.id == id)
    }
}
