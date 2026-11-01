//! Face tracker (alpha-beta filter, the steady state of a constant-velocity Kalman filter)
//! plus mouth-motion speaker scoring. Pure; no I/O.

use crate::detect::{iou, Face};
use crate::frame::GrayFrame;

#[derive(Debug, Clone)]
pub struct Track {
    pub id: u32,
    pub face: Face,
    vx: f32,
    vy: f32,
    misses: u32,
    /// Exponential moving average of mouth-region motion (mean abs luma diff, 0..255).
    pub mouth_energy: f32,
}

#[derive(Debug, Default)]
pub struct FaceTracker {
    tracks: Vec<Track>,
    next_id: u32,
}

const ALPHA: f32 = 0.8;
const BETA: f32 = 0.4;
const MAX_MISSES: u32 = 3;

impl FaceTracker {
    pub fn reset(&mut self) {
        self.tracks.clear();
    }

    pub fn tracks(&self) -> &[Track] {
        &self.tracks
    }

    /// Associate `faces` with live tracks (`dt` seconds since the previous update).
    /// Returns the ids of the tracks aligned index-for-index with `faces`.
    pub fn update(&mut self, faces: &[Face], dt: f32) -> Vec<u32> {
        let dt = dt.max(1e-3);
        // Predicted boxes.
        let predicted: Vec<Face> = self
            .tracks
            .iter()
            .map(|t| {
                let mut f = t.face;
                f.x += t.vx * dt;
                f.y += t.vy * dt;
                f
            })
            .collect();
        // Candidate pairs scored by IoU, with a centre-distance rescue for fast motion.
        let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
        for (ti, p) in predicted.iter().enumerate() {
            for (fi, f) in faces.iter().enumerate() {
                let overlap = iou(p, f);
                let (pc, fc) = (p.center(), f.center());
                let dist = ((pc.0 - fc.0).powi(2) + (pc.1 - fc.1).powi(2)).sqrt();
                let reach = p.w.max(f.w);
                let score = if overlap > 0.1 {
                    overlap + 1.0
                } else if dist < reach {
                    1.0 - dist / reach
                } else {
                    continue;
                };
                pairs.push((score, ti, fi));
            }
        }
        pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut track_used = vec![false; self.tracks.len()];
        let mut face_track: Vec<Option<usize>> = vec![None; faces.len()];
        for (_, ti, fi) in pairs {
            if !track_used[ti] && face_track[fi].is_none() {
                track_used[ti] = true;
                face_track[fi] = Some(ti);
            }
        }
        // Update matched tracks.
        for (fi, ti) in face_track.iter().enumerate() {
            if let Some(ti) = *ti {
                let p = predicted[ti];
                let f = faces[fi];
                let t = &mut self.tracks[ti];
                let (rx, ry) = (f.x - p.x, f.y - p.y);
                t.vx += BETA * rx / dt;
                t.vy += BETA * ry / dt;
                t.face = Face {
                    x: p.x + ALPHA * rx,
                    y: p.y + ALPHA * ry,
                    w: f.w,
                    h: f.h,
                    score: f.score,
                    landmarks: f.landmarks,
                };
                t.misses = 0;
            }
        }
        for (ti, used) in track_used.iter().enumerate() {
            if !used {
                self.tracks[ti].misses += 1;
            }
        }
        let matched_ids: Vec<Option<u32>> = face_track
            .iter()
            .map(|ti| ti.map(|ti| self.tracks[ti].id))
            .collect();
        self.tracks.retain(|t| t.misses <= MAX_MISSES);
        let mut ids = Vec::with_capacity(faces.len());
        let mut new_tracks = Vec::new();
        for (fi, f) in faces.iter().enumerate() {
            match matched_ids[fi] {
                Some(id) => ids.push(id),
                None => {
                    let id = self.next_id;
                    self.next_id += 1;
                    new_tracks.push(Track {
                        id,
                        face: *f,
                        vx: 0.0,
                        vy: 0.0,
                        misses: 0,
                        mouth_energy: 0.0,
                    });
                    ids.push(id);
                }
            }
        }
        self.tracks.extend(new_tracks);
        ids
    }

    /// Fold this frame's mouth motion into each matched track's energy EMA.
    pub fn observe_mouths(
        &mut self,
        ids: &[u32],
        faces: &[Face],
        now: &GrayFrame,
        prev: Option<&GrayFrame>,
    ) {
        let Some(prev) = prev else { return };
        for (id, face) in ids.iter().zip(faces) {
            let (x0, y0, x1, y1) = face.mouth_region();
            if let Some(m) = now.region_motion(prev, x0, y0, x1, y1) {
                if let Some(t) = self.tracks.iter_mut().find(|t| t.id == *id) {
                    t.mouth_energy = 0.5 * t.mouth_energy + 0.5 * m;
                }
            }
        }
    }

    pub fn energy(&self, id: u32) -> f32 {
        self.tracks
            .iter()
            .find(|t| t.id == id)
            .map_or(0.0, |t| t.mouth_energy)
    }
}

/// Picks which face is speaking, with continuity.
#[derive(Debug, Default)]
pub struct SpeakerSelector {
    current: Option<u32>,
}

impl SpeakerSelector {
    pub fn reset(&mut self) {
        self.current = None;
    }

    /// `candidates` = `(track id, mouth energy, face area)`. Returns `(id, dominance 0..1)`.
    pub fn pick(&mut self, candidates: &[(u32, f32, f32)]) -> Option<(u32, f32)> {
        if candidates.is_empty() {
            self.current = None;
            return None;
        }
        if candidates.len() == 1 {
            self.current = Some(candidates[0].0);
            return Some((candidates[0].0, 1.0));
        }
        let mut order: Vec<&(u32, f32, f32)> = candidates.iter().collect();
        order.sort_by(|a, b| b.1.total_cmp(&a.1));
        let (best, second) = (order[0], order[1]);
        const MIN_ENERGY: f32 = 1.5;
        const RATIO: f32 = 1.25;
        if best.1 >= MIN_ENERGY && best.1 >= second.1 * RATIO {
            self.current = Some(best.0);
            let dom = ((best.1 - second.1) / best.1).clamp(0.0, 1.0);
            return Some((best.0, dom));
        }
        // Not decisive: keep the current speaker if still present, else the biggest face.
        if let Some(cur) = self.current {
            if candidates.iter().any(|c| c.0 == cur) {
                return Some((cur, 0.2));
            }
        }
        let biggest = candidates
            .iter()
            .max_by(|a, b| a.2.total_cmp(&b.2))
            .unwrap();
        self.current = Some(biggest.0);
        Some((biggest.0, 0.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(x: f32, y: f32, s: f32) -> Face {
        Face {
            x,
            y,
            w: 0.1,
            h: 0.15,
            score: s,
            landmarks: [
                (x, y),
                (x + 0.05, y),
                (x, y + 0.02),
                (x, y + 0.1),
                (x + 0.05, y + 0.1),
            ],
        }
    }

    #[test]
    fn identities_are_stable_across_moving_and_swapped_input_order() {
        let mut t = FaceTracker::default();
        let ids0 = t.update(&[f(0.1, 0.4, 0.9), f(0.7, 0.4, 0.9)], 0.25);
        let ids1 = t.update(&[f(0.72, 0.4, 0.9), f(0.12, 0.4, 0.9)], 0.25);
        assert_eq!(ids1[0], ids0[1]);
        assert_eq!(ids1[1], ids0[0]);
        assert_ne!(ids0[0], ids0[1]);
    }

    #[test]
    fn a_track_survives_short_dropouts_but_not_long_ones() {
        let mut t = FaceTracker::default();
        let a = t.update(&[f(0.4, 0.4, 0.9)], 0.25)[0];
        for _ in 0..3 {
            t.update(&[], 0.25);
        }
        assert_eq!(t.update(&[f(0.41, 0.4, 0.9)], 0.25)[0], a);
        for _ in 0..5 {
            t.update(&[], 0.25);
        }
        assert_ne!(t.update(&[f(0.41, 0.4, 0.9)], 0.25)[0], a);
    }

    #[test]
    fn speaker_needs_a_decisive_mouth_signal_otherwise_keeps_continuity() {
        let mut s = SpeakerSelector::default();
        let (id, _) = s.pick(&[(1, 9.0, 0.02), (2, 2.0, 0.03)]).unwrap();
        assert_eq!(id, 1);
        // ambiguous energies: stays on 1 even though 2 is bigger
        let (id, _) = s.pick(&[(1, 3.0, 0.02), (2, 3.2, 0.03)]).unwrap();
        assert_eq!(id, 1);
        // decisive hand-off
        let (id, _) = s.pick(&[(1, 1.0, 0.02), (2, 8.0, 0.03)]).unwrap();
        assert_eq!(id, 2);
    }
}
