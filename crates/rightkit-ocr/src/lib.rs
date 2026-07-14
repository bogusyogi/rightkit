//! Product-neutral two-stage ONNX OCR pipeline.
//!
//! Two-model pipeline:
//!   1. Det model (`det.onnx`) — DB detector; finds text
//!      bounding boxes in the image.
//!   2. Rec model (`rec.onnx`) — multilingual recognizer
//!      (50 languages + symbols + emoji, 18,708-char dict); reads text from each
//!      cropped box. The vocab length is read from the model output at runtime,
//!      so the dict is model-driven, not hardcoded.
//!
//! The caller configures the dynamic runtime through `rightkit-ort`, supplies
//! an absolute model directory, and supplies resolved artifact provenance.

mod support;

use image::{imageops, RgbImage};
use ort::{session::Session, value::Tensor};
use rightkit_ort::ProviderDiagnostic;
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};
use support::{
    build_session, crop_box, ctc_greedy_decode, dedup_boxes, det_chw_data, det_net_size,
    extract_boxes, group_into_lines, load_image, load_vocab, rec_chw_data, BBox, DET_BAND_OVERLAP,
    DET_MAX_W, DET_TILE_H,
};

// ---------------------------------------------------------------------------
// Public artifact and error contract
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrProvenance {
    Resolved {
        label: String,
        source: String,
        license: String,
    },
    Conflicting {
        labels: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OcrArtifactManifest {
    pub provenance: OcrProvenance,
    pub detector_sha256: String,
    pub recognizer_sha256: String,
    pub dictionary_sha256: String,
    pub fixture_sha256: String,
    pub expected_fixture_output: String,
}

impl OcrArtifactManifest {
    pub fn characterized_windows_contract() -> Self {
        Self {
            provenance: OcrProvenance::Conflicting {
                labels: vec!["PP-OCRv5".to_string(), "PP-OCRv6".to_string()],
            },
            detector_sha256: "eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1"
                .to_string(),
            recognizer_sha256: "5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634"
                .to_string(),
            dictionary_sha256: "b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d"
                .to_string(),
            fixture_sha256: "803e102100c91a077607cd2d4750d9d748d86ced48fabc0f77062f26dcba0a4b"
                .to_string(),
            expected_fixture_output: "RIGHTKIT OCR 2026\nShared parity fixture".to_string(),
        }
    }

    /// Replace the characterized conflict only with an explicit authoritative
    /// label and provenance source supplied by the signed artifact catalog.
    pub fn with_resolved_provenance(
        mut self,
        label: impl Into<String>,
        source: impl Into<String>,
        license: impl Into<String>,
    ) -> Result<Self, OcrError> {
        self.provenance = OcrProvenance::Resolved {
            label: label.into(),
            source: source.into(),
            license: license.into(),
        };
        self.validate_metadata()?;
        Ok(self)
    }

    pub fn validate_metadata(&self) -> Result<(), OcrError> {
        match &self.provenance {
            OcrProvenance::Conflicting { labels } => Err(OcrError::UnresolvedProvenance {
                labels: labels.clone(),
            }),
            OcrProvenance::Resolved {
                label,
                source,
                license,
            } if label.trim().is_empty()
                || source.trim().is_empty()
                || license.trim().is_empty() =>
            {
                Err(OcrError::InvalidProvenance)
            }
            OcrProvenance::Resolved { .. } => Ok(()),
        }
    }

    pub fn validate_model_dir(&self, model_dir: &Path) -> Result<VerifiedOcrArtifacts, OcrError> {
        self.validate_metadata()?;
        if !model_dir.is_absolute() {
            return Err(OcrError::UnsafeArtifactPath {
                path: model_dir.to_path_buf(),
            });
        }
        let root = model_dir
            .canonicalize()
            .map_err(|_| OcrError::MissingArtifact {
                path: model_dir.to_path_buf(),
            })?;
        if !root.is_dir() {
            return Err(OcrError::MissingArtifact { path: root });
        }
        Ok(VerifiedOcrArtifacts {
            detector: verify_artifact(&root, "det.onnx", &self.detector_sha256)?,
            recognizer: verify_artifact(&root, "rec.onnx", &self.recognizer_sha256)?,
            dictionary: verify_artifact(&root, "en_dict.txt", &self.dictionary_sha256)?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedOcrArtifacts {
    pub detector: PathBuf,
    pub recognizer: PathBuf,
    pub dictionary: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OcrError {
    UnresolvedProvenance {
        labels: Vec<String>,
    },
    InvalidProvenance,
    UnsafeArtifactPath {
        path: PathBuf,
    },
    MissingArtifact {
        path: PathBuf,
    },
    DigestMismatch {
        path: PathBuf,
        expected: String,
        actual: String,
    },
    Runtime(String),
}

impl fmt::Display for OcrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnresolvedProvenance { labels } => write!(
                f,
                "OCR provenance conflict is unresolved: {}",
                labels.join(" vs ")
            ),
            Self::InvalidProvenance => {
                write!(
                    f,
                    "OCR provenance label, source, and license must be non-empty"
                )
            }
            Self::UnsafeArtifactPath { path } => {
                write!(f, "OCR artifact path must be absolute: {}", path.display())
            }
            Self::MissingArtifact { path } => {
                write!(f, "OCR artifact is missing: {}", path.display())
            }
            Self::DigestMismatch {
                path,
                expected,
                actual,
            } => write!(
                f,
                "OCR artifact digest mismatch for {}: expected {expected}, got {actual}",
                path.display()
            ),
            Self::Runtime(message) => write!(f, "OCR runtime error: {message}"),
        }
    }
}

impl std::error::Error for OcrError {}
impl From<String> for OcrError {
    fn from(value: String) -> Self {
        Self::Runtime(value)
    }
}

fn verify_artifact(root: &Path, name: &str, expected: &str) -> Result<PathBuf, OcrError> {
    let path = root.join(name);
    let canonical = path
        .canonicalize()
        .map_err(|_| OcrError::MissingArtifact { path: path.clone() })?;
    if !canonical.starts_with(root) || !canonical.is_file() {
        return Err(OcrError::UnsafeArtifactPath { path: canonical });
    }
    let bytes = std::fs::read(&canonical).map_err(|_| OcrError::MissingArtifact {
        path: canonical.clone(),
    })?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if actual != expected {
        return Err(OcrError::DigestMismatch {
            path: canonical,
            expected: expected.to_string(),
            actual,
        });
    }
    Ok(canonical)
}

pub fn ocr_image_with_manifest(
    path: &Path,
    model_dir: &Path,
    manifest: &OcrArtifactManifest,
) -> Result<String, OcrError> {
    let mut engine = OcrEngine::load_with_manifest(model_dir, manifest)?;
    let img = load_image(path)?;
    engine.run(&img)
}

// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
// OcrEngine — load once, reuse across slides
// ---------------------------------------------------------------------------

/// A loaded, warm OCR engine.
///
/// Build once on a dedicated worker thread; reuse across slides.
/// DirectML sessions are GPU-state-bound — keep on the same thread.
pub struct OcrEngine {
    det: Session,
    rec: Session,
    /// CTC vocabulary: index 0 = blank '\0'; 1..=N = dict chars (N = en_dict.txt
    /// line count for the characterized model); final index = ' '. The decoder indexes
    /// by the model's own output `vocab_size`, so an extra trailing entry is
    /// never reached if the model omits the space class.
    vocab: Vec<char>,
    provider_diagnostics: Vec<ProviderDiagnostic>,
}

impl OcrEngine {
    /// Load using the currently characterized manifest. This deliberately
    /// fails closed until its conflicting model provenance label is resolved.
    pub fn load(model_dir: &Path) -> Result<Self, OcrError> {
        Self::load_with_manifest(
            model_dir,
            &OcrArtifactManifest::characterized_windows_contract(),
        )
    }

    /// Load det + rec sessions from an explicit, verified model directory.
    ///
    /// Probes DirectML first on Windows and records any CPU fallback.
    ///
    /// Expected files:
    /// - `<model_dir>/det.onnx`      detector ONNX
    /// - `<model_dir>/rec.onnx`      recognizer ONNX
    /// - `<model_dir>/en_dict.txt`   character dictionary (UTF-8, one char/line)
    pub fn load_with_manifest(
        model_dir: &Path,
        manifest: &OcrArtifactManifest,
    ) -> Result<Self, OcrError> {
        let artifacts = manifest.validate_model_dir(model_dir)?;
        let vocab = load_vocab(&artifacts.dictionary)?;
        let (det, mut provider_diagnostics) = build_session(&artifacts.detector)?;
        let (rec, rec_diagnostics) = build_session(&artifacts.recognizer)?;
        provider_diagnostics.extend(rec_diagnostics);
        Ok(OcrEngine {
            det,
            rec,
            vocab,
            provider_diagnostics,
        })
    }

    pub fn provider_diagnostics(&self) -> &[ProviderDiagnostic] {
        &self.provider_diagnostics
    }

    /// OCR a pre-loaded RGB image; returns newline-joined text.
    pub fn run(&mut self, img: &RgbImage) -> Result<String, OcrError> {
        let boxes = self.detect(img)?;
        if boxes.is_empty() {
            return Ok(String::new());
        }
        // Group boxes into visual lines (baseline-overlap), then read each line
        // left-to-right. Without this, higher-resolution detection fragments big
        // display text into per-word boxes that a naive top-down sort scrambles.
        let rows = group_into_lines(boxes);
        let mut out_lines = Vec::new();
        for row in rows {
            let mut words = Vec::new();
            for bbox in &row {
                let crop = crop_box(img, bbox);
                if crop.width() == 0 || crop.height() == 0 {
                    continue;
                }
                let t = self.recognize(&crop)?.trim().to_string();
                if !t.is_empty() {
                    words.push(t);
                }
            }
            if !words.is_empty() {
                out_lines.push(words.join(" "));
            }
        }
        Ok(out_lines.join("\n"))
    }

    // -----------------------------------------------------------------------
    // Detection
    // -----------------------------------------------------------------------

    /// Detect text boxes across the whole image.
    ///
    /// Caps working width at [`DET_MAX_W`] (downscale only — never upscale) so
    /// horizontal text keeps near-native resolution, then splits tall images
    /// into overlapping vertical bands each ≤ [`DET_TILE_H`] and detects per
    /// band. Boxes are translated to original-image coordinates and
    /// de-duplicated across band seams. Recognition always crops from the
    /// full-resolution original, so higher detection resolution directly
    /// improves read accuracy.
    fn detect(&mut self, img: &RgbImage) -> Result<Vec<BBox>, String> {
        let (orig_w, orig_h) = (img.width(), img.height());

        // Width cap: downscale only. Most screenshots are ≤ DET_MAX_W → native.
        let ws = if orig_w > DET_MAX_W {
            DET_MAX_W as f32 / orig_w as f32
        } else {
            1.0
        };
        let work = if ws < 1.0 {
            let nh = ((orig_h as f32 * ws).round() as u32).max(1);
            imageops::resize(img, DET_MAX_W, nh, imageops::FilterType::Lanczos3)
        } else {
            img.clone()
        };
        let (work_w, work_h) = (work.width(), work.height());

        // Overlapping vertical bands so tall pages keep native resolution
        // instead of being crushed to a 960px long side.
        let mut bands: Vec<(u32, u32)> = Vec::new();
        if work_h <= DET_TILE_H {
            bands.push((0, work_h));
        } else {
            let step = DET_TILE_H - DET_BAND_OVERLAP;
            let mut y = 0u32;
            loop {
                let end = (y + DET_TILE_H).min(work_h);
                bands.push((y, end));
                if end >= work_h {
                    break;
                }
                y += step;
            }
        }

        let mut all: Vec<BBox> = Vec::new();
        for (ys, ye) in bands {
            let band = imageops::crop_imm(&work, 0, ys, work_w, ye - ys).to_image();
            for mut b in self.detect_one(&band)? {
                b.y0 += ys;
                b.y1 += ys;
                all.push(b);
            }
        }

        // Map working → original coordinates.
        if ws < 1.0 {
            let inv = 1.0 / ws;
            for b in all.iter_mut() {
                b.x0 = (b.x0 as f32 * inv) as u32;
                b.y0 = (b.y0 as f32 * inv) as u32;
                b.x1 = ((b.x1 as f32 * inv) as u32).min(orig_w);
                b.y1 = ((b.y1 as f32 * inv) as u32).min(orig_h);
            }
        }

        let mut boxes = dedup_boxes(all);
        boxes.sort_by(|a, b| a.y0.cmp(&b.y0).then(a.x0.cmp(&b.x0)));
        Ok(boxes)
    }

    /// Run the detector on a single band; returns boxes in band-local pixels.
    fn detect_one(&mut self, band: &RgbImage) -> Result<Vec<BBox>, String> {
        let (bw, bh) = (band.width(), band.height());
        let (net_w, net_h) = det_net_size(bw, bh);
        let resized = imageops::resize(band, net_w, net_h, imageops::FilterType::Lanczos3);

        let data = det_chw_data(&resized, net_w, net_h);
        let shape = [1i64, 3, net_h as i64, net_w as i64];
        let tensor =
            Tensor::<f32>::from_array((shape, data)).map_err(|e| format!("det tensor: {e}"))?;

        let outputs = self
            .det
            .run(ort::inputs![tensor])
            .map_err(|e| format!("det run: {e}"))?;

        // Output: [1, 1, H, W].  Extract as flat &[f32] + shape.
        let (out_shape, prob_flat) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("det extract: {e}"))?;
        // Shape derefs to &[i64]; dims are [batch, channel, H, W].
        let dims: &[i64] = out_shape;
        let (oh, ow) = match dims {
            [_, _, h, w] => (*h as usize, *w as usize),
            s => return Err(format!("unexpected det output shape: {s:?}")),
        };
        let prob_2d: &[f32] = &prob_flat[0..oh * ow];

        // Map prob-map → band pixels (handles any det output downsampling).
        let scale_x = bw as f32 / ow as f32;
        let scale_y = bh as f32 / oh as f32;
        Ok(extract_boxes(
            prob_2d, ow, oh, scale_x, scale_y, 0.3, bw, bh,
        ))
    }

    // -----------------------------------------------------------------------
    // Recognition
    // -----------------------------------------------------------------------

    fn recognize(&mut self, crop: &RgbImage) -> Result<String, String> {
        let (h, w) = (crop.height(), crop.width());
        let new_w = ((48.0 / h as f32) * w as f32).max(4.0) as u32;
        let strip = imageops::resize(crop, new_w, 48, imageops::FilterType::Lanczos3);

        let data = rec_chw_data(&strip, new_w);
        let shape = [1i64, 3, 48, new_w as i64];
        let tensor =
            Tensor::<f32>::from_array((shape, data)).map_err(|e| format!("rec tensor: {e}"))?;

        let outputs = self
            .rec
            .run(ort::inputs![tensor])
            .map_err(|e| format!("rec run: {e}"))?;

        // Output: [1, T, vocab_size].
        let (out_shape, logits_flat) = outputs[0]
            .try_extract_tensor::<f32>()
            .map_err(|e| format!("rec extract: {e}"))?;
        // Shape derefs to &[i64]; dims are [batch, T, vocab_size].
        let dims: &[i64] = out_shape;
        let (t_len, vocab_size) = match dims {
            [_, t, v] => (*t as usize, *v as usize),
            s => return Err(format!("unexpected rec output shape: {s:?}")),
        };
        // logits_flat: [1, T, V]; skip batch dim.
        let logits = &logits_flat[0..t_len * vocab_size];

        Ok(ctc_greedy_decode(logits, t_len, vocab_size, &self.vocab))
    }
}

// ---------------------------------------------------------------------------
