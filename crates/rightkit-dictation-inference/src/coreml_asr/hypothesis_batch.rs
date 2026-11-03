//! Opt-in API batching probe over independent production hypotheses.
use super::model::CoreMlParakeet;
use crate::coreml::{ml_f16, ml_i32, read_f16};
use objc2::rc::autoreleasepool;
use rightkit_asr::parakeet_core::tdt_beam::BeamHyp;

type Predictor = (Vec<f32>, Vec<f32>, Vec<f32>);

/// Shipping config: API batching of predictor and joint (beam 2: -8.5% wall, identical output
/// on 224 clips; docs/experiments/2026-10-01), one tensor call per row group.
pub(crate) fn batch_chunk_size(row_count: usize) -> usize {
    row_count.max(1)
}

/// Rows per tensor call for a bundle's `decoder_batch`: `None` keeps the
/// batch-1 path (absent or 1).
fn tensor_rows_per_call(decoder_batch: usize) -> Option<usize> {
    (decoder_batch >= 2).then_some(decoder_batch)
}

/// Tensor batch dimension for `live` rows: flexible bundles size to the live
/// count, fixed bundles always run `n` rows (padding by duplicating the last).
fn tensor_batch_dim(n: usize, flexible: bool, live: usize) -> usize {
    if flexible {
        live
    } else {
        n
    }
}

/// `[b, width]` row-major, rows beyond `rows.len()` duplicate the last row.
fn pack_rows(rows: &[&[f32]], b: usize, width: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(b * width);
    for r in 0..b {
        out.extend_from_slice(&rows[r.min(rows.len() - 1)][..width]);
    }
    out
}

/// LSTM state `[layers, b, hidden]` from per-row `[layers, 1, hidden]` states.
fn pack_state(rows: &[&[f32]], b: usize, layers: usize, hidden: usize) -> Vec<f32> {
    let mut out = Vec::with_capacity(layers * b * hidden);
    for l in 0..layers {
        for r in 0..b {
            let row = rows[r.min(rows.len() - 1)];
            out.extend_from_slice(&row[l * hidden..(l + 1) * hidden]);
        }
    }
    out
}

/// Inverse of [`pack_state`] for the first `live` rows.
fn unpack_state(t: &[f32], b: usize, live: usize, layers: usize, hidden: usize) -> Vec<Vec<f32>> {
    (0..live)
        .map(|r| {
            let mut row = Vec::with_capacity(layers * hidden);
            for l in 0..layers {
                let at = (l * b + r) * hidden;
                row.extend_from_slice(&t[at..at + hidden]);
            }
            row
        })
        .collect()
}

/// First `live` rows of a `[b, ...]` tensor.
fn unpack_rows(t: &[f32], b: usize, live: usize) -> Vec<Vec<f32>> {
    let width = t.len() / b;
    (0..live)
        .map(|r| t[r * width..(r + 1) * width].to_vec())
        .collect()
}

type DecoderRow<'a> = (i32, &'a [f32], &'a [f32]);

/// Drive decoder rows through `call(b, ids[b], h[pl,b,ph], c[pl,b,ph])`
/// returning `(g[b,..], h, c)` tensors, chunking by `n`.
fn decode_rows_tensor<F>(
    rows: &[DecoderRow],
    n: usize,
    flexible: bool,
    pl: usize,
    ph: usize,
    mut call: F,
) -> Result<Vec<Predictor>, String>
where
    F: FnMut(usize, &[i32], &[f32], &[f32]) -> Result<Predictor, String>,
{
    let mut out = Vec::with_capacity(rows.len());
    for chunk in rows.chunks(n) {
        let b = tensor_batch_dim(n, flexible, chunk.len());
        let mut ids: Vec<i32> = chunk.iter().map(|r| r.0).collect();
        ids.resize(b, chunk[chunk.len() - 1].0);
        let hs: Vec<&[f32]> = chunk.iter().map(|r| r.1).collect();
        let cs: Vec<&[f32]> = chunk.iter().map(|r| r.2).collect();
        let (g, h, c) = call(
            b,
            &ids,
            &pack_state(&hs, b, pl, ph),
            &pack_state(&cs, b, pl, ph),
        )?;
        let gs = unpack_rows(&g, b, chunk.len());
        let hs = unpack_state(&h, b, chunk.len(), pl, ph);
        let cs = unpack_state(&c, b, chunk.len(), pl, ph);
        for ((g, h), c) in gs.into_iter().zip(hs).zip(cs) {
            out.push((g, h, c));
        }
    }
    Ok(out)
}

/// Drive joint rows `(g, enc)` through `call(b, enc[b,enc_d], g[b,ph])`.
fn joint_rows_tensor<F>(
    rows: &[(&[f32], &[f32])],
    n: usize,
    flexible: bool,
    ph: usize,
    enc_d: usize,
    mut call: F,
) -> Result<Vec<Vec<f32>>, String>
where
    F: FnMut(usize, &[f32], &[f32]) -> Result<Vec<f32>, String>,
{
    let mut out = Vec::with_capacity(rows.len());
    for chunk in rows.chunks(n) {
        let b = tensor_batch_dim(n, flexible, chunk.len());
        let gs: Vec<&[f32]> = chunk.iter().map(|r| r.0).collect();
        let es: Vec<&[f32]> = chunk.iter().map(|r| r.1).collect();
        let logits = call(b, &pack_rows(&es, b, enc_d), &pack_rows(&gs, b, ph))?;
        out.extend(unpack_rows(&logits, b, chunk.len()));
    }
    Ok(out)
}

impl CoreMlParakeet {
    /// `Some(n)` when the bundle's TextDecoder/Joint are true tensor-batched.
    pub(crate) fn tensor_batch_rows(&self) -> Option<usize> {
        tensor_rows_per_call(self.meta.decoder_batch)
    }

    /// One prediction per `decoder_batch` rows (tensor batching).
    pub(crate) fn decoder_rows_tensor(
        &self,
        n: usize,
        rows: &[DecoderRow],
    ) -> Result<Vec<Predictor>, String> {
        let (pl, ph) = (self.meta.pred_layers, self.meta.pred_hidden);
        decode_rows_tensor(
            rows,
            n,
            self.meta.decoder_batch_flexible,
            pl,
            ph,
            |b, ids, h, c| {
                autoreleasepool(|_| {
                    let ids = ml_i32(&[b, 1], ids)?;
                    let hin = ml_f16(&[pl, b, ph], h)?;
                    let cin = ml_f16(&[pl, b, ph], c)?;
                    let outs = self.dec.predict_multi_shaped(
                        &[
                            ("decoder_input_ids", &ids),
                            ("state_h", &hin),
                            ("state_c", &cin),
                        ],
                        &[&self.g_name, &self.state_h_out, &self.state_c_out],
                        &[&[b, 1, ph], &[pl, b, ph], &[pl, b, ph]],
                    )?;
                    Ok((read_f16(&outs[0]), read_f16(&outs[1]), read_f16(&outs[2])))
                })
            },
        )
    }

    pub(crate) fn joint_rows_tensor(
        &self,
        n: usize,
        rows: &[(&[f32], &[f32])],
    ) -> Result<Vec<Vec<f32>>, String> {
        let ph = self.meta.pred_hidden;
        joint_rows_tensor(
            rows,
            n,
            self.meta.decoder_batch_flexible,
            ph,
            1024,
            |b, e, g| {
                autoreleasepool(|_| {
                    let enc = ml_f16(&[b, 1024], e)?;
                    let dec = ml_f16(&[b, ph], g)?;
                    let out = self.joint.predict(
                        &[("encoder_step", &enc), ("decoder_step", &dec)],
                        &self.joint_out,
                    )?;
                    Ok(read_f16(&out))
                })
            },
        )
    }

    pub(crate) fn predict_hypotheses(
        &self,
        hypotheses: &[&BeamHyp],
    ) -> Result<Vec<Predictor>, String> {
        if let Some(n) = self.tensor_batch_rows() {
            let rows: Vec<DecoderRow> = hypotheses
                .iter()
                .map(|h| (h.last_tok, h.h.as_slice(), h.c.as_slice()))
                .collect();
            return self.decoder_rows_tensor(n, &rows);
        }
        autoreleasepool(|_| {
            let inputs = hypotheses
                .iter()
                .map(|h| {
                    Ok((
                        ml_i32(&[1, 1], &[h.last_tok])?,
                        ml_f16(&[self.meta.pred_layers, 1, self.meta.pred_hidden], &h.h)?,
                        ml_f16(&[self.meta.pred_layers, 1, self.meta.pred_hidden], &h.c)?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let rows: Vec<_> = inputs
                .iter()
                .map(|(ids, h, c)| vec![("decoder_input_ids", ids), ("state_h", h), ("state_c", c)])
                .collect();
            let outputs = self.dec.predict_feature_batch(
                &rows,
                &[&self.g_name, &self.state_h_out, &self.state_c_out],
            )?;
            Ok(outputs
                .iter()
                .map(|r| (read_f16(&r[0]), read_f16(&r[1]), read_f16(&r[2])))
                .collect())
        })
    }
    pub(crate) fn predict_hypothesis_joints(
        &self,
        inputs: &[(&[f32], &[f32])],
    ) -> Result<Vec<Vec<f32>>, String> {
        if let Some(n) = self.tensor_batch_rows() {
            return self.joint_rows_tensor(n, inputs);
        }
        autoreleasepool(|_| {
            let arrays = inputs
                .iter()
                .map(|(g, e)| {
                    Ok((
                        ml_f16(&[1, self.meta.pred_hidden], g)?,
                        ml_f16(&[1, 1024], e)?,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            let rows: Vec<_> = arrays
                .iter()
                .map(|(g, e)| vec![("encoder_step", e), ("decoder_step", g)])
                .collect();
            let outputs = self
                .joint
                .predict_feature_batch(&rows, &[&self.joint_out])?;
            Ok(outputs.iter().map(|r| read_f16(&r[0])).collect())
        })
    }
    /// Graph/state sanity before corpus probes: different tokens, h/c & frames.
    /// Returns max differences for projection, recurrent h/c & joint logits.
    pub fn hypothesis_batch_sanity(&self) -> Result<[f32; 4], String> {
        let pl = self.meta.pred_layers;
        let ph = self.meta.pred_hidden;
        let hypotheses: Vec<_> = (0..5)
            .map(|i| BeamHyp {
                h: (0..pl * ph)
                    .map(|j| ((j + i) % 11) as f32 * 0.001)
                    .collect(),
                c: (0..pl * ph)
                    .map(|j| ((j + i * 3) % 13) as f32 * 0.001)
                    .collect(),
                last_tok: if i == 0 { self.meta.blank_id } else { i as i32 },
                t: i,
                sym: 0,
                score: 0.0,
                hits: Vec::new(),
            })
            .collect();
        let frames: Vec<Vec<f32>> = (0..5)
            .map(|i| {
                (0..1024)
                    .map(|j| ((j + i * 7) % 17) as f32 * 0.01)
                    .collect()
            })
            .collect();
        let mut maxima = [0.0f32; 4];
        for count in [1usize, 2, 5] {
            let refs: Vec<_> = hypotheses[..count].iter().collect();
            let batch = self.predict_hypotheses(&refs)?;
            let joint_inputs: Vec<_> = batch
                .iter()
                .zip(&frames)
                .map(|(p, e)| (p.0.as_slice(), e.as_slice()))
                .collect();
            let joints = self.predict_hypothesis_joints(&joint_inputs)?;
            for i in 0..count {
                let h = &hypotheses[i];
                let scalar = self.predict_decoder(h.last_tok, &h.h, &h.c, pl, ph)?;
                let logits = self.predict_joint(&scalar.0, &frames[i], ph, 1024)?;
                for (k, (left, right)) in [
                    (&scalar.0, &batch[i].0),
                    (&scalar.1, &batch[i].1),
                    (&scalar.2, &batch[i].2),
                    (&logits, &joints[i]),
                ]
                .into_iter()
                .enumerate()
                {
                    if left.len() != right.len() {
                        return Err("batch sanity shape mismatch".into());
                    }
                    for (&a, &b) in left.iter().zip(right) {
                        if !a.is_finite() || !b.is_finite() {
                            return Err("batch sanity nonfinite".into());
                        }
                        maxima[k] = maxima[k].max((a - b).abs());
                    }
                }
            }
        }
        Ok(maxima)
    }
}
