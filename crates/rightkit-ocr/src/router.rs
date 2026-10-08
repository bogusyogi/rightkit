//! Backend selection and native-first fallback (ScrapeRight's routing).
//!
//! `Auto` runs the OS-native backend and falls back to the two-stage ONNX path
//! **only** when the native backend proves unusable on this machine
//! ([`crate::is_backend_unavailable`]) — never on quality, never on a
//! per-image fault (those propagate, so a bug is not masked by degraded text).
//! The result always says which backend produced it and why a fallback fired.

use std::path::{Path, PathBuf};

use image::RgbImage;
#[cfg(feature = "onnx")]
use rightkit_ort::{configure_runtime, CandidateSource, RuntimeCandidate};
use serde::Serialize;

use crate::native;
use crate::types::{decode_encoded, is_backend_unavailable, OcrBackend, OcrLine};
use crate::worker::WorkerCommand;
#[cfg(feature = "onnx")]
use crate::OcrEngine;
use crate::{OcrArtifactManifest, OcrError};

/// The loaded ONNX engine slot. Without the `onnx` feature it is uninhabited, so the
/// slot is always `None` and no ONNX runtime code is compiled in.
#[cfg(feature = "onnx")]
type OnnxSlot = OcrEngine;
#[cfg(not(feature = "onnx"))]
type OnnxSlot = std::convert::Infallible;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendPreference {
    /// Native first, ONNX fallback when native is unavailable.
    Auto,
    /// Native only; an unavailable backend is an error.
    Native,
    /// ONNX only (e.g. benchmarking or a deliberate cross-platform baseline).
    Onnx,
}

/// Where native recognition runs.
#[derive(Debug, Clone, Default)]
pub enum Isolation {
    /// In this process (Vision still self-checks blank results).
    #[default]
    InProcess,
    /// One short-lived child per request, canary in a fresh child on blank
    /// results (ViewRight's protocol; required for repeated accurate Vision
    /// requests on affected Macs).
    Worker(WorkerCommand),
}

/// Verified ONNX model set used by the fallback / `Onnx` preference.
#[derive(Debug, Clone)]
pub struct OnnxConfig {
    pub model_dir: PathBuf,
    pub manifest: OcrArtifactManifest,
    /// Absolute ONNX Runtime library; `None` keeps whatever the caller already
    /// configured through `rightkit-ort` / `ORT_DYLIB_PATH`.
    pub ort_runtime: Option<PathBuf>,
}

impl OnnxConfig {
    pub fn new(model_dir: impl Into<PathBuf>, manifest: OcrArtifactManifest) -> Self {
        Self {
            model_dir: model_dir.into(),
            manifest,
            ort_runtime: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct OcrOptions {
    pub preference: BackendPreference,
    pub onnx: Option<OnnxConfig>,
    pub isolation: Isolation,
}

impl OcrOptions {
    pub fn new(preference: BackendPreference) -> Self {
        Self {
            preference,
            onnx: None,
            isolation: Isolation::InProcess,
        }
    }
    pub fn with_onnx(mut self, onnx: OnnxConfig) -> Self {
        self.onnx = Some(onnx);
        self
    }
    pub fn with_isolation(mut self, isolation: Isolation) -> Self {
        self.isolation = isolation;
        self
    }
}

impl Default for OcrOptions {
    fn default() -> Self {
        Self::new(BackendPreference::Auto)
    }
}

/// Text + geometry + confidence + provenance.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OcrResult {
    pub lines: Vec<OcrLine>,
    pub backend: OcrBackend,
    /// Set when `Auto` fell back: the native backend's unavailability error.
    pub fallback_reason: Option<String>,
}

impl OcrResult {
    /// Newline-joined text (ScrapeRight / CodeRight shape).
    pub fn text(&self) -> String {
        self.lines
            .iter()
            .map(|l| l.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// A reusable OCR handle. The ONNX engine loads lazily on first need and is
/// then kept warm.
pub struct Ocr {
    options: OcrOptions,
    #[cfg_attr(not(feature = "onnx"), allow(dead_code))]
    onnx: Option<OnnxSlot>,
}

impl Ocr {
    pub fn new(options: OcrOptions) -> Self {
        Self {
            options,
            onnx: None,
        }
    }

    pub fn recognize_file(&mut self, path: &Path) -> Result<OcrResult, OcrError> {
        let bytes = std::fs::read(path)
            .map_err(|e| OcrError::Runtime(format!("Cannot read image {path:?}: {e}")))?;
        self.recognize_encoded(&bytes)
    }

    /// Decode (with the pixel guard) and recognize PNG/JPEG bytes. In worker
    /// isolation the original bytes go to the child unchanged.
    pub fn recognize_encoded(&mut self, bytes: &[u8]) -> Result<OcrResult, OcrError> {
        let img = decode_encoded(bytes)?;
        self.recognize_inner(&img, Some(bytes))
    }

    pub fn recognize(&mut self, img: &RgbImage) -> Result<OcrResult, OcrError> {
        self.recognize_inner(img, None)
    }

    /// Is the native backend usable here? Probes with the embedded known-text
    /// canary; `Ok(false)` is a stable capability verdict, `Err` a transient
    /// failure the caller should retry rather than cache.
    pub fn probe_native(&self) -> Result<bool, String> {
        if native::native_backend().is_none() {
            return Ok(false);
        }
        let result = match &self.options.isolation {
            Isolation::Worker(cmd) => cmd.run(native::canary_png()),
            Isolation::InProcess => {
                decode_encoded(native::canary_png()).and_then(|i| native::recognize(&i))
            }
        };
        match result {
            Ok(lines) if !lines.is_empty() => Ok(true),
            Ok(_) => Err("native canary returned no observations".into()),
            Err(e) if is_backend_unavailable(&e) => Ok(false),
            Err(e) => Err(e),
        }
    }

    fn recognize_inner(
        &mut self,
        img: &RgbImage,
        encoded: Option<&[u8]>,
    ) -> Result<OcrResult, OcrError> {
        match self.options.preference {
            BackendPreference::Onnx => self.run_onnx(img, None),
            BackendPreference::Native => {
                let lines = self.run_native(img, encoded).map_err(native_error)?;
                Ok(native_result(lines, None))
            }
            BackendPreference::Auto => match self.run_native(img, encoded) {
                Ok(lines) => Ok(native_result(lines, None)),
                Err(e) if is_backend_unavailable(&e) => {
                    if self.options.onnx.is_none() {
                        return Err(OcrError::NoFallbackConfigured(e));
                    }
                    self.run_onnx(img, Some(e))
                }
                Err(e) => Err(OcrError::Runtime(e)),
            },
        }
    }

    fn run_native(&self, img: &RgbImage, encoded: Option<&[u8]>) -> Result<Vec<OcrLine>, String> {
        let Some(backend) = native::native_backend() else {
            return Err(format!(
                "{} no native OCR backend on this platform",
                crate::types::OCR_UNSUPPORTED_PLATFORM
            ));
        };
        match &self.options.isolation {
            Isolation::InProcess => native::recognize(img),
            Isolation::Worker(cmd) => {
                let owned;
                let bytes = match encoded {
                    Some(b) => b,
                    None => {
                        owned = encode_png(img)?;
                        &owned
                    }
                };
                let lines = cmd.run(bytes)?;
                if !lines.is_empty() || backend != OcrBackend::Vision {
                    return Ok(lines);
                }
                // Vision cannot tell "blank page" from "could not run": verify
                // in a FRESH child before trusting an empty result.
                match cmd.run(native::canary_png()) {
                    Ok(c) if !c.is_empty() => Ok(lines),
                    Ok(_) => Err(format!(
                        "{} Vision returned no observations and the canary in a fresh worker \
                         also returned none, so text recognition is not working here",
                        crate::types::MACOS_VISION_UNAVAILABLE
                    )),
                    Err(e) => Err(e),
                }
            }
        }
    }

    #[cfg(not(feature = "onnx"))]
    fn run_onnx(
        &mut self,
        _img: &RgbImage,
        _fallback_reason: Option<String>,
    ) -> Result<OcrResult, OcrError> {
        Err(OcrError::BackendUnavailable(
            "ONNX fallback not compiled in: rebuild rightkit-ocr with the `onnx` feature".into(),
        ))
    }

    #[cfg(feature = "onnx")]
    fn run_onnx(
        &mut self,
        img: &RgbImage,
        fallback_reason: Option<String>,
    ) -> Result<OcrResult, OcrError> {
        let cfg = self.options.onnx.clone().ok_or_else(|| {
            OcrError::NoFallbackConfigured("ONNX backend requested without model config".into())
        })?;
        if self.onnx.is_none() {
            if let Some(rt) = &cfg.ort_runtime {
                configure_runtime([RuntimeCandidate::new(rt.clone(), CandidateSource::Explicit)])
                    .map_err(|e| OcrError::Runtime(format!("ONNX runtime: {e:?}")))?;
            }
            self.onnx = Some(OcrEngine::load_with_manifest(
                &cfg.model_dir,
                &cfg.manifest,
            )?);
        }
        let lines = self.onnx.as_mut().expect("loaded above").run_lines(img)?;
        Ok(OcrResult {
            lines,
            backend: OcrBackend::Onnx,
            fallback_reason,
        })
    }
}

fn native_result(lines: Vec<OcrLine>, fallback_reason: Option<String>) -> OcrResult {
    OcrResult {
        lines,
        backend: native::native_backend().expect("native result implies native backend"),
        fallback_reason,
    }
}

fn native_error(e: String) -> OcrError {
    if is_backend_unavailable(&e) {
        OcrError::BackendUnavailable(e)
    } else {
        OcrError::Runtime(e)
    }
}

fn encode_png(img: &RgbImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .map_err(|e| format!("Cannot encode image for worker: {e}"))?;
    Ok(out)
}
