//! Face detection: YuNet through ONNX Runtime, plus the pure decode/NMS it needs.

use crate::error::{ReframeError, Result};
use crate::frame::RgbFrame;
use crate::ort_rt::{build_session, EpReport};
use rightkit_onnx::ort::session::Session;
use rightkit_onnx::ort::value::Tensor;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// YuNet's fixed network input edge.
pub const NET: usize = 640;
const STRIDES: [usize; 3] = [8, 16, 32];

/// One detected face in normalized `[0,1]` frame coordinates (top-left origin).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Face {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub score: f32,
    /// Right eye, left eye, nose tip, right mouth corner, left mouth corner (image coords).
    pub landmarks: [(f32, f32); 5],
}

impl Face {
    pub fn center(&self) -> (f32, f32) {
        (self.x + self.w / 2.0, self.y + self.h / 2.0)
    }
    pub fn area(&self) -> f32 {
        self.w * self.h
    }
    /// Normalized box around the mouth, from the two mouth-corner landmarks.
    pub fn mouth_region(&self) -> (f32, f32, f32, f32) {
        let (ax, ay) = self.landmarks[3];
        let (bx, by) = self.landmarks[4];
        let cx = (ax + bx) / 2.0;
        let cy = (ay + by) / 2.0;
        let half_w = ((ax - bx).abs() * 0.9).max(self.w * 0.2);
        let half_h = (self.h * 0.14).max(0.004);
        (cx - half_w, cy - half_h, cx + half_w, cy + half_h)
    }
}

pub fn iou(a: &Face, b: &Face) -> f32 {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    let inter = (x1 - x0).max(0.0) * (y1 - y0).max(0.0);
    let union = a.area() + b.area() - inter;
    if union <= 0.0 {
        0.0
    } else {
        inter / union
    }
}

/// Greedy non-maximum suppression, highest score first.
pub fn nms(mut faces: Vec<Face>, iou_threshold: f32) -> Vec<Face> {
    faces.sort_by(|a, b| b.score.total_cmp(&a.score));
    let mut keep: Vec<Face> = Vec::new();
    for f in faces {
        if keep.iter().all(|k| iou(k, &f) <= iou_threshold) {
            keep.push(f);
        }
    }
    keep
}

/// Raw YuNet head outputs for one stride: `cls`/`obj` are `n`, `bbox` `n*4`, `kps` `n*10`.
pub struct Head<'a> {
    pub stride: usize,
    pub cls: &'a [f32],
    pub obj: &'a [f32],
    pub bbox: &'a [f32],
    pub kps: &'a [f32],
}

/// Decode YuNet heads into faces in *network pixels* (before letterbox removal).
pub fn decode_heads(heads: &[Head<'_>], score_threshold: f32) -> Vec<Face> {
    let mut out = Vec::new();
    for h in heads {
        let cols = NET / h.stride;
        let rows = NET / h.stride;
        let s = h.stride as f32;
        for r in 0..rows {
            for c in 0..cols {
                let i = r * cols + c;
                if i >= h.cls.len() || i * 10 + 9 >= h.kps.len() || i * 4 + 3 >= h.bbox.len() {
                    continue;
                }
                let score = (h.cls[i].clamp(0.0, 1.0) * h.obj[i].clamp(0.0, 1.0)).sqrt();
                if score < score_threshold {
                    continue;
                }
                let cx = (c as f32 + h.bbox[i * 4]) * s;
                let cy = (r as f32 + h.bbox[i * 4 + 1]) * s;
                let w = h.bbox[i * 4 + 2].exp() * s;
                let hh = h.bbox[i * 4 + 3].exp() * s;
                let mut landmarks = [(0.0, 0.0); 5];
                for (n, lm) in landmarks.iter_mut().enumerate() {
                    *lm = (
                        (h.kps[i * 10 + 2 * n] + c as f32) * s,
                        (h.kps[i * 10 + 2 * n + 1] + r as f32) * s,
                    );
                }
                out.push(Face {
                    x: cx - w / 2.0,
                    y: cy - hh / 2.0,
                    w,
                    h: hh,
                    score,
                    landmarks,
                });
            }
        }
    }
    out
}

/// Anything that finds faces in a frame (the real detector, or a scripted one in tests).
pub trait FaceDetector {
    fn detect(&mut self, frame: &RgbFrame) -> Result<Vec<Face>>;
}

/// What the plan records about the detector that produced its evidence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DetectorInfo {
    pub model: String,
    pub model_sha256: String,
    pub license: String,
    pub execution_provider: String,
    pub requested_provider: String,
    pub fallback_reason: Option<String>,
    pub ort_dylib: String,
}

pub struct YuNet {
    session: Session,
    pub report: EpReport,
    score_threshold: f32,
}

impl YuNet {
    pub fn load(model: &Path) -> Result<Self> {
        let (session, report) = build_session(model)?;
        Ok(Self {
            session,
            report,
            score_threshold: 0.6,
        })
    }

    pub fn info(&self) -> DetectorInfo {
        DetectorInfo {
            model: crate::models::YUNET_FILE.to_string(),
            model_sha256: crate::models::YUNET_SHA256.to_string(),
            license: crate::models::YUNET_LICENSE.to_string(),
            execution_provider: self.report.used.as_str().to_string(),
            requested_provider: self.report.requested.as_str().to_string(),
            fallback_reason: self.report.fallback_reason.clone(),
            ort_dylib: self.report.ort_dylib.clone(),
        }
    }
}

/// Letterbox into a 640x640 BGR CHW float tensor (top-left anchored, black padding), 0..255.
fn preprocess(frame: &RgbFrame) -> (Vec<f32>, f32) {
    let scale = (NET as f32 / frame.width.max(frame.height) as f32).min(1.0);
    let (dw, dh) = (
        ((frame.width as f32 * scale).round() as usize).clamp(1, NET),
        ((frame.height as f32 * scale).round() as usize).clamp(1, NET),
    );
    let plane = NET * NET;
    let mut data = vec![0f32; 3 * plane];
    for y in 0..dh {
        let sy = if scale < 1.0 {
            ((y as f32 / scale) as usize).min(frame.height - 1)
        } else {
            y
        };
        for x in 0..dw {
            let sx = if scale < 1.0 {
                ((x as f32 / scale) as usize).min(frame.width - 1)
            } else {
                x
            };
            let p = (sy * frame.width + sx) * 3;
            let o = y * NET + x;
            data[o] = frame.data[p + 2] as f32; // B
            data[plane + o] = frame.data[p + 1] as f32; // G
            data[2 * plane + o] = frame.data[p] as f32; // R
        }
    }
    (data, scale)
}

impl FaceDetector for YuNet {
    fn detect(&mut self, frame: &RgbFrame) -> Result<Vec<Face>> {
        let (data, scale) = preprocess(frame);
        let tensor = Tensor::<f32>::from_array(([1i64, 3, NET as i64, NET as i64], data))
            .map_err(|e| ReframeError::Ort(format!("input tensor: {e}")))?;
        let outputs = self
            .session
            .run(rightkit_onnx::ort::inputs![tensor])
            .map_err(|e| ReframeError::Ort(format!("run: {e}")))?;
        let get = |name: &str| -> Result<Vec<f32>> {
            let (_, v) = outputs[name]
                .try_extract_tensor::<f32>()
                .map_err(|e| ReframeError::Ort(format!("extract {name}: {e}")))?;
            Ok(v.to_vec())
        };
        let mut storage = Vec::new();
        for s in STRIDES {
            storage.push((
                s,
                get(&format!("cls_{s}"))?,
                get(&format!("obj_{s}"))?,
                get(&format!("bbox_{s}"))?,
                get(&format!("kps_{s}"))?,
            ));
        }
        let heads: Vec<Head<'_>> = storage
            .iter()
            .map(|(s, cls, obj, bbox, kps)| Head {
                stride: *s,
                cls,
                obj,
                bbox,
                kps,
            })
            .collect();
        let raw = decode_heads(&heads, self.score_threshold);
        // network px -> original frame px -> normalized
        let (fw, fh) = (frame.width as f32, frame.height as f32);
        let norm = |x: f32, y: f32| ((x / scale / fw), (y / scale / fh));
        let faces: Vec<Face> = nms(raw, 0.3)
            .into_iter()
            .map(|f| {
                let (x0, y0) = norm(f.x, f.y);
                let (x1, y1) = norm(f.x + f.w, f.y + f.h);
                let mut lm = [(0.0, 0.0); 5];
                for (o, i) in lm.iter_mut().zip(f.landmarks.iter()) {
                    *o = norm(i.0, i.1);
                }
                Face {
                    x: x0,
                    y: y0,
                    w: x1 - x0,
                    h: y1 - y0,
                    score: f.score,
                    landmarks: lm,
                }
            })
            .filter(|f| {
                f.w > 0.0
                    && f.h > 0.0
                    && f.x < 1.0
                    && f.y < 1.0
                    && f.x + f.w > 0.0
                    && f.y + f.h > 0.0
            })
            .collect();
        Ok(faces)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face(x: f32, y: f32, w: f32, h: f32, s: f32) -> Face {
        Face {
            x,
            y,
            w,
            h,
            score: s,
            landmarks: [(0.0, 0.0); 5],
        }
    }

    #[test]
    fn nms_keeps_the_best_of_overlapping_boxes_and_separate_faces() {
        let kept = nms(
            vec![
                face(0.1, 0.1, 0.2, 0.2, 0.7),
                face(0.11, 0.1, 0.2, 0.2, 0.9),
                face(0.6, 0.1, 0.2, 0.2, 0.8),
            ],
            0.3,
        );
        assert_eq!(kept.len(), 2);
        assert!((kept[0].score - 0.9).abs() < 1e-6);
    }

    #[test]
    fn decode_places_a_cell_at_its_stride_grid() {
        // One positive cell at stride 32, row 2 col 3: bbox offset 0.5,0.5 and log-size ln(2).
        let n = (NET / 32) * (NET / 32);
        let mut cls = vec![0.0; n];
        let mut obj = vec![0.0; n];
        let mut bbox = vec![0.0; n * 4];
        let kps = vec![0.0; n * 10];
        let i = 2 * (NET / 32) + 3;
        cls[i] = 1.0;
        obj[i] = 1.0;
        bbox[i * 4] = 0.5;
        bbox[i * 4 + 1] = 0.5;
        bbox[i * 4 + 2] = 2f32.ln();
        bbox[i * 4 + 3] = 2f32.ln();
        let faces = decode_heads(
            &[Head {
                stride: 32,
                cls: &cls,
                obj: &obj,
                bbox: &bbox,
                kps: &kps,
            }],
            0.5,
        );
        assert_eq!(faces.len(), 1);
        let f = faces[0];
        // centre = (3.5*32, 2.5*32) = (112, 80); size 64.
        assert!((f.x + f.w / 2.0 - 112.0).abs() < 1e-3);
        assert!((f.y + f.h / 2.0 - 80.0).abs() < 1e-3);
        assert!((f.w - 64.0).abs() < 1e-3);
    }

    #[test]
    fn preprocess_letterboxes_top_left_and_swaps_to_bgr() {
        let frame = RgbFrame::solid(320, 160, [10, 20, 30]);
        let (data, scale) = preprocess(&frame);
        assert!((scale - 1.0).abs() < 1e-6, "no upscaling");
        let plane = NET * NET;
        assert_eq!(data[0], 30.0);
        assert_eq!(data[plane], 20.0);
        assert_eq!(data[2 * plane], 10.0);
        // padding region is zero
        assert_eq!(data[(200) * NET + 10], 0.0);
    }
}
