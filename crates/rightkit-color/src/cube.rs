//! `.cube` 3D LUT parsing, validation and bounded intensity mixing.
//!
//! User LUTs are untrusted input: size must be 2..=[`MAX_LUT_SIZE`], every value finite and
//! within `MAX_ABS_VALUE`, the entry count exact, 1D LUTs and unknown keywords rejected.
//! Intensity is applied by blending the table with the identity table, so the render graph
//! stays a single linear `lut3d` filter (no split/blend) and strength is bounded by construction.

use crate::clamp_or;

pub const MAX_LUT_SIZE: usize = 65;
/// Largest `.cube` file accepted (a 65^3 table is about 6 MB of text).
pub const MAX_CUBE_BYTES: usize = 16 * 1024 * 1024;
const MAX_ABS_VALUE: f32 = 8.0;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LutError {
    #[error("LUT file too large ({0} bytes)")]
    TooLarge(usize),
    #[error("line {line}: {msg}")]
    Malformed { line: usize, msg: String },
    #[error("missing LUT_3D_SIZE")]
    MissingSize,
    #[error("1D LUTs are not supported (LUT_1D_SIZE)")]
    OneDimensional,
    #[error("LUT_3D_SIZE {0} out of range 2..={MAX_LUT_SIZE}")]
    SizeOutOfRange(usize),
    #[error("expected {expected} table entries, found {found}")]
    WrongEntryCount { expected: usize, found: usize },
}

/// A validated 3D LUT. Entries are ordered red fastest, then green, then blue (`.cube` order).
#[derive(Debug, Clone, PartialEq)]
pub struct Cube3d {
    pub title: Option<String>,
    pub size: usize,
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    pub data: Vec<[f32; 3]>,
}

fn bad(line: usize, msg: impl Into<String>) -> LutError {
    LutError::Malformed {
        line,
        msg: msg.into(),
    }
}

fn floats3(rest: &str, line: usize) -> Result<[f32; 3], LutError> {
    let v: Vec<&str> = rest.split_whitespace().collect();
    if v.len() != 3 {
        return Err(bad(line, "expected three numbers"));
    }
    let mut out = [0f32; 3];
    for (o, s) in out.iter_mut().zip(v) {
        let f: f32 = s
            .parse()
            .map_err(|_| bad(line, format!("not a number: {s}")))?;
        if !f.is_finite() {
            return Err(bad(line, "non-finite value"));
        }
        *o = f;
    }
    Ok(out)
}

impl Cube3d {
    pub fn parse(text: &str) -> Result<Self, LutError> {
        if text.len() > MAX_CUBE_BYTES {
            return Err(LutError::TooLarge(text.len()));
        }
        let mut title = None;
        let mut size: Option<usize> = None;
        let mut dmin = [0f32; 3];
        let mut dmax = [1f32; 3];
        let mut data: Vec<[f32; 3]> = Vec::new();
        for (i, raw) in text.lines().enumerate() {
            let n = i + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let first = line.chars().next().unwrap_or(' ');
            if first.is_ascii_alphabetic() {
                let (kw, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
                match kw {
                    "TITLE" => title = Some(rest.trim().trim_matches('"').to_string()),
                    "LUT_3D_SIZE" => {
                        if size.is_some() {
                            return Err(bad(n, "duplicate LUT_3D_SIZE"));
                        }
                        let s: usize = rest
                            .trim()
                            .parse()
                            .map_err(|_| bad(n, "LUT_3D_SIZE is not an integer"))?;
                        if !(2..=MAX_LUT_SIZE).contains(&s) {
                            return Err(LutError::SizeOutOfRange(s));
                        }
                        size = Some(s);
                    }
                    "LUT_1D_SIZE" => return Err(LutError::OneDimensional),
                    "DOMAIN_MIN" => dmin = floats3(rest, n)?,
                    "DOMAIN_MAX" => dmax = floats3(rest, n)?,
                    other => return Err(bad(n, format!("unknown keyword {other}"))),
                }
                continue;
            }
            let s = size.ok_or(LutError::MissingSize)?;
            if data.len() >= s * s * s {
                return Err(LutError::WrongEntryCount {
                    expected: s * s * s,
                    found: data.len() + 1,
                });
            }
            let v = floats3(line, n)?;
            if v.iter().any(|x| x.abs() > MAX_ABS_VALUE) {
                return Err(bad(n, format!("value outside +-{MAX_ABS_VALUE}")));
            }
            data.push(v);
        }
        let size = size.ok_or(LutError::MissingSize)?;
        if data.len() != size * size * size {
            return Err(LutError::WrongEntryCount {
                expected: size * size * size,
                found: data.len(),
            });
        }
        if (0..3).any(|c| dmax[c] <= dmin[c]) {
            return Err(bad(0, "DOMAIN_MAX must exceed DOMAIN_MIN"));
        }
        Ok(Cube3d {
            title,
            size,
            domain_min: dmin,
            domain_max: dmax,
            data,
        })
    }

    pub fn to_text(&self) -> String {
        let mut s = String::with_capacity(self.data.len() * 27 + 128);
        if let Some(t) = &self.title {
            s.push_str(&format!("TITLE \"{}\"\n", t.replace('"', "")));
        }
        s.push_str(&format!("LUT_3D_SIZE {}\n", self.size));
        s.push_str(&format!(
            "DOMAIN_MIN {:.6} {:.6} {:.6}\nDOMAIN_MAX {:.6} {:.6} {:.6}\n",
            self.domain_min[0],
            self.domain_min[1],
            self.domain_min[2],
            self.domain_max[0],
            self.domain_max[1],
            self.domain_max[2]
        ));
        for v in &self.data {
            s.push_str(&format!("{:.6} {:.6} {:.6}\n", v[0], v[1], v[2]));
        }
        s
    }

    /// Blend towards the identity table: `0.0` = identity, `1.0` = this LUT. NaN -> 0, clamped.
    /// The identity output is the normalised input (`0..1`), as `lut3d` assumes `0..1` outputs.
    pub fn mixed(&self, intensity: f64) -> Cube3d {
        let t = clamp_or(intensity, 0.0, 1.0, 0.0) as f32;
        let n = self.size;
        let denom = (n - 1) as f32;
        let data = self
            .data
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let idx = [i % n, (i / n) % n, i / (n * n)];
                let mut o = [0f32; 3];
                for c in 0..3 {
                    let identity = idx[c] as f32 / denom;
                    o[c] = identity + (v[c] - identity) * t;
                }
                o
            })
            .collect();
        Cube3d {
            data,
            ..self.clone()
        }
    }

    /// Stable content hash (FNV-1a over the formatted table) for cache file names.
    pub fn fingerprint(&self) -> String {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in self.to_text().bytes() {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        format!("{h:016x}")
    }
}
