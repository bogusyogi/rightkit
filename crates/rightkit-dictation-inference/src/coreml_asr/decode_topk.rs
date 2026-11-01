// EXPERIMENT (examples/beam_eval): greedy decode that also records each emitted
// token's top-k alternatives (the acoustic confusion network). Not wired into
// the shipped engine.
use objc2::rc::autoreleasepool;

use crate::coreml::{ml_f16, ml_i32, read_f16};

use super::detok::detok;
use super::model::CoreMlParakeet;
use super::stats::argmax;

impl CoreMlParakeet {
    /// EXPERIMENT (examples/beam_eval): greedy decode that ALSO records, per emitted
    /// token, the top-`k` alternatives `(piece_id, logit)` from the joint — i.e. the
    /// acoustic confusion network. The 1-best path is identical to `decode_window`
    /// (plain argmax, no bias). Not wired into the shipped engine.
    fn decode_window_topk(
        &self,
        seg: &[f32],
        max_symbols: usize,
        k: usize,
        out: &mut Vec<(usize, Vec<(usize, f32)>)>,
    ) -> Result<(), String> {
        let m = &self.meta;
        let pl = m.pred_layers;
        let ph = m.pred_hidden;
        let blank = m.blank_id;
        let nd = m.durations.len();
        let mut padded = vec![0f32; self.win_samples];
        padded[..seg.len()].copy_from_slice(seg);
        let mel_full = autoreleasepool(|_| -> Result<Vec<f32>, String> {
            let audio_in = ml_f16(&[1, self.win_samples], &padded)?;
            let len_in = ml_i32(&[1], &[seg.len() as i32])?;
            let o = self.mel.predict(
                &[("audio", &audio_in), ("audio_len", &len_in)],
                &self.mel_out,
            )?;
            Ok(read_f16(&o))
        })?;
        let mel_t_full = mel_full.len() / m.n_mel;
        let mut mel = vec![0f32; m.n_mel * m.mel_frames];
        for cc in 0..m.n_mel {
            for t in 0..m.mel_frames {
                mel[cc * m.mel_frames + t] = mel_full[cc * mel_t_full + t];
            }
        }
        let enc_flat = autoreleasepool(|_| -> Result<Vec<f32>, String> {
            let mel_in = ml_f16(&[1, m.n_mel, m.mel_frames], &mel)?;
            let len_in = ml_i32(&[1], &[m.mel_frames as i32])?;
            let o = self.enc.predict(
                &[("melspectrogram_features", &mel_in), ("mel_len", &len_in)],
                &self.enc_out,
            )?;
            Ok(read_f16(&o))
        })?;
        let enc_t = enc_flat.len() / 1024;
        let enc_d = 1024usize;
        let encoded = |t: usize| -> Vec<f32> {
            let mut v = vec![0f32; enc_d];
            for d in 0..enc_d {
                v[d] = enc_flat[d * enc_t + t];
            }
            v
        };
        let nframes = enc_t.min(((seg.len() as f32) / 160.0 / 8.0).ceil() as usize);
        let topk = |logits: &[f32]| -> Vec<(usize, f32)> {
            let mut idx: Vec<usize> = (0..logits.len()).collect();
            idx.sort_unstable_by(|&a, &b| {
                logits[b]
                    .partial_cmp(&logits[a])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            idx.into_iter().take(k).map(|i| (i, logits[i])).collect()
        };
        let mut h = vec![0f32; pl * ph];
        let mut c = vec![0f32; pl * ph];
        let mut last_tok: i32 = blank;
        let mut t = 0usize;
        while t < nframes {
            let enc_step = encoded(t);
            let mut sym = 0usize;
            loop {
                if sym >= max_symbols {
                    t += 1;
                    break;
                }
                let (_g, new_h, new_c, logits) =
                    self.predict_decoder_joint(last_tok, &h, &c, &enc_step, pl, ph, enc_d)?;
                if m.is_tdt {
                    let tok_logits = &logits[..logits.len() - nd];
                    let tok = argmax(tok_logits);
                    let dur = m.durations[argmax(&logits[logits.len() - nd..])] as usize;
                    if tok as i32 != blank {
                        out.push((tok, topk(tok_logits)));
                        last_tok = tok as i32;
                        h = new_h;
                        c = new_c;
                        sym += 1;
                        if dur == 0 && sym < max_symbols {
                            continue;
                        }
                    }
                    t += dur.max(1);
                    break;
                } else {
                    let tok = argmax(&logits);
                    if tok as i32 == blank {
                        t += 1;
                        break;
                    }
                    out.push((tok, topk(&logits)));
                    last_tok = tok as i32;
                    h = new_h;
                    c = new_c;
                    sym += 1;
                }
            }
        }
        Ok(())
    }

    /// Greedy transcript + per-emitted-token top-`k` alternative pieces (confusion
    /// network), for the beam/lattice-rescoring experiment.
    pub fn transcribe_topk(
        &self,
        audio: &[f32],
        k: usize,
    ) -> Result<(String, Vec<Vec<String>>), String> {
        let win = self.win_samples;
        let mut all: Vec<(usize, Vec<(usize, f32)>)> = Vec::new();
        let mut start = 0usize;
        while start < audio.len() {
            let end = (start + win).min(audio.len());
            self.decode_window_topk(&audio[start..end], 10, k, &mut all)?;
            start += win;
        }
        let ids: Vec<usize> = all.iter().map(|x| x.0).collect();
        let text = detok(&ids, &self.pieces);
        let rows: Vec<Vec<String>> = all
            .iter()
            .map(|(_, alts)| {
                alts.iter()
                    .map(|(id, _)| self.pieces.get(*id).cloned().unwrap_or_default())
                    .collect()
            })
            .collect();
        Ok((text, rows))
    }
}
