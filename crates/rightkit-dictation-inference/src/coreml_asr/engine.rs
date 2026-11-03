// `CoreMlParakeet` engine impl: bundle load, decode-driver entry points, and the
// shared decoder+joint CoreML call. The struct and its identity/fingerprint
// helpers live in `model.rs`; the beam loop itself is in `decode_window.rs`.
use std::path::Path;

use objc2::rc::autoreleasepool;

use crate::coreml::{read_f16, Stage};

use super::detok::detok;
use super::display::{
    assemble_display_pieces, assemble_display_pieces_with_policy, group_timed_words,
};
use super::frontend_lookahead_probe::{
    native_memory_gate, EncodedWindow, FrontendLookaheadConfig, FrontendLookaheadProbe,
    FrontendLookaheadStats, MemoryGate,
};
use super::fused_probe::FusedProbe;
use super::input_reuse_probe;
use super::model::{
    model_dir_sha256, stage_path, CoreMlParakeet, PipelineMeta, ProductionActivation, TimedPiece,
    TimedTok,
};

/// The decode beam width: `2`. Width 2 keeps one alternative hypothesis alive so
/// multi-token context bias can recover names that greedy decoding drops.
const DEFAULT_BEAM_WIDTH: usize = 2;

type DecoderJointOutputs = (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>);
type PredictorOutputs = (Vec<f32>, Vec<f32>, Vec<f32>);
impl CoreMlParakeet {
    /// Load the compiled `.mlmodelc` Parakeet bundle shared by Orthic Labs apps.
    pub fn load(dir: &Path) -> Result<Self, String> {
        input_reuse_probe::clear_evaluator_override();
        crate::coreml::clear_evaluator_output_backings();
        let meta = PipelineMeta::from_dir(dir)?;
        let global_compute = crate::settings::asr_compute_profile();
        let encoder_setting = crate::settings::asr_encoder_compute();
        let decoder_setting = crate::settings::asr_decoder_compute();
        let encoder_profile = if encoder_setting == "inherit" {
            global_compute.as_str()
        } else {
            encoder_setting.as_str()
        };
        let decoder_profile = if decoder_setting == "inherit" {
            global_compute.as_str()
        } else {
            decoder_setting.as_str()
        };
        let mut mel =
            Stage::load_with_compute_profile(&stage_path(dir, "MelSpectrogram"), encoder_profile)?;
        let mut enc =
            Stage::load_with_compute_profile(&stage_path(dir, "AudioEncoder"), encoder_profile)?;
        let mut dec =
            Stage::load_with_compute_profile(&stage_path(dir, "TextDecoder"), decoder_profile)?;
        let mut joint =
            Stage::load_with_compute_profile(&stage_path(dir, "Joint"), decoder_profile)?;
        let encoder_compute = enc.compute_profile().to_string();
        let decoder_compute = dec.compute_profile().to_string();
        let coreml_compile_cache = [
            mel.compile_cache(),
            enc.compile_cache(),
            dec.compile_cache(),
            joint.compile_cache(),
        ]
        .into_iter()
        .find(|status| *status == "fresh_compile")
        .or_else(|| {
            [
                mel.compile_cache(),
                enc.compile_cache(),
                dec.compile_cache(),
                joint.compile_cache(),
            ]
            .into_iter()
            .find(|status| *status == "cache_hit")
        })
        .unwrap_or("precompiled_bundle")
        .to_string();

        let mel_out = mel
            .output_names()
            .into_iter()
            .next()
            .ok_or("MelSpectrogram has no output")?;
        let enc_out = enc
            .output_names()
            .into_iter()
            .next()
            .ok_or("AudioEncoder has no output")?;
        let joint_out = joint
            .output_names()
            .into_iter()
            .next()
            .ok_or("Joint has no output")?;
        // Decoder: g = [1,1,pred_hidden]; the two LSTM states = [pred_layers,1,pred_hidden].
        let g_name = dec.output_by_shape(&[meta.decoder_declared_batch(), 1, meta.pred_hidden])?;
        let states = dec.outputs_by_shape(&[
            meta.pred_layers,
            meta.decoder_declared_batch(),
            meta.pred_hidden,
        ]);
        if states.len() < 2 {
            return Err(format!(
                "TextDecoder: expected 2 state outputs of shape [{},1,{}], found {}",
                meta.pred_layers,
                meta.pred_hidden,
                states.len()
            ));
        }
        let state_h_out = states[0].clone();
        let state_c_out = states[1].clone();

        let vocab = std::fs::read_to_string(dir.join("vocab.txt"))
            .map_err(|e| format!("read vocab.txt: {e}"))?;
        let pieces: Vec<String> = vocab
            .lines()
            .map(|ln| ln.split('\t').next().unwrap_or("").to_string())
            .collect();

        let win_samples = (meta.window_sec * 16_000.0) as usize;
        let beam_width = DEFAULT_BEAM_WIDTH;
        eprintln!("asr_beam_width width={beam_width} source=default");
        tracing::info!(
            encoder_compute,
            decoder_compute,
            "CoreML Parakeet compute routing"
        );
        let model_fingerprint = match rightkit_asr::parakeet_coreml::sampled_model_fingerprint(dir)
        {
            Ok(sampled_fingerprint) => {
                tracing::info!(
                model_dir = %dir.display(),
                sampled_fingerprint = sampled_fingerprint,
                window_sec = meta.window_sec,
                mel_frames = meta.mel_frames,
                enc_frames = meta.enc_frames,
                "CoreML Parakeet model fingerprint"
                );
                sampled_fingerprint
            }
            Err(error) => {
                tracing::warn!(
                    model_dir = %dir.display(),
                    error = %error,
                    "CoreML Parakeet model fingerprint unavailable"
                );
                "unavailable".to_string()
            }
        };
        let model_dir_sha256 = if crate::settings::asr_detailed_telemetry()
            || crate::settings::onboarding_calibration_active()
        {
            match model_dir_sha256(dir) {
                Ok(hash) => Some(hash),
                Err(error) => {
                    tracing::warn!(model_dir = %dir.display(), error, "CoreML model SHA-256 unavailable");
                    None
                }
            }
        } else {
            None
        };
        // The validated graph is an optional sibling of the split model. A
        // missing or incompatible graph keeps the split decoder working.
        let fused_probe = match FusedProbe::load_production(dir) {
            Ok(value) => value,
            Err(error) => {
                tracing::warn!(model_dir = %dir.display(), error, "production fused graph rejected; using split decoder");
                None
            }
        };
        let production_fused = fused_probe.is_some();
        if production_fused {
            mel.enable_output_backings();
            enc.enable_output_backings();
            dec.enable_output_backings();
            joint.enable_output_backings();
        }
        let mut model = Self {
            bundle_dir: dir.to_path_buf(),
            meta,
            mel,
            enc,
            dec,
            joint,
            mel_out,
            enc_out,
            g_name,
            state_h_out,
            state_c_out,
            joint_out,
            pieces,
            win_samples,
            bias: None,
            beam_width,
            model_fingerprint,
            model_dir_sha256,
            coreml_compile_cache,
            encoder_compute,
            encoder_profile: encoder_profile.to_string(),
            decoder_compute,
            fused_probe,
            evaluator_fused_enabled: None,
            fused_max_rows: if production_fused { 2 } else { 8 },
            frontend_lookahead: None,
            evaluator_frontend_lookahead_enabled: None,
            frontend_lookahead_last_stats: None,
        };
        if model.fused_probe.is_some() {
            model.set_beam_width(2);
            if let Some(gate) = native_memory_gate() {
                match model.prepare_frontend_lookahead_production(gate) {
                    Ok(true) => tracing::info!(
                        physical_memory_bytes = gate.physical_memory_bytes,
                        available_memory_bytes = gate.available_memory_bytes,
                        "CoreML frontend lookahead enabled"
                    ),
                    Ok(false) => tracing::info!("CoreML frontend lookahead not admitted"),
                    Err(error) => tracing::warn!(error, "CoreML frontend lookahead unavailable"),
                }
            } else {
                tracing::info!("CoreML frontend lookahead memory facts unavailable");
            }
        }
        Ok(model)
    }

    /// Set the decode beam width (clamped to `1..=8`). `1` is bit-identical
    /// to the original greedy decode (see `decode_window`); each additional
    /// width point costs one more decoder/joint call per active hypothesis
    /// per step — this is not optimized, by design (see task rationale in
    /// `decode_window.rs`).
    pub fn set_beam_width(&mut self, width: usize) {
        self.beam_width = width.clamp(1, 8);
    }

    /// Configure archived benchmark fusion explicitly after model load. This
    /// keeps evaluator arms on one resident runtime while production remains
    /// driven only by its validated sibling manifest.
    pub fn configure_evaluator_fused_probe(
        &mut self,
        enabled: bool,
        max_rows: usize,
    ) -> Result<(), String> {
        if enabled && self.fused_probe.is_none() {
            return Err(
                "evaluator fused graph requires the validated production fused graph".into(),
            );
        }
        self.evaluator_fused_enabled = Some(enabled);
        self.fused_max_rows = max_rows.clamp(1, 8);
        Ok(())
    }

    pub fn configure_evaluator_input_reuse(&mut self, enabled: bool) {
        input_reuse_probe::set_evaluator_enabled(enabled);
    }

    pub fn configure_evaluator_output_backings(&mut self, enabled: bool) {
        crate::coreml::set_evaluator_output_backings(enabled);
        self.mel.set_output_backings_enabled(enabled);
        self.enc.set_output_backings_enabled(enabled);
        self.dec.set_output_backings_enabled(enabled);
        self.joint.set_output_backings_enabled(enabled);
        if let Some(probe) = self.fused_probe.as_mut() {
            probe.stage.set_output_backings_enabled(enabled);
        }
    }

    /// Configure contextual biasing (dictionary + command terms). Mirrors
    /// `HrTransducer::set_context_bias_phrases`. Returns the count installed.
    /// Install phrase bias, encoding through the shared first-party core so
    /// macOS, Windows and iOS agree on what a vocabulary phrase means.
    pub fn set_context_bias_phrases<I, S>(&mut self, phrases: I, score: f32) -> usize
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let bias = rightkit_asr::parakeet_core::bias::ContextBias::from_phrases(
            &self.pieces,
            phrases,
            score,
        );
        let installed = bias.len();
        self.bias = (!bias.is_empty()).then_some(bias);
        installed
    }

    pub fn clear_context_bias(&mut self) {
        self.bias = None;
    }

    pub fn is_tdt(&self) -> bool {
        self.meta.is_tdt
    }

    /// Configured CoreML compute profiles permitted for these stages. CoreML
    /// does not disclose actual per-inference device placement.
    pub fn configured_compute_route(&self) -> (&str, &str) {
        (&self.encoder_compute, &self.decoder_compute)
    }

    pub fn prepare_frontend_lookahead_evaluator(
        &mut self,
        memory_gate: MemoryGate,
        output_backings: bool,
    ) -> Result<bool, String> {
        if self.frontend_lookahead.is_some() {
            return Ok(true);
        }
        let config = FrontendLookaheadConfig::evaluator(memory_gate, output_backings);
        self.frontend_lookahead = FrontendLookaheadProbe::spawn(
            &self.bundle_dir,
            &self.encoder_profile,
            &self.meta,
            config,
        )?;
        if self.frontend_lookahead.is_some() {
            self.frontend_lookahead_last_stats = None;
        }
        Ok(self.frontend_lookahead.is_some())
    }

    pub fn prepare_frontend_lookahead_production(
        &mut self,
        memory_gate: MemoryGate,
    ) -> Result<bool, String> {
        if self.frontend_lookahead.is_some() {
            return Ok(true);
        }
        let config = FrontendLookaheadConfig::production(memory_gate);
        self.frontend_lookahead = FrontendLookaheadProbe::spawn(
            &self.bundle_dir,
            &self.encoder_profile,
            &self.meta,
            config,
        )?;
        if self.frontend_lookahead.is_some() {
            self.frontend_lookahead_last_stats = None;
        }
        Ok(self.frontend_lookahead.is_some())
    }

    pub fn frontend_lookahead_prepared(&self) -> bool {
        self.frontend_lookahead.is_some()
    }

    pub(crate) fn frontend_lookahead_enabled(&self) -> bool {
        self.frontend_lookahead.is_some()
            && self.evaluator_frontend_lookahead_enabled.unwrap_or(true)
    }

    pub fn configure_evaluator_frontend_lookahead(&mut self, enabled: bool) {
        self.evaluator_frontend_lookahead_enabled = Some(enabled);
    }

    /// Snapshot candidate counters, admission facts, and worker residency for
    /// evaluator receipts. `None` means no worker was retained.
    pub fn frontend_lookahead_stats(&self) -> Option<FrontendLookaheadStats> {
        self.frontend_lookahead
            .as_ref()
            .map(FrontendLookaheadProbe::stats)
            .or(self.frontend_lookahead_last_stats)
    }

    pub fn production_activation(&self) -> ProductionActivation {
        ProductionActivation {
            fused_loaded: self.fused_probe.is_some() && self.evaluator_fused_enabled.is_none(),
            fused_max_rows: self.fused_max_rows,
            all_stage_output_backings: self.mel.output_backings_enabled()
                && self.enc.output_backings_enabled()
                && self.dec.output_backings_enabled()
                && self.joint.output_backings_enabled()
                && self
                    .fused_probe
                    .as_ref()
                    .map(|probe| probe.stage.output_backings_enabled())
                    .unwrap_or(false),
            frontend_lookahead_admitted: self.frontend_lookahead.is_some()
                && self.evaluator_frontend_lookahead_enabled.is_none(),
        }
    }

    pub(crate) fn enqueue_frontend_window(
        &mut self,
        sequence: u64,
        segment: &[f32],
    ) -> Result<(), String> {
        self.frontend_lookahead
            .as_mut()
            .ok_or("frontend lookahead is not prepared")?
            .request(sequence, segment)
    }

    pub(crate) fn take_frontend_window(&mut self, sequence: u64) -> Result<EncodedWindow, String> {
        let wait_started = std::time::Instant::now();
        let encoded = self
            .frontend_lookahead
            .as_mut()
            .ok_or("frontend lookahead is not prepared")?
            .take(sequence)?;
        tracing::info!(
            target: "rightkit_asr_stage_timing",
            sequence,
            wait_ms = wait_started.elapsed().as_secs_f64() * 1_000.0,
            "frontend_lookahead_wait"
        );
        Ok(encoded)
    }

    pub(crate) fn disable_frontend_lookahead(&mut self) {
        if let Some(probe) = self.frontend_lookahead.take() {
            let mut stats = probe.stats();
            drop(probe);
            stats.worker_resident = false;
            self.frontend_lookahead_last_stats = Some(stats);
        }
    }

    pub fn model_fingerprint(&self) -> &str {
        &self.model_fingerprint
    }

    pub fn model_dir_sha256(&self) -> Option<&str> {
        self.model_dir_sha256.as_deref()
    }

    pub fn compile_cache(&self) -> &str {
        &self.coreml_compile_cache
    }

    pub(crate) fn predict_decoder_joint(
        &self,
        last_tok: i32,
        h: &[f32],
        c: &[f32],
        enc_step: &[f32],
        pl: usize,
        ph: usize,
        enc_d: usize,
    ) -> Result<DecoderJointOutputs, String> {
        let (g, new_h, new_c) = self.predict_decoder(last_tok, h, c, pl, ph)?;
        let logits = self.predict_joint(&g, enc_step, ph, enc_d)?;
        Ok((g, new_h, new_c, logits))
    }

    /// Run TextDecoder for one predictor state. Its output depends only on the
    /// last token and LSTM state, so decode_window may retain it across blank
    /// transitions while recomputing Joint for each encoder frame.
    pub(crate) fn predict_decoder(
        &self,
        last_tok: i32,
        h: &[f32],
        c: &[f32],
        pl: usize,
        ph: usize,
    ) -> Result<PredictorOutputs, String> {
        if let Some(n) = self.tensor_batch_rows() {
            return Ok(self.decoder_rows_tensor(n, &[(last_tok, h, c)])?.remove(0));
        }
        autoreleasepool(|_| -> Result<_, String> {
            let ids = input_reuse_probe::i32("decoder_input_ids", &[1, 1], &[last_tok])?;
            let hin = input_reuse_probe::f16("state_h", &[pl, 1, ph], h)?;
            let cin = input_reuse_probe::f16("state_c", &[pl, 1, ph], c)?;
            let outs = self.dec.predict_multi(
                &[
                    ("decoder_input_ids", &ids),
                    ("state_h", &hin),
                    ("state_c", &cin),
                ],
                &[&self.g_name, &self.state_h_out, &self.state_c_out],
            )?;
            Ok((read_f16(&outs[0]), read_f16(&outs[1]), read_f16(&outs[2])))
        })
    }

    /// Run Joint for one encoder frame and cached predictor projection.
    /// Joint output is frame-dependent and must never be reused across frames.
    pub(crate) fn predict_joint(
        &self,
        g: &[f32],
        enc_step: &[f32],
        ph: usize,
        enc_d: usize,
    ) -> Result<Vec<f32>, String> {
        if let Some(n) = self.tensor_batch_rows() {
            return Ok(self.joint_rows_tensor(n, &[(g, enc_step)])?.remove(0));
        }
        let logits = autoreleasepool(|_| -> Result<Vec<f32>, String> {
            let enc_in = input_reuse_probe::f16("encoder_step", &[1, enc_d], enc_step)?;
            let dec_in = input_reuse_probe::f16("decoder_step", &[1, ph], g)?;
            let out = self.joint.predict(
                &[("encoder_step", &enc_in), ("decoder_step", &dec_in)],
                &self.joint_out,
            )?;
            Ok(read_f16(&out))
        })?;
        Ok(logits)
    }

    /// Transcribe 16 kHz mono f32 samples. Windowed, per-window state reset.
    pub fn transcribe(&self, audio: &[f32]) -> Result<String, String> {
        let hits = self.decode_all(audio)?;
        let ids: Vec<usize> = hits.iter().map(|h| h.0).collect();
        Ok(detok(&ids, &self.pieces))
    }

    /// Transcribe with word-level timestamps (offline file path). Same decode as
    /// [`transcribe`](Self::transcribe); additionally groups subword pieces into words on the `▁`
    /// boundary and stamps each with the TDT frame time (80 ms/frame: 160 hop ×
    /// 8 subsample @ 16 kHz, offset by the window start). Returns (full_text, words).
    pub fn transcribe_timed(&self, audio: &[f32]) -> Result<(String, Vec<TimedTok>), String> {
        let hits = self.decode_all(audio)?;
        let ids: Vec<usize> = hits.iter().map(|h| h.0).collect();
        let text = detok(&ids, &self.pieces);

        let words = group_timed_words(&hits, &self.pieces);
        Ok((text, words))
    }

    /// Transcribe to display-token pieces with timestamps. Used by buffered seam
    /// experiments that must match the ONNX `TimedToken` commit behavior.
    pub fn transcribe_pieces_timed(&self, audio: &[f32]) -> Result<Vec<TimedPiece>, String> {
        self.transcribe_pieces_timed_with_policy(
            audio,
            rightkit_asr::parakeet_core::DetokPolicy::canonical_rust(),
        )
    }

    pub fn transcribe_pieces_timed_with_policy(
        &self,
        audio: &[f32],
        policy: rightkit_asr::parakeet_core::DetokPolicy,
    ) -> Result<Vec<TimedPiece>, String> {
        let hits = self.decode_all(audio)?;
        Ok(assemble_display_pieces_with_policy(
            &hits,
            &self.pieces,
            policy,
        ))
    }

    pub(crate) fn transcribe_pieces_timed_with_frontend(
        &self,
        audio: &[f32],
        frontend: EncodedWindow,
    ) -> Result<Vec<TimedPiece>, String> {
        if audio.len() > self.win_samples {
            return Err("frontend lookahead window exceeds model window".into());
        }
        let mut hits = Vec::new();
        self.decode_window_with_frontend(audio, 10, 0.0, &mut hits, Some(frontend))?;
        Ok(assemble_display_pieces(&hits, &self.pieces))
    }

    /// Windowed greedy decode over the whole clip. Returns (piece_id, start_sec)
    /// per emitted token. Shared by [`transcribe`] and [`transcribe_timed`].
    ///
    /// SINGLE WINDOW ONLY. Do not loop this over `ws += win` to cover long
    /// audio: that splits at hard 15 s boundaries with no quiet cut and no
    /// seam de-duplication, landing cuts mid-word. Long audio has exactly one
    /// correct path: `AsrRuntime::transcribe*`, which applies the
    /// padded-window contract in `docs/ASR_DECODE_CONTRACT.md`.
    fn decode_all(&self, audio: &[f32]) -> Result<Vec<(usize, f32, usize)>, String> {
        let max_symbols = 10usize;
        if audio.len() > self.win_samples {
            return Err(format!(
                "audio is {:.1}s but this decode primitive handles at most one {:.1}s window; \
                 use AsrRuntime::transcribe/transcribe_file for long audio (padded-window contract)",
                audio.len() as f32 / 16_000.0,
                self.win_samples as f32 / 16_000.0,
            ));
        }
        let mut hits: Vec<(usize, f32, usize)> = Vec::new();
        self.decode_window(audio, max_symbols, 0.0, &mut hits)?;
        Ok(hits)
    }
}
