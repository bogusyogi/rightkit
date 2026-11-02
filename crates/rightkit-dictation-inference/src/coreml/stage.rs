// Shared native CoreML.framework bridge (Apple Silicon / ANE).
//
// Generic `MLModel`/`MLMultiArray` plumbing reused by every on-device CoreML
// engine — Parakeet TDT (`coreml_asr.rs`) and WhisperKit (`whisper_coreml.rs`).
// Pure Rust on `objc2-core-ml`; no Swift, no subprocess.
//
// THE load-bearing subtlety: CoreML prediction outputs are NOT guaranteed
// C-contiguous — they can carry arbitrary element strides. `read_f16` gathers
// through `MLMultiArray.strides()`; reading `dataPointer` linearly would
// scramble the layout (and min/max/mean wouldn't reveal it). See
// docs/ARCHITECTURE.md.
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::ProtocolObject;
use objc2::AnyThread;
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel,
    MLModelConfiguration, MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{NSDictionary, NSString, NSURL};

use super::multiarray::{ml_f16, ml_f32, ml_i32};

unsafe extern "C" {
    fn rightkit_dictation_coreml_feature_batch(
        model: *mut c_void,
        providers: *mut c_void,
        names: *const *const std::ffi::c_char,
        name_count: usize,
        error: *mut *mut c_void,
        exception: *mut *mut c_void,
    ) -> *mut c_void;
    fn rightkit_dictation_coreml_prediction(
        model: *mut c_void,
        provider: *mut c_void,
        error_out: *mut *mut c_void,
        exception_description_out: *mut *mut c_void,
    ) -> *mut c_void;
    fn rightkit_dictation_coreml_prediction_with_output_backings(
        model: *mut c_void,
        provider: *mut c_void,
        names: *const *const std::ffi::c_char,
        backings: *const *mut c_void,
        count: usize,
        error_out: *mut *mut c_void,
        exception_description_out: *mut *mut c_void,
    ) -> *mut c_void;
    fn rightkit_dictation_coreml_exception_smoke() -> *mut c_void;
    fn rightkit_dictation_coreml_joint_batch(
        model: *mut c_void,
        encoders: *mut *mut c_void,
        decoder: *mut c_void,
        count: usize,
        output_name: *const std::ffi::c_char,
        error_out: *mut *mut c_void,
        exception_description_out: *mut *mut c_void,
    ) -> *mut c_void;
}

/// Process-wide lease for main Core ML ASR predictions.
///
/// CPU KWS owns a separate worker and does not take this lease.
pub(crate) struct CoreMlInferenceLease {
    _lease: crate::inference_gate::InferenceLease,
}

pub(crate) fn inference_lease(owner: &'static str) -> CoreMlInferenceLease {
    let wait_started = Instant::now();
    let lease = crate::inference_gate::lease(owner);
    let wait_ms = wait_started.elapsed().as_millis() as u64;
    if wait_ms > 0 {
        tracing::info!(
            target: "coreml_inference",
            owner,
            wait_ms,
            "coreml_inference_waited"
        );
    }
    CoreMlInferenceLease { _lease: lease }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct OutputBackingProbeCounts {
    pub requested: u64,
    pub honored: u64,
    pub ignored: u64,
    pub failed: u64,
}

static OUTPUT_BACKING_REQUESTED: AtomicU64 = AtomicU64::new(0);
static OUTPUT_BACKING_HONORED: AtomicU64 = AtomicU64::new(0);
static OUTPUT_BACKING_IGNORED: AtomicU64 = AtomicU64::new(0);
static OUTPUT_BACKING_FAILED: AtomicU64 = AtomicU64::new(0);

/// ORT-13: opt-in `MLModelConfiguration` knobs (test-mode switches, default off; both can change
/// numerics or load cost, so neither ships on):
///
/// - `RIGHTKIT_COREML_LOW_PRECISION_GPU_PROBE=1`: `allowLowPrecisionAccumulationOnGPU` (macOS 10.15+;
///   only matters where a stage runs on the GPU, e.g. Intel Macs on `cpuAndGPU`);
/// - `RIGHTKIT_COREML_FAST_PREDICTION_PROBE=1`: `MLOptimizationHints.specializationStrategy =
///   fastPrediction` (macOS 14.4+; ignored, with a log line, on older systems).
///
/// Applies to every stage this process loads, so a probe arm covers the whole pipeline.
fn apply_configuration_probes(config: &MLModelConfiguration) {
    if rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_LOW_PRECISION_GPU_PROBE").as_deref()
        == Ok("1")
    {
        // SAFETY: plain BOOL property setter on a configuration this thread owns.
        unsafe { config.setAllowLowPrecisionAccumulationOnGPU(true) };
        tracing::info!(
            probe = "low_precision_gpu",
            "CoreML configuration probe applied"
        );
    }
    if rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_FAST_PREDICTION_PROBE").as_deref()
        == Ok("1")
    {
        if objc2::available!(macos = 14.4) {
            // SAFETY: the class and both selectors exist from macOS 14.4 (checked above); the
            // hints object is copied by the setter.
            unsafe {
                let hints = objc2_core_ml::MLOptimizationHints::new();
                hints.setSpecializationStrategy(
                    objc2_core_ml::MLSpecializationStrategy::FastPrediction,
                );
                config.setOptimizationHints(&hints);
            }
            tracing::info!(
                probe = "fast_prediction",
                "CoreML configuration probe applied"
            );
        } else {
            tracing::info!(
                probe = "fast_prediction",
                "CoreML configuration probe skipped: needs macOS 14.4"
            );
        }
    }
}

fn output_backing_probe_enabled() -> bool {
    OUTPUT_BACKING_EVALUATOR.with(|override_value| {
        override_value.get().unwrap_or_else(|| {
            rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_OUTPUT_BACKING_PROBE").as_deref()
                == Ok("1")
        })
    })
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum OutputBackingKind {
    Float16,
    Float32,
    Int32,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
struct OutputBackingKey {
    name: String,
    shape: Vec<usize>,
    kind: OutputBackingKind,
}

thread_local! {
    static OUTPUT_BACKING_EVALUATOR: Cell<Option<bool>> = const { Cell::new(None) };
}

pub(crate) fn set_evaluator_output_backings(enabled: bool) {
    OUTPUT_BACKING_EVALUATOR.with(|value| value.set(Some(enabled)));
}

pub(crate) fn clear_evaluator_output_backings() {
    OUTPUT_BACKING_EVALUATOR.with(|value| value.set(None));
}

/// Diagnostic used by the release stress harness to prove an Objective-C
/// exception cannot cross into Rust even with `panic = "abort"`.
pub fn exception_bridge_smoke() -> Result<String, String> {
    let description = unsafe { rightkit_dictation_coreml_exception_smoke() };
    let description = unsafe { Retained::<NSString>::from_raw(description.cast()) }
        .ok_or_else(|| "Objective-C exception smoke returned no description".to_string())?;
    Ok(description.to_string())
}

/// One loaded CoreML model stage on the ANE+CPU.
pub struct Stage {
    model: Retained<MLModel>,
    compute_profile: &'static str,
    compile_cache: &'static str,
    // Owned by stage so reload/drop releases every retained array, even when
    // stage moves between loader & consumer threads. Stage is not Sync.
    output_backings: RefCell<HashMap<OutputBackingKey, Retained<MLMultiArray>>>,
    output_backings_enabled: bool,
}

// SAFETY: Apple documents `MLModel` as thread-safe ("You can safely call this
// method [prediction] from multiple threads" / MLModel instances may be shared
// across threads). `objc2` just doesn't encode that. Owned output arrays are
// accessed synchronously through non-Sync Stage; moving Stage transfers sole
// cache ownership. Needed so the VAD's background loader thread can hand loaded
// model to the consumer thread (vad.rs `start_loading`).
unsafe impl Send for Stage {}

impl Stage {
    /// Independent fixed-shape providers; API batching never shares recurrent state.
    pub fn predict_feature_batch(
        &self,
        inputs: &[Vec<(&str, &Retained<MLMultiArray>)>],
        out_names: &[&str],
    ) -> Result<Vec<Vec<Retained<MLMultiArray>>>, String> {
        if inputs.is_empty() || out_names.is_empty() {
            return Err("empty feature batch".into());
        }
        unsafe {
            let mut providers = Vec::with_capacity(inputs.len());
            for row in inputs {
                let keys: Vec<_> = row.iter().map(|(k, _)| NSString::from_str(k)).collect();
                let values: Vec<_> = row
                    .iter()
                    .map(|(_, v)| MLFeatureValue::featureValueWithMultiArray(v))
                    .collect();
                let key_refs: Vec<&NSString> = keys.iter().map(|v| v.as_ref()).collect();
                let value_refs: Vec<&MLFeatureValue> = values.iter().map(|v| v.as_ref()).collect();
                let dict = NSDictionary::from_slices(&key_refs, &value_refs);
                let provider = MLDictionaryFeatureProvider::initWithDictionary_error(
                    MLDictionaryFeatureProvider::alloc(),
                    std::mem::transmute::<
                        &NSDictionary<NSString, MLFeatureValue>,
                        &NSDictionary<NSString, objc2::runtime::AnyObject>,
                    >(&dict),
                )
                .map_err(|e| format!("batch provider: {}", nserr(&e)))?;
                providers.push(provider);
            }
            let refs: Vec<&MLDictionaryFeatureProvider> =
                providers.iter().map(|v| v.as_ref()).collect();
            let providers = objc2_foundation::NSArray::from_slice(&refs);
            let names: Vec<_> = out_names
                .iter()
                .map(|n| std::ffi::CString::new(*n).map_err(|e| e.to_string()))
                .collect::<Result<_, _>>()?;
            let name_ptrs: Vec<_> = names.iter().map(|n| n.as_ptr()).collect();
            let mut error = std::ptr::null_mut();
            let mut exception = std::ptr::null_mut();
            let result = rightkit_dictation_coreml_feature_batch(
                Retained::as_ptr(&self.model).cast_mut().cast(),
                Retained::as_ptr(&providers).cast_mut().cast(),
                name_ptrs.as_ptr(),
                names.len(),
                &mut error,
                &mut exception,
            );
            if !exception.is_null() {
                let description = Retained::<NSString>::from_raw(exception.cast()).unwrap();
                return Err(format!("feature batch exception: {description}"));
            }
            if !error.is_null() {
                let error = Retained::<objc2_foundation::NSError>::from_raw(error.cast()).unwrap();
                return Err(format!("feature batch: {}", nserr(&error)));
            }
            let arrays =
                Retained::<objc2_foundation::NSArray<MLMultiArray>>::from_raw(result.cast())
                    .ok_or("feature batch returned no result")?;
            if arrays.len() != inputs.len() * out_names.len() {
                return Err("feature batch output count mismatch".into());
            }
            // Consume retained output handles once in provider/output order;
            // avoid a flat intermediate plus another retain/release per row.
            let mut ordered = arrays.iter();
            Ok((0..inputs.len())
                .map(|_| ordered.by_ref().take(out_names.len()).collect())
                .collect())
        }
    }
    /// Load a stage. `.mlmodelc` loads directly; `.mlpackage` / `.mlmodel`
    /// are compiled once and cached as `.mlmodelc`. Packaged macOS apps run
    /// from read-only locations such as a mounted DMG, so the compiled cache lives
    /// in the writable app-data cache the engine resolves at startup
    /// (`engine_paths::coreml_compiled_cache_dir`).
    pub fn load(path: &Path) -> Result<Self, String> {
        Self::load_inner(path, None)
    }

    /// Load a stage using a persisted developer compute profile. The
    /// process-level RIGHTKIT_COREML_CU override remains highest priority.
    pub fn load_with_compute_profile(path: &Path, profile: &str) -> Result<Self, String> {
        Self::load_inner(path, Some(profile))
    }

    /// Load a stage pinned to CPU-only compute. For tiny per-frame models
    /// (e.g. the Silero VAD, one 32 ms frame per call) where ANE/GPU dispatch
    /// overhead would exceed the compute itself. `RIGHTKIT_COREML_CU` still wins.
    pub fn load_cpu_only(path: &Path) -> Result<Self, String> {
        Self::load_inner(path, Some("cpu_only"))
    }

    fn load_inner(path: &Path, configured_profile: Option<&str>) -> Result<Self, String> {
        autoreleasepool(|_| unsafe {
            let config = MLModelConfiguration::new();
            // Compute units overridable via RIGHTKIT_COREML_CU for variable isolation:
            // `cpuOnly` skips the ANE compile entirely (loads in seconds — use it to
            // debug decode correctness without the ~10-min cold ANE compile),
            // `all` = CPU+GPU+ANE, default `cpuAndNeuralEngine`.
            #[cfg(not(target_arch = "x86_64"))]
            let fallback_cu = MLComputeUnits::CPUAndNeuralEngine;
            #[cfg(target_arch = "x86_64")]
            let fallback_cu = MLComputeUnits::CPUAndGPU;
            let (cu, compute_profile) =
                match rightkit_dictation_core::dev_var!("RIGHTKIT_COREML_CU").as_deref() {
                    Ok("cpuOnly") => (MLComputeUnits::CPUOnly, "env_cpu_only"),
                    Ok("all") => (MLComputeUnits::All, "env_all"),
                    Ok("cpuAndGPU") => (MLComputeUnits::CPUAndGPU, "env_cpu_gpu"),
                    Ok("cpuAndNeuralEngine") => {
                        (MLComputeUnits::CPUAndNeuralEngine, "env_neural_engine")
                    }
                    _ => match configured_profile {
                        Some("neural_engine") => {
                            (MLComputeUnits::CPUAndNeuralEngine, "neural_engine")
                        }
                        Some("cpu_gpu") => (MLComputeUnits::CPUAndGPU, "cpu_gpu"),
                        Some("cpu_only") => (MLComputeUnits::CPUOnly, "cpu_only"),
                        _ => (fallback_cu, "automatic"),
                    },
                };
            config.setComputeUnits(cu);
            apply_configuration_probes(&config);
            let already_compiled = path.extension().and_then(|e| e.to_str()) == Some("mlmodelc");
            let (load_path, compile_cache) = if already_compiled {
                (path.to_path_buf(), "precompiled_bundle")
            } else {
                let cache_path = compiled_model_cache_path(path);
                let cache_hit = cache_path.join("model.mil").exists()
                    || cache_path.join("coremldata.bin").exists();
                (
                    persistent_compiled_model(path)?,
                    if cache_hit {
                        "cache_hit"
                    } else {
                        "fresh_compile"
                    },
                )
            };
            let p = NSString::from_str(&load_path.to_string_lossy());
            let url = NSURL::fileURLWithPath(&p);
            let model = MLModel::modelWithContentsOfURL_configuration_error(&url, &config)
                .map_err(|e| format!("load {}: {}", load_path.display(), nserr(&e)))?;
            Ok(Self {
                model,
                compute_profile,
                compile_cache,
                output_backings: RefCell::new(HashMap::new()),
                output_backings_enabled: false,
            })
        })
    }

    /// Arm reusable output buffers for a validated production graph.
    pub(crate) fn enable_output_backings(&mut self) {
        self.output_backings_enabled = true;
    }

    pub(crate) fn set_output_backings_enabled(&mut self, enabled: bool) {
        self.output_backings_enabled = enabled;
    }

    pub(crate) fn output_backings_enabled(&self) -> bool {
        self.output_backings_enabled
    }

    /// Return and clear scalar output-backing probe counts.
    pub fn take_output_backing_probe_counts() -> OutputBackingProbeCounts {
        OutputBackingProbeCounts {
            requested: OUTPUT_BACKING_REQUESTED.swap(0, Ordering::Relaxed),
            honored: OUTPUT_BACKING_HONORED.swap(0, Ordering::Relaxed),
            ignored: OUTPUT_BACKING_IGNORED.swap(0, Ordering::Relaxed),
            failed: OUTPUT_BACKING_FAILED.swap(0, Ordering::Relaxed),
        }
    }

    pub fn compute_profile(&self) -> &'static str {
        self.compute_profile
    }

    pub fn compile_cache(&self) -> &'static str {
        self.compile_cache
    }

    /// Output feature names, in dictionary order.
    pub fn output_names(&self) -> Vec<String> {
        unsafe {
            let desc = self.model.modelDescription();
            let by_name = desc.outputDescriptionsByName();
            by_name.allKeys().iter().map(|k| k.to_string()).collect()
        }
    }

    /// Input feature names, in dictionary order.
    pub fn input_names(&self) -> Vec<String> {
        unsafe {
            let desc = self.model.modelDescription();
            let by_name = desc.inputDescriptionsByName();
            by_name.allKeys().iter().map(|k| k.to_string()).collect()
        }
    }

    /// The single output whose multiarray shape equals `want`.
    pub fn output_by_shape(&self, want: &[usize]) -> Result<String, String> {
        self.outputs_by_shape(want)
            .into_iter()
            .next()
            .ok_or_else(|| format!("no output with shape {want:?}"))
    }

    /// All outputs whose shape equals `want`, sorted by name (autogen `var_NN`
    /// names sort into spec/declaration order).
    pub fn outputs_by_shape(&self, want: &[usize]) -> Vec<String> {
        unsafe {
            let desc = self.model.modelDescription();
            let by_name = desc.outputDescriptionsByName();
            let mut hits: Vec<String> = Vec::new();
            for key in by_name.allKeys().iter() {
                if let Some(fd) = by_name.objectForKey(&key) {
                    if let Some(c) = fd.multiArrayConstraint() {
                        let shape: Vec<usize> = c
                            .shape()
                            .iter()
                            .map(|n| n.integerValue() as usize)
                            .collect();
                        if shape == want {
                            hits.push(key.to_string());
                        }
                    }
                }
            }
            hits.sort();
            hits
        }
    }

    /// Run a prediction, return one named output's MLMultiArray.
    pub fn predict(
        &self,
        inputs: &[(&str, &Retained<MLMultiArray>)],
        out_name: &str,
    ) -> Result<Retained<MLMultiArray>, String> {
        Ok(self
            .predict_multi(inputs, &[out_name])?
            .into_iter()
            .next()
            .unwrap())
    }

    /// Run a prediction, return several named outputs (in `out_names` order).
    pub fn predict_multi(
        &self,
        inputs: &[(&str, &Retained<MLMultiArray>)],
        out_names: &[&str],
    ) -> Result<Vec<Retained<MLMultiArray>>, String> {
        self.predict_multi_shaped(inputs, out_names, &[])
    }

    /// Probe-only concrete shapes for flexible tensor batch outputs. Model
    /// constraints describe default B1, while a synchronous call may use B2.
    pub(crate) fn predict_multi_shaped(
        &self,
        inputs: &[(&str, &Retained<MLMultiArray>)],
        out_names: &[&str],
        output_shapes: &[&[usize]],
    ) -> Result<Vec<Retained<MLMultiArray>>, String> {
        if !output_shapes.is_empty() && output_shapes.len() != out_names.len() {
            return Err("CoreML concrete output shape count mismatch".into());
        }
        unsafe {
            let keys: Vec<Retained<NSString>> =
                inputs.iter().map(|(k, _)| NSString::from_str(k)).collect();
            let vals: Vec<Retained<MLFeatureValue>> = inputs
                .iter()
                .map(|(_, v)| MLFeatureValue::featureValueWithMultiArray(v))
                .collect();
            let key_refs: Vec<&NSString> = keys.iter().map(|k| k.as_ref()).collect();
            let val_refs: Vec<&MLFeatureValue> = vals.iter().map(|v| v.as_ref()).collect();
            let dict: Retained<NSDictionary<NSString, MLFeatureValue>> =
                NSDictionary::from_slices(&key_refs, &val_refs);
            let provider = MLDictionaryFeatureProvider::initWithDictionary_error(
                MLDictionaryFeatureProvider::alloc(),
                // SAFETY: NSDictionary<NSString, MLFeatureValue> is layout-identical to
                // NSDictionary<NSString, AnyObject> (MLFeatureValue: NSObject).
                std::mem::transmute::<
                    &NSDictionary<NSString, MLFeatureValue>,
                    &NSDictionary<NSString, objc2::runtime::AnyObject>,
                >(&dict),
            )
            .map_err(|e| format!("feature provider: {}", nserr(&e)))?;

            let provider_obj: &ProtocolObject<dyn MLFeatureProvider> =
                ProtocolObject::from_ref(&*provider);
            let provider_ptr =
                provider_obj.as_ref() as *const objc2::runtime::AnyObject as *mut c_void;
            // `NSError` does not cover Objective-C exceptions. Keep the message
            // send wholly inside native `@try/@catch`; allowing NSException to
            // unwind through Rust aborts release builds (`panic = "abort"`).
            let (result, output_backings) = if (output_backing_probe_enabled()
                || self.output_backings_enabled)
                && !out_names.is_empty()
            {
                let probe: Result<
                    (
                        Retained<objc2::runtime::AnyObject>,
                        Vec<Retained<MLMultiArray>>,
                    ),
                    String,
                > = (|| {
                    let mut cache = self.output_backings.try_borrow_mut().map_err(|_| {
                        "CoreML output backing probe re-entered on one thread".to_string()
                    })?;
                    let mut backings = Vec::with_capacity(out_names.len());
                    for (index, name) in out_names.iter().enumerate() {
                        let (default_shape, kind) = self.output_backing_spec(name)?;
                        let shape = output_shapes
                            .get(index)
                            .map(|shape| shape.to_vec())
                            .unwrap_or(default_shape);
                        let key = OutputBackingKey {
                            name: (*name).to_string(),
                            shape: shape.clone(),
                            kind,
                        };
                        let backing = match cache.entry(key) {
                            std::collections::hash_map::Entry::Vacant(entry) => {
                                let count = shape
                                    .iter()
                                    .try_fold(1usize, |n, &d| n.checked_mul(d))
                                    .ok_or_else(|| {
                                        format!("output `{name}` shape {shape:?} overflows")
                                    })?;
                                let backing = match kind {
                                    OutputBackingKind::Float16 => {
                                        ml_f16(&shape, &vec![0.0; count])?
                                    }
                                    OutputBackingKind::Float32 => {
                                        ml_f32(&shape, &vec![0.0; count])?
                                    }
                                    OutputBackingKind::Int32 => ml_i32(&shape, &vec![0; count])?,
                                };
                                (*entry.insert(backing)).clone()
                            }
                            std::collections::hash_map::Entry::Occupied(entry) => {
                                entry.get().clone()
                            }
                        };
                        backings.push(backing);
                    }

                    let result = prediction_with_output_backings(
                        &self.model,
                        provider_ptr,
                        out_names,
                        &backings,
                    )?;
                    Ok((result, backings))
                })();
                match probe {
                    Ok(value) => {
                        OUTPUT_BACKING_REQUESTED
                            .fetch_add(out_names.len() as u64, Ordering::Relaxed);
                        value
                    }
                    Err(error) => {
                        OUTPUT_BACKING_FAILED.fetch_add(out_names.len() as u64, Ordering::Relaxed);
                        tracing::debug!(
                            error,
                            "CoreML output backing probe unavailable; using normal prediction"
                        );
                        let result = prediction_default(&self.model, provider_ptr)?;
                        (result, Vec::new())
                    }
                }
            } else {
                let result = prediction_default(&self.model, provider_ptr)?;
                (result, Vec::new())
            };
            // SAFETY: the Objective-C bridge's result is declared
            // `id<MLFeatureProvider>` and only returns that value on success.
            let result: Retained<ProtocolObject<dyn MLFeatureProvider>> =
                Retained::cast_unchecked(result);

            let mut outs = Vec::with_capacity(out_names.len());
            for name in out_names {
                let ns = NSString::from_str(name);
                let fv = result
                    .featureValueForName(&ns)
                    .ok_or_else(|| format!("output `{name}` missing"))?;
                let arr = fv
                    .multiArrayValue()
                    .ok_or_else(|| format!("output `{name}` is not a multiarray"))?;
                outs.push(arr);
            }
            if !output_backings.is_empty() {
                for (array, backing) in outs.iter().zip(output_backings.iter()) {
                    if Retained::as_ptr(array) == Retained::as_ptr(backing) {
                        OUTPUT_BACKING_HONORED.fetch_add(1, Ordering::Relaxed);
                    } else {
                        OUTPUT_BACKING_IGNORED.fetch_add(1, Ordering::Relaxed);
                    }
                }
            }
            Ok(outs)
        }
    }

    fn output_backing_spec(&self, name: &str) -> Result<(Vec<usize>, OutputBackingKind), String> {
        unsafe {
            let desc = self.model.modelDescription();
            let outputs = desc.outputDescriptionsByName();
            let key = NSString::from_str(name);
            let feature = outputs
                .objectForKey(&key)
                .ok_or_else(|| format!("output `{name}` missing from model description"))?;
            let constraint = feature
                .multiArrayConstraint()
                .ok_or_else(|| format!("output `{name}` has no fixed multiarray constraint"))?;
            let shape = constraint
                .shape()
                .iter()
                .map(|dimension| dimension.integerValue() as usize)
                .collect::<Vec<_>>();
            let kind = match constraint.dataType() {
                value if value == MLMultiArrayDataType::Float16 => OutputBackingKind::Float16,
                value if value == MLMultiArrayDataType::Float32 => OutputBackingKind::Float32,
                value if value == MLMultiArrayDataType::Int32 => OutputBackingKind::Int32,
                _ => return Err(format!("output `{name}` has unsupported backing type")),
            };
            Ok((shape, kind))
        }
    }

    /// Run Joint over one decoder state and several encoder frames in one
    /// CoreML batch. Each returned array corresponds to its input frame.
    pub fn predict_joint_batch(
        &self,
        encoders: &[&Retained<MLMultiArray>],
        decoder: &Retained<MLMultiArray>,
        output: &str,
    ) -> Result<Vec<Retained<MLMultiArray>>, String> {
        if encoders.is_empty() {
            return Err("CoreML joint batch requires at least one encoder frame".into());
        }
        let output = std::ffi::CString::new(output)
            .map_err(|_| "CoreML joint output name contains NUL".to_string())?;
        let mut encoder_ptrs: Vec<*mut c_void> = encoders
            .iter()
            .map(|array| Retained::as_ptr(array).cast_mut().cast())
            .collect();
        unsafe {
            let mut error = std::ptr::null_mut();
            let mut exception = std::ptr::null_mut();
            let result = rightkit_dictation_coreml_joint_batch(
                Retained::as_ptr(&self.model).cast_mut().cast(),
                encoder_ptrs.as_mut_ptr(),
                Retained::as_ptr(decoder).cast_mut().cast(),
                encoder_ptrs.len(),
                output.as_ptr(),
                &mut error,
                &mut exception,
            );
            if !exception.is_null() {
                let description = Retained::<NSString>::from_raw(exception.cast())
                    .expect("batch bridge returned a non-null exception description");
                return Err(format!("batch prediction exception: {description}"));
            }
            if !error.is_null() {
                let error = Retained::<objc2_foundation::NSError>::from_raw(error.cast())
                    .expect("batch bridge returned a non-null NSError");
                return Err(format!("batch prediction: {}", nserr(&error)));
            }
            let arrays =
                Retained::<objc2_foundation::NSArray<MLMultiArray>>::from_raw(result.cast())
                    .ok_or_else(|| "batch prediction returned no result".to_string())?;
            if arrays.len() != encoders.len() {
                return Err(format!(
                    "batch prediction returned {} outputs for {} inputs",
                    arrays.len(),
                    encoders.len()
                ));
            }
            Ok(arrays.iter().collect())
        }
    }
}

unsafe fn adopt_prediction(
    result: *mut c_void,
    error: *mut c_void,
    exception: *mut c_void,
) -> Result<Retained<objc2::runtime::AnyObject>, String> {
    if !exception.is_null() {
        let description = Retained::<NSString>::from_raw(exception.cast())
            .expect("bridge returned a non-null exception description");
        return Err(format!("prediction exception: {description}"));
    }
    if !error.is_null() {
        let error = Retained::<objc2_foundation::NSError>::from_raw(error.cast())
            .expect("bridge returned a non-null NSError");
        return Err(format!("prediction: {}", nserr(&error)));
    }
    Retained::<objc2::runtime::AnyObject>::from_raw(result.cast())
        .ok_or_else(|| "prediction returned no result or error".to_string())
}

unsafe fn prediction_default(
    model: &Retained<MLModel>,
    provider: *mut c_void,
) -> Result<Retained<objc2::runtime::AnyObject>, String> {
    let mut error = std::ptr::null_mut();
    let mut exception = std::ptr::null_mut();
    let result = rightkit_dictation_coreml_prediction(
        Retained::as_ptr(model).cast_mut().cast(),
        provider,
        &mut error,
        &mut exception,
    );
    adopt_prediction(result, error, exception)
}

unsafe fn prediction_with_output_backings(
    model: &Retained<MLModel>,
    provider: *mut c_void,
    names: &[&str],
    backings: &[Retained<MLMultiArray>],
) -> Result<Retained<objc2::runtime::AnyObject>, String> {
    let names: Vec<_> = names
        .iter()
        .map(|name| std::ffi::CString::new(*name).map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let name_ptrs: Vec<_> = names.iter().map(|name| name.as_ptr()).collect();
    let mut backing_ptrs: Vec<*mut c_void> = backings
        .iter()
        .map(|backing| Retained::as_ptr(backing).cast_mut().cast())
        .collect();
    let mut error = std::ptr::null_mut();
    let mut exception = std::ptr::null_mut();
    let result = rightkit_dictation_coreml_prediction_with_output_backings(
        Retained::as_ptr(model).cast_mut().cast(),
        provider,
        name_ptrs.as_ptr(),
        backing_ptrs.as_mut_ptr(),
        backing_ptrs.len(),
        &mut error,
        &mut exception,
    );
    adopt_prediction(result, error, exception)
}

pub fn nserr(e: &objc2_foundation::NSError) -> String {
    e.localizedDescription().to_string()
}

fn persistent_compiled_model(path: &Path) -> Result<PathBuf, String> {
    let compiled_path = compiled_model_cache_path(path);
    if compiled_path.join("model.mil").exists() || compiled_path.join("coremldata.bin").exists() {
        return Ok(compiled_path);
    }

    tracing::info!(
        "coreml_compile_start {} -> {}",
        path.display(),
        compiled_path.display()
    );
    let compiled_tmp = autoreleasepool(|_| unsafe {
        let p = NSString::from_str(&path.to_string_lossy());
        let url = NSURL::fileURLWithPath(&p);
        MLModel::compileModelAtURL_error(&url)
            .map_err(|e| format!("compile {}: {}", path.display(), nserr(&e)))
            .and_then(|url| {
                let compiled = url.path().ok_or_else(|| {
                    format!("compiled model URL has no path for {}", path.display())
                })?;
                Ok(PathBuf::from(compiled.to_string()))
            })
    })?;

    let tmp_destination = compiled_path.with_extension("mlmodelc.tmp");
    if tmp_destination.exists() {
        std::fs::remove_dir_all(&tmp_destination)
            .map_err(|e| format!("remove stale {}: {e}", tmp_destination.display()))?;
    }
    if let Some(parent) = tmp_destination.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("create {}: {e}", parent.display()))?;
    }
    // The staging path was cleared above, so nothing may be overwritten. Links in a compiled tree
    // were always skipped (neither copied nor followed). `copy_tree` would follow a link to a file
    // and refuse a link to a directory, so it is told to keep links as links (it never follows
    // one) and every link is then removed from the staging copy: the published tree is the same
    // as before, and the number skipped is logged.
    rightkit_fs::copy_tree(
        &compiled_tmp,
        &tmp_destination,
        rightkit_fs::CopyOptions::new(true, true),
    )
    .map_err(|e| {
        format!(
            "copy {} -> {}: {e}",
            compiled_tmp.display(),
            tmp_destination.display()
        )
    })?;
    let skipped = remove_links(&tmp_destination)?;
    if skipped > 0 {
        tracing::warn!(
            "coreml_compile_links_skipped count={skipped} path={}",
            compiled_path.display()
        );
    }
    // rightkit-fs owns the directory publish: an existing cache is exchanged atomically
    // (or moved aside on volumes without exchange), never deleted before the new tree is in.
    rightkit_fs::publish_dir::publish_dir(&tmp_destination, &compiled_path).map_err(|e| {
        format!(
            "install compiled model {} -> {}: {e}",
            tmp_destination.display(),
            compiled_path.display()
        )
    })?;
    tracing::info!("coreml_compile_done {}", compiled_path.display());
    Ok(compiled_path)
}

/// Remove every symbolic link under `root` (links themselves, never what they point at) and return
/// how many there were.
fn remove_links(root: &Path) -> Result<usize, String> {
    let mut removed = 0;
    for entry in std::fs::read_dir(root).map_err(|e| format!("read {}: {e}", root.display()))? {
        let entry = entry.map_err(|e| format!("read entry in {}: {e}", root.display()))?;
        let path = entry.path();
        let kind = entry
            .file_type()
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        if kind.is_symlink() {
            std::fs::remove_file(&path)
                .map_err(|e| format!("remove link {}: {e}", path.display()))?;
            removed += 1;
        } else if kind.is_dir() {
            removed += remove_links(&path)?;
        }
    }
    Ok(removed)
}

fn compiled_model_cache_path(path: &Path) -> PathBuf {
    if let Some(cache_dir) = crate::engine_paths::coreml_compiled_cache_dir() {
        // A persisted directory name must not depend on the compiler's hasher, so the path is hashed with SHA-256.
        let hash =
            rightkit_fs::digest::sha256_bytes(path.to_string_lossy().as_bytes())[..16].to_string();
        let stem = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("model")
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect::<String>();
        cache_dir.join(format!("{stem}-{hash}.mlmodelc"))
    } else {
        path.with_extension("mlmodelc")
    }
}
