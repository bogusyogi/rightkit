//! CoreML decoder/joint graph loader for validated production assets and
//! legacy evaluator probes.
//!
//! Production loads a hash-validated sibling graph from the TDT model archive;
//! the legacy evaluator path remains available when `RIGHTKIT_COREML_FUSED_PROBE_DIR`
//! names a compiled `.mlmodelc`, source `.mlpackage`, or directory. The graph
//! is expected to accept independent rows with these logical shapes:
//!
//! * token ids: `[B, 1]`, `i32`
//! * recurrent state: `[2, B, 640]`, `f16`
//! * encoder frame: `[B, 1024]`, `f16`
//!
//! It returns predictor projection, recurrent `h`/`c`, and joint logits. The
//! loader accepts common exporter aliases so the probe can consume a graph
//! without changing the production stage names.

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use objc2::rc::autoreleasepool;

use crate::coreml::{read_f16, read_f32, Stage};
use objc2_core_ml::MLMultiArrayDataType;

use super::model::{model_dir_sha256, CoreMlParakeet};

const FUSED_PROBE_ENV: &str = "RIGHTKIT_COREML_FUSED_PROBE_DIR";
const PRODUCTION_GRAPH: &str = "fused_predictor_joint.mlmodelc";
const PRODUCTION_GRAPH_PACKAGE: &str = "fused_predictor_joint.mlpackage";
const PRODUCTION_MANIFEST: &str = "fused_predictor_joint.manifest.json";
const PRODUCTION_SCHEMA: &str = "heardright.coreml.fused-model.v1";

const GRAPH_NAMES: &[&str] = &[
    "FusedDecoderJoint.mlmodelc",
    "FusedDecoderJoint.mlpackage",
    "DecoderJoint.mlmodelc",
    "DecoderJoint.mlpackage",
    "PredictorJoint.mlmodelc",
    "PredictorJoint.mlpackage",
    "fused_decoder_joint.mlmodelc",
    "fused_decoder_joint.mlpackage",
    "decoder_joint.mlmodelc",
    "decoder_joint.mlpackage",
];

const TOKEN_INPUTS: &[&str] = &[
    "decoder_input_ids",
    "input_ids",
    "targets",
    "last_token",
    "last_tok",
    "token",
    "tokens",
];
const H_INPUTS: &[&str] = &["state_h", "input_states_1", "h", "hidden_state", "input_h"];
const C_INPUTS: &[&str] = &["state_c", "input_states_2", "c", "cell_state", "input_c"];
const FRAME_INPUTS: &[&str] = &[
    "encoder_step",
    "encoder_outputs",
    "encoder_frame",
    "encoder",
    "frame",
];

const PROJECTION_OUTPUTS: &[&str] = &[
    "projection",
    "predictor_g",
    "predictor",
    "decoder_step",
    "decoder_output_projected",
    "g",
    "joint_input",
    "projected",
];
const H_OUTPUTS: &[&str] = &[
    "state_h_out",
    "output_states_1",
    "new_state_1",
    "new_h",
    "h_out",
    "hidden_state_out",
];
const C_OUTPUTS: &[&str] = &[
    "state_c_out",
    "output_states_2",
    "new_state_2",
    "new_c",
    "c_out",
    "cell_state_out",
];
const LOGIT_OUTPUTS: &[&str] = &["logits", "joint_logits", "outputs", "output"];

#[derive(Clone, Copy, Debug)]
pub(crate) struct FusedProbeRow<'a> {
    pub(crate) last_tok: i32,
    pub(crate) h: &'a [f32],
    pub(crate) c: &'a [f32],
    pub(crate) enc_step: &'a [f32],
}

#[derive(Clone, Debug)]
pub(crate) struct FusedProbeOutput {
    pub(crate) g: Vec<f32>,
    pub(crate) h: Vec<f32>,
    pub(crate) c: Vec<f32>,
    pub(crate) logits: Vec<f32>,
}

pub(crate) struct FusedProbe {
    pub(super) stage: Stage,
    token_input: String,
    h_input: String,
    c_input: String,
    frame_input: String,
    g_output: String,
    h_output: String,
    c_output: String,
    logits_output: String,
}

#[derive(Debug, serde::Deserialize)]
struct ProductionManifest {
    schema: String,
    graph: String,
    graph_sha256: String,
    pipeline_sha256: String,
    vocab_sha256: String,
    vocab_tokens: usize,
    blank_id: i32,
    tdt_durations: Vec<i64>,
    is_tdt: bool,
    #[serde(default)]
    split_files: Vec<ManifestFile>,
    dimensions: ManifestDimensions,
    beam_width: usize,
    max_rows: usize,
    joint_output: String,
    joint_norm_precision: String,
}

#[derive(Debug, serde::Deserialize)]
struct ManifestFile {
    name: String,
    sha256: String,
}

#[derive(Debug, serde::Deserialize)]
struct ManifestDimensions {
    pred_layers: usize,
    pred_hidden: usize,
    enc_d_model: usize,
    vocab: usize,
}

impl FusedProbe {
    /// Load the canonical graph installed beside the split macOS TDT model.
    /// Any validation failure is returned to the caller, which deliberately
    /// keeps the split decoder available as a safe fallback.
    ///
    /// Gated on capability, not on the directory name (ORT-02 / ENG-15): a payload without the
    /// graph files returns `None` below exactly as before, and a payload that ships them (v3
    /// today; an Ultra export once qualified) gets the same hash validation.
    /// `RIGHTKIT_COREML_FUSED=off` (test-mode) forces the split decoder for an A/B; `auto` is default.
    pub(crate) fn load_production(dir: &Path) -> Result<Option<Self>, String> {
        if rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_FUSED").as_deref() == Ok("off") {
            tracing::info!(model_dir = %dir.display(), "production fused graph disabled by RIGHTKIT_COREML_FUSED=off");
            return Ok(None);
        }
        let manifest_path = dir.join(PRODUCTION_MANIFEST);
        let graph_path = dir.join(PRODUCTION_GRAPH);
        let graph_package_path = dir.join(PRODUCTION_GRAPH_PACKAGE);
        if !manifest_path.is_file() && !graph_path.exists() && !graph_package_path.exists() {
            return Ok(None);
        }
        let raw = std::fs::read(&manifest_path)
            .map_err(|e| format!("read {}: {e}", manifest_path.display()))?;
        let manifest: ProductionManifest = serde_json::from_slice(&raw)
            .map_err(|e| format!("parse {}: {e}", manifest_path.display()))?;
        validate_production_manifest(dir, &manifest)?;
        let graph = dir.join(&manifest.graph);
        let mut probe = Self::load_graph(&graph, None)?
            .ok_or_else(|| format!("production fused graph missing: {}", graph.display()))?;
        probe.stage.enable_output_backings();
        tracing::info!(graph = %graph.display(), "validated production CoreML fused decoder loaded");
        Ok(Some(probe))
    }

    pub(crate) fn load_from_env() -> Result<Option<Self>, String> {
        let Some(raw) = rightkit_dictation_core::dev_var_os!(FUSED_PROBE_ENV) else {
            return Ok(None);
        };
        let requested = PathBuf::from(raw);
        let graph = resolve_graph_path(&requested)?;
        let profile = rightkit_dictation_core::dev_var!(FUSED_PROBE_PROFILE_ENV)
            .ok()
            .filter(|value| matches!(value.as_str(), "cpu_only" | "cpu_gpu" | "neural_engine"))
            .unwrap_or_else(crate::settings::asr_decoder_compute);
        Self::load_graph(&graph, Some(profile.as_str()))
    }

    fn load_graph(graph: &Path, profile: Option<&str>) -> Result<Option<Self>, String> {
        let profile = profile
            .map(str::to_string)
            .unwrap_or_else(crate::settings::asr_decoder_compute);
        let stage = Stage::load_with_compute_profile(graph, &profile)?;
        let inputs = stage.input_names();
        let outputs = stage.output_names();
        let token_input = choose_name(&inputs, TOKEN_INPUTS, "token input")?;
        let h_input = choose_name(&inputs, H_INPUTS, "state_h input")?;
        let c_input = choose_name(&inputs, C_INPUTS, "state_c input")?;
        let frame_input = choose_name(&inputs, FRAME_INPUTS, "encoder frame input")?;
        let g_output = choose_name(&outputs, PROJECTION_OUTPUTS, "predictor projection output")?;
        let h_output = choose_name(&outputs, H_OUTPUTS, "state_h output")?;
        let c_output = choose_name(&outputs, C_OUTPUTS, "state_c output")?;
        let logits_output = choose_name(&outputs, LOGIT_OUTPUTS, "logits output")?;
        tracing::info!(
            graph = %graph.display(),
            compute = stage.compute_profile(),
            token_input,
            h_input,
            c_input,
            frame_input,
            g_output,
            h_output,
            c_output,
            logits_output,
            "CoreML fused probe loaded"
        );
        Ok(Some(Self {
            stage,
            token_input,
            h_input,
            c_input,
            frame_input,
            g_output,
            h_output,
            c_output,
            logits_output,
        }))
    }

    fn predict_rows(
        &self,
        rows: &[FusedProbeRow<'_>],
        pred_layers: usize,
        pred_hidden: usize,
        enc_d: usize,
    ) -> Result<Vec<FusedProbeOutput>, String> {
        if rows.is_empty() {
            return Err("fused probe requires at least one row".into());
        }
        let batch = rows.len();
        if pred_layers == 0 || pred_hidden == 0 || enc_d == 0 {
            return Err("fused probe dimensions must be nonzero".into());
        }
        let state_len = pred_layers
            .checked_mul(pred_hidden)
            .ok_or("fused probe state dimensions overflow")?;
        let mut tokens = Vec::with_capacity(batch);
        let mut h_data = Vec::with_capacity(batch * state_len);
        let mut c_data = Vec::with_capacity(batch * state_len);
        let mut enc_data = Vec::with_capacity(batch * enc_d);
        for row in rows {
            if row.h.len() != state_len {
                return Err(format!(
                    "fused probe h row has {}, expected {}",
                    row.h.len(),
                    state_len
                ));
            }
            if row.c.len() != state_len {
                return Err(format!(
                    "fused probe c row has {}, expected {}",
                    row.c.len(),
                    state_len
                ));
            }
            if row.enc_step.len() != enc_d {
                return Err(format!(
                    "fused probe encoder row has {}, expected {}",
                    row.enc_step.len(),
                    enc_d
                ));
            }
            tokens.push(row.last_tok);
            enc_data.extend_from_slice(row.enc_step);
        }
        // CoreML's recurrent state contract follows ONNX: layer-major, then
        // row, then hidden. Production BeamHyp stores each row as [2,1,640],
        // so transpose each row into [2,B,640] here.
        for layer in 0..pred_layers {
            for row in rows {
                h_data.extend_from_slice(&row.h[layer * pred_hidden..(layer + 1) * pred_hidden]);
            }
        }
        for layer in 0..pred_layers {
            for row in rows {
                c_data.extend_from_slice(&row.c[layer * pred_hidden..(layer + 1) * pred_hidden]);
            }
        }
        let outputs = autoreleasepool(|_| -> Result<Vec<_>, String> {
            let token_array = super::input_reuse_probe::i32("fused.token", &[batch, 1], &tokens)?;
            let h_array = super::input_reuse_probe::f16(
                "fused.h",
                &[pred_layers, batch, pred_hidden],
                &h_data,
            )?;
            let c_array = super::input_reuse_probe::f16(
                "fused.c",
                &[pred_layers, batch, pred_hidden],
                &c_data,
            )?;
            let encoder_array =
                super::input_reuse_probe::f16("fused.encoder", &[batch, enc_d], &enc_data)?;
            let out = self.stage.predict_multi_shaped(
                &[
                    (&self.token_input, &token_array),
                    (&self.h_input, &h_array),
                    (&self.c_input, &c_array),
                    (&self.frame_input, &encoder_array),
                ],
                &[
                    &self.g_output,
                    &self.h_output,
                    &self.c_output,
                    &self.logits_output,
                ],
                &[
                    &[batch, pred_hidden],
                    &[pred_layers, batch, pred_hidden],
                    &[pred_layers, batch, pred_hidden],
                    &[batch, 8198],
                ],
            )?;
            out.into_iter()
                .map(|array| {
                    let values = unsafe {
                        match array.dataType() {
                            MLMultiArrayDataType::Float16 => read_f16(&array),
                            MLMultiArrayDataType::Float32 => read_f32(&array),
                            _ => return Err("fused probe output must be FP16 or FP32".into()),
                        }
                    };
                    if values.iter().any(|value| !value.is_finite()) {
                        return Err("fused probe output is nonfinite".into());
                    }
                    Ok(values)
                })
                .collect()
        })?;
        if outputs.len() != 4 {
            return Err(format!(
                "fused probe returned {} outputs, expected 4",
                outputs.len()
            ));
        }
        let g_width = outputs[0].len();
        if g_width != batch * pred_hidden {
            return Err(format!(
                "fused probe projection has {}, expected {}",
                g_width,
                batch * pred_hidden
            ));
        }
        let expected_state = pred_layers * batch * pred_hidden;
        for (name, values) in [("h", &outputs[1]), ("c", &outputs[2])] {
            if values.len() != expected_state {
                return Err(format!(
                    "fused probe {name} output has {}, expected {}",
                    values.len(),
                    expected_state
                ));
            }
        }
        if outputs[3].is_empty() || outputs[3].len() % batch != 0 {
            return Err(format!(
                "fused probe logits length {} is not divisible by batch {}",
                outputs[3].len(),
                batch
            ));
        }
        let logits_width = outputs[3].len() / batch;
        let mut rows_out = Vec::with_capacity(batch);
        for row in 0..batch {
            let mut h = Vec::with_capacity(state_len);
            let mut c = Vec::with_capacity(state_len);
            for layer in 0..pred_layers {
                let offset = (layer * batch + row) * pred_hidden;
                h.extend_from_slice(&outputs[1][offset..offset + pred_hidden]);
                c.extend_from_slice(&outputs[2][offset..offset + pred_hidden]);
            }
            rows_out.push(FusedProbeOutput {
                g: outputs[0][row * pred_hidden..(row + 1) * pred_hidden].to_vec(),
                h,
                c,
                logits: outputs[3][row * logits_width..(row + 1) * logits_width].to_vec(),
            });
        }
        Ok(rows_out)
    }

    pub(crate) fn predict_row(
        &self,
        row: FusedProbeRow<'_>,
        pred_layers: usize,
        pred_hidden: usize,
        enc_d: usize,
    ) -> Result<FusedProbeOutput, String> {
        self.predict_rows(&[row], pred_layers, pred_hidden, enc_d)
            .map(|mut rows| rows.remove(0))
    }
}

fn validate_production_manifest(dir: &Path, manifest: &ProductionManifest) -> Result<(), String> {
    if manifest.schema != PRODUCTION_SCHEMA {
        return Err(format!(
            "unsupported fused manifest schema {:?}",
            manifest.schema
        ));
    }
    if !is_production_graph_name(&manifest.graph)
        || !is_plain_name(&manifest.graph)
        || manifest.beam_width != 2
        || manifest.max_rows != 2
        || manifest.joint_output != "raw_linear"
        || manifest.joint_norm_precision != "host_fp32"
    {
        return Err("fused manifest is not the accepted raw B2 production candidate".into());
    }
    if manifest.dimensions.pred_layers != 2
        || manifest.dimensions.pred_hidden != 640
        || manifest.dimensions.enc_d_model != 1024
        || manifest.dimensions.vocab != 8198
        || manifest.vocab_tokens != 8192
        || manifest.blank_id != 8192
        || manifest.tdt_durations != [0, 1, 2, 3, 4]
        || !manifest.is_tdt
    {
        return Err("fused manifest dimensions are incompatible with Parakeet TDT v3".into());
    }
    for name in ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"] {
        if !dir.join(format!("{name}.mlmodelc")).is_dir()
            || dir.join(format!("{name}.mlpackage")).exists()
        {
            return Err(format!(
                "production split stage must be compiled-only: {name}"
            ));
        }
    }
    let pipeline = dir.join("pipeline.json");
    let vocab = dir.join("vocab.txt");
    if sha256_path(&pipeline)? != manifest.pipeline_sha256
        || sha256_path(&vocab)? != manifest.vocab_sha256
    {
        return Err("fused manifest split-model identity does not match installed files".into());
    }
    let pipeline_raw = std::fs::read_to_string(&pipeline)
        .map_err(|error| format!("read {}: {error}", pipeline.display()))?;
    let pipeline_json: serde_json::Value = serde_json::from_str(&pipeline_raw)
        .map_err(|error| format!("parse {}: {error}", pipeline.display()))?;
    let actual_pred_layers = pipeline_json["pred_layers"]
        .as_u64()
        .ok_or("pipeline.json missing pred_layers")? as usize;
    let actual_pred_hidden = pipeline_json["pred_hidden"]
        .as_u64()
        .ok_or("pipeline.json missing pred_hidden")? as usize;
    let actual_enc_d_model = pipeline_json["enc_d_model"]
        .as_u64()
        .ok_or("pipeline.json missing enc_d_model")? as usize;
    let actual_blank_id = pipeline_json["blank_id"]
        .as_i64()
        .ok_or("pipeline.json missing blank_id")? as i32;
    let actual_is_tdt = pipeline_json["is_tdt"]
        .as_bool()
        .ok_or("pipeline.json missing is_tdt")?;
    let actual_durations = pipeline_json["tdt_durations"]
        .as_array()
        .ok_or("pipeline.json missing tdt_durations")?
        .iter()
        .map(|value| {
            value
                .as_i64()
                .ok_or("pipeline.json has invalid tdt duration")
        })
        .collect::<Result<Vec<_>, _>>()?;
    let actual_vocab_tokens = std::fs::read_to_string(&vocab)
        .map_err(|error| format!("read {}: {error}", vocab.display()))?
        .lines()
        .count();
    if actual_pred_layers != manifest.dimensions.pred_layers
        || actual_pred_hidden != manifest.dimensions.pred_hidden
        || actual_enc_d_model != manifest.dimensions.enc_d_model
        || actual_blank_id != manifest.blank_id
        || !actual_is_tdt
        || actual_durations != manifest.tdt_durations
        || actual_vocab_tokens != manifest.vocab_tokens
    {
        return Err("fused manifest dimensions do not match installed split metadata".into());
    }
    if manifest.split_files.is_empty() {
        return Err("fused manifest has no split-model file inventory".into());
    }
    let inventory = manifest
        .split_files
        .iter()
        .map(|file| file.name.clone())
        .collect::<BTreeSet<_>>();
    if inventory.len() != manifest.split_files.len() {
        return Err("fused manifest split inventory contains duplicate paths".into());
    }
    let graph = dir.join(&manifest.graph);
    let mut actual = Vec::new();
    collect_relative_files(dir, &mut actual)?;
    let actual = actual
        .into_iter()
        .filter(|path| {
            !path.starts_with(&graph)
                && path.file_name().and_then(|name| name.to_str()) != Some(PRODUCTION_MANIFEST)
        })
        .map(|path| {
            path.strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect::<BTreeSet<_>>();
    if inventory != actual {
        return Err("fused manifest split-model file inventory is incomplete or stale".into());
    }
    for file in &manifest.split_files {
        if !is_safe_relative(&file.name) {
            return Err(format!(
                "fused manifest split file {:?} is unsafe",
                file.name
            ));
        }
        if file.name == PRODUCTION_MANIFEST
            || file.name == PRODUCTION_GRAPH
            || file.name == PRODUCTION_GRAPH_PACKAGE
        {
            return Err(format!(
                "fused manifest split inventory contains production asset {:?}",
                file.name
            ));
        }
        let path = dir.join(&file.name);
        if sha256_path(&path)? != file.sha256 {
            return Err(format!("fused manifest hash mismatch for {}", file.name));
        }
    }
    let got_graph = model_dir_sha256(&graph)?;
    if !got_graph.eq_ignore_ascii_case(&manifest.graph_sha256) {
        return Err("fused graph hash does not match manifest".into());
    }
    Ok(())
}

fn collect_relative_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in std::fs::read_dir(dir).map_err(|error| format!("read model dir: {error}"))? {
        let path = entry
            .map_err(|error| format!("read model entry: {error}"))?
            .path();
        if path.is_dir() {
            collect_relative_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn is_production_graph_name(value: &str) -> bool {
    value == PRODUCTION_GRAPH
}

fn is_plain_name(value: &str) -> bool {
    let mut components = Path::new(value).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

fn is_safe_relative(value: &str) -> bool {
    let mut saw = false;
    for component in Path::new(value).components() {
        match component {
            Component::Normal(_) => saw = true,
            Component::CurDir => {}
            _ => return false,
        }
    }
    saw
}

fn sha256_path(path: &Path) -> Result<String, String> {
    if path.is_dir() {
        return model_dir_sha256(path);
    }
    rightkit_fs::digest::sha256_file(path).map_err(|e| format!("read {}: {e}", path.display()))
}

fn choose_name(available: &[String], aliases: &[&str], role: &str) -> Result<String, String> {
    if let Some(name) = aliases
        .iter()
        .find_map(|alias| available.iter().find(|name| name == alias))
    {
        return Ok(name.clone());
    }
    let normalised = |name: &str| name.to_ascii_lowercase().replace(['_', '-', '.'], "");
    available
        .iter()
        .find(|name| {
            let candidate = normalised(name);
            aliases.iter().any(|alias| candidate == normalised(alias))
        })
        .cloned()
        .ok_or_else(|| format!("fused probe has no {role}; available={available:?}"))
}

fn resolve_graph_path(requested: &Path) -> Result<PathBuf, String> {
    if requested.is_file()
        || requested
            .extension()
            .is_some_and(|ext| ext == "mlmodelc" || ext == "mlpackage")
    {
        if requested.exists() {
            return Ok(requested.to_path_buf());
        }
        return Err(format!(
            "{FUSED_PROBE_ENV} graph does not exist: {}",
            requested.display()
        ));
    }
    if requested.is_dir() {
        for name in GRAPH_NAMES {
            let candidate = requested.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
        let mut candidates = std::fs::read_dir(requested)
            .map_err(|error| format!("read {FUSED_PROBE_ENV} directory: {error}"))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_dir()
                    && matches!(
                        path.extension().and_then(|ext| ext.to_str()),
                        Some("mlmodelc") | Some("mlpackage")
                    )
            })
            .collect::<Vec<_>>();
        candidates.sort();
        if candidates.len() == 1 {
            return Ok(candidates.remove(0));
        }
        if candidates.is_empty() {
            return Err(format!(
                "{FUSED_PROBE_ENV} directory has no CoreML graph: {}",
                requested.display()
            ));
        }
        return Err(format!(
            "{FUSED_PROBE_ENV} directory has multiple CoreML graphs; name one explicitly: {}",
            requested.display()
        ));
    }
    Err(format!(
        "{FUSED_PROBE_ENV} path does not exist: {}",
        requested.display()
    ))
}

impl CoreMlParakeet {
    pub fn fused_probe_compute_profile(&self) -> Option<&str> {
        self.fused_probe
            .as_ref()
            .map(|probe| probe.stage.compute_profile())
    }

    pub(crate) fn fused_probe_loaded(&self) -> bool {
        self.fused_probe.is_some()
    }

    pub(crate) fn fused_probe_enabled(&self) -> bool {
        self.fused_probe_loaded() && self.evaluator_fused_enabled.unwrap_or(true)
    }

    pub(crate) fn fused_probe_step(
        &self,
        row: FusedProbeRow<'_>,
    ) -> Result<FusedProbeOutput, String> {
        let probe = self
            .fused_probe
            .as_ref()
            .ok_or("CoreML fused probe is not loaded")?;
        probe.predict_row(row, self.meta.pred_layers, self.meta.pred_hidden, 1024)
    }

    pub(crate) fn fused_probe_steps(
        &self,
        rows: &[FusedProbeRow<'_>],
    ) -> Result<Vec<FusedProbeOutput>, String> {
        let probe = self
            .fused_probe
            .as_ref()
            .ok_or("CoreML fused probe is not loaded")?;
        probe.predict_rows(rows, self.meta.pred_layers, self.meta.pred_hidden, 1024)
    }

    /// Run evaluator-owned fused graph sanity using independent synthetic
    /// tokens, recurrent states, and encoder frames. Each result row is:
    /// `[batch, split_vs_fused_g, split_vs_fused_h, split_vs_fused_c,
    /// split_vs_fused_logits, batch_vs_scalar_g, batch_vs_scalar_h,
    /// batch_vs_scalar_c, batch_vs_scalar_logits]`.
    ///
    /// Production loads the validated sibling graph; evaluator loads may use
    /// `RIGHTKIT_COREML_FUSED_PROBE_DIR`. The optional
    /// `RIGHTKIT_COREML_FUSED_PROBE_MAX_ROWS` cap selects available counts from
    /// `[1, 2, 4, 8]`; this lets a static batch-one export run count one.
    /// Numerical differences are reported as maxima, while shape/nonfinite
    /// failures remain hard errors.
    pub fn fused_probe_sanity(&self) -> Result<Option<Vec<[f32; 9]>>, String> {
        if !self.fused_probe_loaded() {
            return Ok(None);
        }
        let max_rows = self.fused_max_rows;
        let counts = [1usize, 2, 4, 8]
            .into_iter()
            .filter(|count| *count <= max_rows)
            .collect::<Vec<_>>();

        let pl = self.meta.pred_layers;
        let ph = self.meta.pred_hidden;
        let rows = (0..8usize)
            .map(|i| {
                let h = (0..pl * ph)
                    .map(|j| ((j + i) % 11) as f32 * 0.001)
                    .collect::<Vec<_>>();
                let c = (0..pl * ph)
                    .map(|j| ((j + i * 3) % 13) as f32 * 0.001)
                    .collect::<Vec<_>>();
                let enc_step = (0..1024usize)
                    .map(|j| ((j + i * 7) % 17) as f32 * 0.01)
                    .collect::<Vec<_>>();
                (
                    if i == 0 { self.meta.blank_id } else { i as i32 },
                    h,
                    c,
                    enc_step,
                )
            })
            .collect::<Vec<_>>();
        let probe_rows = rows
            .iter()
            .map(|(last_tok, h, c, enc_step)| FusedProbeRow {
                last_tok: *last_tok,
                h,
                c,
                enc_step,
            })
            .collect::<Vec<_>>();

        let mut result = Vec::with_capacity(counts.len());
        for count in counts {
            let selected = &probe_rows[..count];
            let fused_batch = self.fused_probe_steps(selected)?;
            if fused_batch.len() != count {
                return Err(format!(
                    "fused probe sanity returned {} rows for batch {}",
                    fused_batch.len(),
                    count
                ));
            }
            let mut maxima = [0.0f32; 8];
            for (row, batch_output) in selected.iter().zip(&fused_batch) {
                let fused_scalar = self.fused_probe_step(*row)?;
                let (split_g, split_h, split_c, split_logits) = self.predict_decoder_joint(
                    row.last_tok,
                    row.h,
                    row.c,
                    row.enc_step,
                    pl,
                    ph,
                    1024,
                )?;
                maxima[0] = maxima[0].max(max_abs(&split_g, &fused_scalar.g)?);
                maxima[1] = maxima[1].max(max_abs(&split_h, &fused_scalar.h)?);
                maxima[2] = maxima[2].max(max_abs(&split_c, &fused_scalar.c)?);
                maxima[3] = maxima[3].max(max_abs(&split_logits, &fused_scalar.logits)?);
                maxima[4] = maxima[4].max(max_abs(&fused_scalar.g, &batch_output.g)?);
                maxima[5] = maxima[5].max(max_abs(&fused_scalar.h, &batch_output.h)?);
                maxima[6] = maxima[6].max(max_abs(&fused_scalar.c, &batch_output.c)?);
                maxima[7] = maxima[7].max(max_abs(&fused_scalar.logits, &batch_output.logits)?);
            }
            result.push([
                count as f32,
                maxima[0],
                maxima[1],
                maxima[2],
                maxima[3],
                maxima[4],
                maxima[5],
                maxima[6],
                maxima[7],
            ]);
        }
        Ok(Some(result))
    }
}

fn max_abs(left: &[f32], right: &[f32]) -> Result<f32, String> {
    if left.len() != right.len() {
        return Err(format!(
            "fused probe sanity shape mismatch: {} vs {}",
            left.len(),
            right.len()
        ));
    }
    if left.iter().chain(right).any(|value| !value.is_finite()) {
        return Err("fused probe sanity found nonfinite output".into());
    }
    Ok(left
        .iter()
        .zip(right)
        .map(|(left, right)| (left - right).abs())
        .fold(0.0, f32::max))
}
