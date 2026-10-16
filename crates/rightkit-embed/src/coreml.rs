//! EmbeddingGemma 2 Core ML bundle (macOS 15+): `EmbeddingGemma2Text.mlpackage`, a multi-function
//! ML Program with one fixed-shape function per sequence length, plus the bf16 token table that
//! is looked up on the host. Source: `FluidInference/embeddinggemma-2-coreml`; its card and the
//! package spec are the contract. ONNX Runtime cannot load an `.mlpackage`, so this talks to
//! Core ML directly through `objc2-core-ml` (same crate versions as `rightkit-asr`'s bridge).
//!
//! Contract of function `embed_<L>` for `L` in 32, 64, 128, 256, 512 (batch 1):
//!
//! - input `inputs_embeds` float16 `[1, L, 512]`: `embeddings.bf16[token_id]` (bf16 widened to
//!   f32 by a 16-bit left shift) multiplied by `embed_scale` (`config.json`, sqrt(512) =
//!   22.627417), rounded to float16; pad positions carry the pad-id row, scaled the same way;
//! - input `attention_mask` float16 `[1, L]`: 1 on real tokens, 0 on right padding;
//! - output `embedding` float32 `[1, 768]`: mean pooling, the 768 projection and L2
//!   normalisation are all in the graph, nothing is applied on the host.
//!
//! A text runs through the smallest function whose length is at least its token count; longer
//! token sequences are truncated to 512 by the caller (first 512 ids kept). The package is
//! compiled once with `MLModel.compileModelAtURL` into a cache directory keyed on the hashes of
//! the package files and the macOS version; each function is loaded on first use through
//! `MLModelConfiguration.functionName` (macOS 15). Compute units are chosen per function
//! ([`CoreMlUnits`]; default `cpu_and_ne` up to 128 tokens, `cpu_and_gpu` for 256 and 512);
//! `CpuOnly` on every function is the parity reference. Each function is one `MLModel` on its
//! own units, loaded once and kept resident; [`CoreMlEngine::prewarm`] runs one dummy predict per
//! function at load so no query pays the first-call compile or shader build. The compiled
//! `.mlmodelc` does not depend on the units, so one cache entry serves every unit choice.

#![allow(deprecated)] // MLMultiArray::dataPointer, as in rightkit-asr's bridge

use std::collections::BTreeMap;
use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::AnyThread;
use objc2_core_ml::{
    MLComputeUnits, MLDictionaryFeatureProvider, MLFeatureProvider, MLFeatureValue, MLModel,
    MLModelConfiguration, MLMultiArray, MLMultiArrayDataType,
};
use objc2_foundation::{
    NSArray, NSDictionary, NSError, NSNumber, NSOperatingSystemVersion, NSProcessInfo, NSString,
    NSURL,
};
use rightkit_onnx::half::f16;

use crate::coreml_units::{CoreMlComputeUnit, CoreMlUnits};
use crate::e40::{bucket_for, BUCKETS, HIDDEN, MAX_TOKENS, VOCAB};
use crate::embedder::{hash_file, EmbedError};

/// The text package inside the model directory (a directory; its files are pinned individually).
pub(crate) const PACKAGE_DIR: &str = "EmbeddingGemma2Text.mlpackage";
/// Host-side token table: raw little-endian bfloat16, row-major `[VOCAB, HIDDEN]`, no header.
pub(crate) const TABLE_FILE: &str = "embeddings.bf16";
/// Files that must exist for a directory to be a Core ML bundle and that the pipeline id covers.
/// `config.json` is covered because it carries `embed_scale`.
pub(crate) const ARTIFACTS: [&str; 6] = [
    "EmbeddingGemma2Text.mlpackage/Manifest.json",
    "EmbeddingGemma2Text.mlpackage/Data/com.apple.CoreML/model.mlmodel",
    "EmbeddingGemma2Text.mlpackage/Data/com.apple.CoreML/weights/weight.bin",
    TABLE_FILE,
    "tokenizer.json",
    "config.json",
];
/// Package files whose hashes key the compile cache (the first three of [`ARTIFACTS`]).
const PACKAGE_FILES: [&str; 3] = [ARTIFACTS[0], ARTIFACTS[1], ARTIFACTS[2]];
/// Name of the cache directory created under the model directory when none is configured.
pub(crate) const DEFAULT_CACHE_SUBDIR: &str = ".coreml-compiled";
/// Input and output feature names of every `embed_<L>` function.
const INPUT_EMBEDS: &str = "inputs_embeds";
const INPUT_MASK: &str = "attention_mask";
const OUTPUT_NAME: &str = "embedding";
/// Native output width.
const EMBED_DIM: usize = 768;
/// Bump when the cache layout or compile procedure changes.
const CACHE_SCHEMA: &str = "rightkit-embed/coreml-cache/1";

/// True when `dir` holds the Core ML layout: the text package beside the bf16 token table. This
/// is how the bundle is chosen, from the directory and never from a setting.
pub(crate) fn is_bundle(dir: &Path) -> bool {
    dir.join(PACKAGE_DIR).is_dir() && dir.join(TABLE_FILE).is_file()
}

/// The `MLComputeUnits` value of a compute-unit choice.
fn ml_units(unit: CoreMlComputeUnit) -> MLComputeUnits {
    match unit {
        CoreMlComputeUnit::All => MLComputeUnits::All,
        CoreMlComputeUnit::CpuAndNe => MLComputeUnits::CPUAndNeuralEngine,
        CoreMlComputeUnit::CpuAndGpu => MLComputeUnits::CPUAndGPU,
        CoreMlComputeUnit::CpuOnly => MLComputeUnits::CPUOnly,
    }
}

/// Name of the function that serves `bucket` tokens.
pub(crate) fn function_name(bucket: usize) -> String {
    format!("embed_{bucket}")
}

/// Validates the fields of `config.json` this backend relies on and returns `embed_scale`.
/// Mirrors the checks of the 12 Oct Mac measurement harness.
pub(crate) fn parse_config(bytes: &[u8]) -> Result<f32, String> {
    let config: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|e| format!("unreadable config.json: {e}"))?;
    let number = |key: &str| {
        config[key]
            .as_f64()
            .ok_or_else(|| format!("config.json has no numeric `{key}`"))
    };
    if number("hidden_size")? as usize != HIDDEN || number("embedding_dim")? as usize != EMBED_DIM {
        return Err(format!(
            "config.json is not hidden {HIDDEN} / embedding {EMBED_DIM}"
        ));
    }
    let scale = number("embed_scale")?;
    if (scale - (HIDDEN as f64).sqrt()).abs() > 1e-5 {
        return Err(format!("embed_scale {scale} is not sqrt({HIDDEN})"));
    }
    let table = &config["token_table"];
    if table["dtype"] != "bfloat16" || table["shape"] != serde_json::json!([VOCAB, HIDDEN]) {
        return Err(format!("token_table is not bfloat16 [{VOCAB}, {HIDDEN}]"));
    }
    for bucket in BUCKETS {
        if config["functions"][function_name(bucket)]["tokens"] != bucket {
            return Err(format!(
                "config.json lists no function {}",
                function_name(bucket)
            ));
        }
    }
    Ok(scale as f32)
}

/// bfloat16 bits widened to f32: the 16 bits become the high half of an f32.
pub(crate) fn bf16_to_f32(bits: u16) -> f32 {
    f32::from_bits(u32::from(bits) << 16)
}

/// The host-side token table: the file's little-endian bf16 bytes (268 MB) and the embedding
/// scale. A looked-up row is widened, scaled in f32 and rounded to f16, the order the harness
/// uses (`(rows_f32 * scale).astype(float16)`).
pub(crate) struct TokenTable {
    bytes: Vec<u8>,
    rows: usize,
    scale: f32,
}

impl TokenTable {
    pub(crate) fn load(path: &Path, scale: f32) -> Result<Self, EmbedError> {
        let bytes = std::fs::read(path).map_err(|e| {
            EmbedError::InvalidConfig(format!("token table {}: {e}", path.display()))
        })?;
        Self::from_bytes(bytes, VOCAB, scale)
            .map_err(|e| EmbedError::InvalidConfig(format!("token table {}: {e}", path.display())))
    }

    /// `rows` is [`VOCAB`] for the real file; tests use a small table.
    pub(crate) fn from_bytes(bytes: Vec<u8>, rows: usize, scale: f32) -> Result<Self, String> {
        let expected = rows * HIDDEN * 2;
        if bytes.len() != expected {
            return Err(format!(
                "expected {expected} bytes ({rows} x {HIDDEN} bfloat16), found {}",
                bytes.len()
            ));
        }
        Ok(Self { bytes, rows, scale })
    }

    /// Appends the scaled fp16 row of `id` to `out`.
    pub(crate) fn push_row(&self, id: i64, out: &mut Vec<f16>) -> Result<(), EmbedError> {
        let row = usize::try_from(id)
            .ok()
            .filter(|row| *row < self.rows)
            .ok_or_else(|| {
                EmbedError::Tokenizer(format!("token id {id} is outside the token table"))
            })?;
        let start = row * HIDDEN * 2;
        for pair in self.bytes[start..start + HIDDEN * 2].as_chunks::<2>().0 {
            let value = bf16_to_f32(u16::from_le_bytes([pair[0], pair[1]])) * self.scale;
            let half = f16::from_f32(value);
            if !half.is_finite() {
                return Err(EmbedError::Output(format!(
                    "token {id}: scaled embedding does not fit float16"
                )));
            }
            out.push(half);
        }
        Ok(())
    }
}

/// `inputs_embeds` (`bucket * HIDDEN` values) and the float16 mask for `ids` padded to `bucket`
/// with `pad_id` rows and a zero mask.
pub(crate) fn build_inputs(
    table: &TokenTable,
    ids: &[i64],
    bucket: usize,
    pad_id: i64,
) -> Result<(Vec<f16>, Vec<f16>), EmbedError> {
    let mut embeds = Vec::with_capacity(bucket * HIDDEN);
    let mut mask = vec![f16::ZERO; bucket];
    for (position, flag) in mask.iter_mut().enumerate() {
        match ids.get(position) {
            Some(id) => {
                table.push_row(*id, &mut embeds)?;
                *flag = f16::ONE;
            }
            None => table.push_row(pad_id, &mut embeds)?,
        }
    }
    Ok((embeds, mask))
}

/// Cache key: SHA-256 (first 16 bytes, hex) over the schema, the hashes of the package files and
/// the macOS version (a compiled model is not promised to survive an OS update).
pub(crate) fn cache_key(file_hashes: &[(&str, String)], os: (isize, isize, isize)) -> String {
    let mut text = format!("schema={CACHE_SCHEMA}\nos={}.{}.{}\n", os.0, os.1, os.2);
    for (name, hash) in file_hashes {
        text.push_str(&format!("{name}={hash}\n"));
    }
    rightkit_fs::digest::sha256_bytes(text.as_bytes())[..32].to_owned()
}

fn nserr(error: &NSError) -> String {
    error.localizedDescription().to_string()
}

fn runtime(message: impl std::fmt::Display) -> EmbedError {
    EmbedError::Runtime(format!("Core ML: {message}"))
}

fn os_version() -> (isize, isize, isize) {
    let v = NSProcessInfo::processInfo().operatingSystemVersion();
    (v.majorVersion, v.minorVersion, v.patchVersion)
}

/// Function selection (`MLModelConfiguration.functionName`) needs macOS 15.
fn require_macos_15() -> Result<(), EmbedError> {
    let at_least =
        NSProcessInfo::processInfo().isOperatingSystemAtLeastVersion(NSOperatingSystemVersion {
            majorVersion: 15,
            minorVersion: 0,
            patchVersion: 0,
        });
    if at_least {
        Ok(())
    } else {
        let (major, minor, patch) = os_version();
        Err(runtime(format!(
            "the EmbeddingGemma 2 Core ML package needs macOS 15 or later (multi-function \
             models); this is macOS {major}.{minor}.{patch}"
        )))
    }
}

/// One loaded function. Apple documents `MLModel` prediction as thread-safe; objc2 does not
/// encode that, so the wrapper asserts `Send` and every use goes through the engine's mutex.
struct Loaded(Retained<MLModel>);

// SAFETY: see above; `Loaded` is only reachable through `CoreMlEngine::models`' mutex.
unsafe impl Send for Loaded {}

/// The functions of one package on a per-length compute-unit map, with the shared token table.
/// `models` holds at most one `MLModel` per (length, unit): the unit of a length is fixed by
/// `units`, so there are at most five, built once and resident until `release`.
pub(crate) struct CoreMlEngine {
    compiled: PathBuf,
    table: Arc<TokenTable>,
    units: CoreMlUnits,
    models: Mutex<BTreeMap<(usize, CoreMlComputeUnit), Loaded>>,
}

impl CoreMlEngine {
    /// Validates the bundle, reads `config.json` and the token table, and compiles the package
    /// into `cache_dir` (default `<dir>/.coreml-compiled`) unless a compile keyed on the same
    /// package hashes and macOS version is already there. No function is loaded yet.
    pub(crate) fn open(
        dir: &Path,
        units: CoreMlUnits,
        cache_dir: Option<&Path>,
    ) -> Result<Self, EmbedError> {
        require_macos_15()?;
        let config_path = dir.join("config.json");
        let config = std::fs::read(&config_path)
            .map_err(|e| EmbedError::InvalidConfig(format!("{}: {e}", config_path.display())))?;
        let scale = parse_config(&config)
            .map_err(|e| EmbedError::InvalidConfig(format!("{}: {e}", config_path.display())))?;
        let table = TokenTable::load(&dir.join(TABLE_FILE), scale)?;
        let mut hashes = Vec::with_capacity(PACKAGE_FILES.len());
        for name in PACKAGE_FILES {
            let path = dir.join(name);
            if !path.is_file() {
                return Err(EmbedError::MissingArtifact(path));
            }
            hashes.push((name, hash_file(&path).map_err(runtime)?));
        }
        let cache = cache_dir
            .map(Path::to_path_buf)
            .unwrap_or_else(|| dir.join(DEFAULT_CACHE_SUBDIR));
        let compiled = cache.join(format!(
            "embeddinggemma2-text-{}.mlmodelc",
            cache_key(&hashes, os_version())
        ));
        if !compiled.is_dir() {
            compile_into(&dir.join(PACKAGE_DIR), &compiled)?;
        }
        Ok(Self {
            compiled,
            table: Arc::new(table),
            units,
            models: Mutex::new(BTreeMap::new()),
        })
    }

    /// An engine over the same compiled package and token table (not read or compiled again)
    /// on `units`.
    pub(crate) fn with_units(&self, units: CoreMlUnits) -> Self {
        Self {
            compiled: self.compiled.clone(),
            table: Arc::clone(&self.table),
            units,
            models: Mutex::new(BTreeMap::new()),
        }
    }

    /// The compute units of each function on this engine.
    pub(crate) fn units(&self) -> &CoreMlUnits {
        &self.units
    }

    /// Name reported by `Embedder::provider_used`: `"cpu"` when every function is `cpu_only`,
    /// else `"coreml"`.
    pub(crate) fn provider_name(&self) -> &'static str {
        if self.units.is_cpu_only() {
            "cpu"
        } else {
            "coreml"
        }
    }

    /// Loads every function on its units and runs one dummy text (the pad token) through each,
    /// so the first query per length pays neither the function load nor the first-call compile
    /// (Neural Engine) or shader build (GPU; measured up to 51 s cold). Returns the total time
    /// and the time per length. A function that cannot load or predict is an error: it must not
    /// be admitted.
    pub(crate) fn prewarm(
        &self,
        pad_id: i64,
    ) -> Result<(Duration, Vec<(usize, Duration)>), EmbedError> {
        let started = Instant::now();
        let mut per_length = Vec::with_capacity(BUCKETS.len());
        for bucket in BUCKETS {
            let one = Instant::now();
            self.run_in(&[pad_id], pad_id, bucket)?;
            per_length.push((bucket, one.elapsed()));
        }
        Ok((started.elapsed(), per_length))
    }

    /// Drops every loaded function; the next text loads the one it needs.
    pub(crate) fn release(&self) {
        if let Ok(mut models) = self.models.lock() {
            models.clear();
        }
    }

    /// Native-dimension (768) vector for one tokenized text, as the function returns it
    /// (already L2-normalised in the graph).
    pub(crate) fn run(&self, ids: &[i64], pad_id: i64) -> Result<Vec<f32>, EmbedError> {
        let bucket = bucket_for(ids.len()).ok_or_else(|| {
            EmbedError::Output(format!(
                "{} tokens exceed the longest Core ML function ({MAX_TOKENS})",
                ids.len()
            ))
        })?;
        self.run_in(ids, pad_id, bucket)
    }

    /// [`Self::run`] on the function of length `bucket` (which must hold `ids`), loaded on the
    /// units the map gives that length.
    fn run_in(&self, ids: &[i64], pad_id: i64, bucket: usize) -> Result<Vec<f32>, EmbedError> {
        let unit = self
            .units
            .unit_for(bucket)
            .ok_or_else(|| runtime(format!("no compute units for length {bucket}")))?;
        let (embeds, mask) = build_inputs(&self.table, ids, bucket, pad_id)?;
        let mut models = self
            .models
            .lock()
            .map_err(|_| runtime("model mutex poisoned"))?;
        let model = &match models.entry((bucket, unit)) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(Loaded(self.load_function(bucket, unit)?))
            }
        }
        .0;
        autoreleasepool(|_| unsafe {
            let embeds = new_f16_array(&[1, bucket, HIDDEN], &embeds)?;
            let mask = new_f16_array(&[1, bucket], &mask)?;
            let output = predict(model, &embeds, &mask)?;
            let values = read_output(&output)?;
            if values.len() != EMBED_DIM {
                return Err(EmbedError::Output(format!(
                    "{} returned {} values, expected {EMBED_DIM}",
                    function_name(bucket),
                    values.len()
                )));
            }
            Ok(values)
        })
    }

    /// One function of the compiled package on `unit`.
    fn load_function(
        &self,
        bucket: usize,
        unit: CoreMlComputeUnit,
    ) -> Result<Retained<MLModel>, EmbedError> {
        let name = function_name(bucket);
        autoreleasepool(|_| unsafe {
            let config = MLModelConfiguration::new();
            config.setComputeUnits(ml_units(unit));
            config.setFunctionName(Some(&NSString::from_str(&name)));
            let url = NSURL::fileURLWithPath(&NSString::from_str(&self.compiled.to_string_lossy()));
            let loaded = objc2::exception::catch(AssertUnwindSafe(|| {
                MLModel::modelWithContentsOfURL_configuration_error(&url, &config)
            }))
            .map_err(|_| runtime(format!("exception loading {name}")))?;
            let model = loaded.map_err(|e| runtime(format!("load {name}: {}", nserr(&e))))?;
            let description = model.modelDescription();
            let outputs: Vec<String> = description
                .outputDescriptionsByName()
                .allKeys()
                .iter()
                .map(|key| key.to_string())
                .collect();
            if !outputs.iter().any(|output| output == OUTPUT_NAME) {
                return Err(runtime(format!(
                    "{name} has outputs {outputs:?}, expected `{OUTPUT_NAME}`"
                )));
            }
            Ok(model)
        })
    }
}

/// Compiles `package` with `MLModel.compileModelAtURL` and installs the result at `dest`
/// atomically (temporary sibling, then rename), so a concurrent or interrupted compile never
/// leaves a half-written `.mlmodelc` at `dest`.
fn compile_into(package: &Path, dest: &Path) -> Result<(), EmbedError> {
    // Core ML compiles every package of the same name to the same temporary path, so two
    // compiles in one process would move or delete each other's output: one at a time, and a
    // compile that waited reuses what the previous one installed.
    static COMPILE: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = COMPILE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if dest.is_dir() {
        return Ok(());
    }
    let compiled: PathBuf = autoreleasepool(|_| unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&package.to_string_lossy()));
        let compiled =
            objc2::exception::catch(AssertUnwindSafe(|| MLModel::compileModelAtURL_error(&url)))
                .map_err(|_| runtime(format!("exception compiling {}", package.display())))?
                .map_err(|e| runtime(format!("compile {}: {}", package.display(), nserr(&e))))?;
        compiled
            .path()
            .map(|path| PathBuf::from(path.to_string()))
            .ok_or_else(|| runtime("compiled model URL has no path"))
    })?;
    let parent = dest
        .parent()
        .ok_or_else(|| runtime(format!("no parent directory for {}", dest.display())))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| runtime(format!("create {}: {e}", parent.display())))?;
    let staging = dest.with_extension(format!("mlmodelc.tmp{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&staging);
    // The compiler writes to a temporary directory, usually on another volume: rename when it
    // is the same volume, copy otherwise.
    if std::fs::rename(&compiled, &staging).is_err() {
        copy_dir(&compiled, &staging)?;
        let _ = std::fs::remove_dir_all(&compiled);
    }
    match std::fs::rename(&staging, dest) {
        Ok(()) => Ok(()),
        // Another process installed the same key first: use its copy.
        Err(_) if dest.is_dir() => {
            let _ = std::fs::remove_dir_all(&staging);
            Ok(())
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(runtime(format!("install {}: {e}", dest.display())))
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> Result<(), EmbedError> {
    let fail = |what: &Path, e: std::io::Error| runtime(format!("{}: {e}", what.display()));
    std::fs::create_dir_all(to).map_err(|e| fail(to, e))?;
    for entry in std::fs::read_dir(from).map_err(|e| fail(from, e))? {
        let entry = entry.map_err(|e| fail(from, e))?;
        let (src, dst) = (entry.path(), to.join(entry.file_name()));
        let kind = entry.file_type().map_err(|e| fail(&src, e))?;
        if kind.is_dir() {
            copy_dir(&src, &dst)?;
        } else if kind.is_file() {
            std::fs::copy(&src, &dst).map_err(|e| fail(&src, e))?;
        }
    }
    Ok(())
}

/// A new fp16 `MLMultiArray` of `dims` holding `data` (C order; a fresh array is contiguous).
unsafe fn new_f16_array(
    dims: &[usize],
    data: &[f16],
) -> Result<Retained<MLMultiArray>, EmbedError> {
    let want: usize = dims.iter().product();
    if want != data.len() {
        return Err(runtime(format!(
            "shape {dims:?} needs {want} elements, got {}",
            data.len()
        )));
    }
    let numbers: Vec<Retained<NSNumber>> = dims
        .iter()
        .map(|&dim| NSNumber::new_isize(dim as isize))
        .collect();
    let refs: Vec<&NSNumber> = numbers.iter().map(|n| n.as_ref()).collect();
    let shape = NSArray::from_slice(&refs);
    let array = MLMultiArray::initWithShape_dataType_error(
        MLMultiArray::alloc(),
        &shape,
        MLMultiArrayDataType::Float16,
    )
    .map_err(|e| runtime(format!("MLMultiArray alloc: {}", nserr(&e))))?;
    let pointer = array.dataPointer().as_ptr() as *mut f16;
    std::ptr::copy_nonoverlapping(data.as_ptr(), pointer, data.len());
    Ok(array)
}

/// Runs `embeds` and `mask` through `model`; returns the `embedding` output array.
unsafe fn predict(
    model: &MLModel,
    embeds: &Retained<MLMultiArray>,
    mask: &Retained<MLMultiArray>,
) -> Result<Retained<MLMultiArray>, EmbedError> {
    let keys = [
        NSString::from_str(INPUT_EMBEDS),
        NSString::from_str(INPUT_MASK),
    ];
    let values = [
        MLFeatureValue::featureValueWithMultiArray(embeds),
        MLFeatureValue::featureValueWithMultiArray(mask),
    ];
    let key_refs: Vec<&NSString> = keys.iter().map(|k| k.as_ref()).collect();
    let value_refs: Vec<&MLFeatureValue> = values.iter().map(|v| v.as_ref()).collect();
    let dictionary: Retained<NSDictionary<NSString, MLFeatureValue>> =
        NSDictionary::from_slices(&key_refs, &value_refs);
    let provider = MLDictionaryFeatureProvider::initWithDictionary_error(
        MLDictionaryFeatureProvider::alloc(),
        // SAFETY: NSDictionary<NSString, MLFeatureValue> is layout-identical to
        // NSDictionary<NSString, AnyObject> (MLFeatureValue: NSObject); same cast as rightkit-asr.
        std::mem::transmute::<
            &NSDictionary<NSString, MLFeatureValue>,
            &NSDictionary<NSString, AnyObject>,
        >(&dictionary),
    )
    .map_err(|e| runtime(format!("feature provider: {}", nserr(&e))))?;
    let provider_object: &ProtocolObject<dyn MLFeatureProvider> =
        ProtocolObject::from_ref(&*provider);
    let result = objc2::exception::catch(AssertUnwindSafe(|| {
        model.predictionFromFeatures_error(provider_object)
    }))
    .map_err(|_| runtime("exception during prediction"))?
    .map_err(|e| runtime(format!("predict: {}", nserr(&e))))?;
    result
        .featureValueForName(&NSString::from_str(OUTPUT_NAME))
        .ok_or_else(|| runtime(format!("output `{OUTPUT_NAME}` missing")))?
        .multiArrayValue()
        .ok_or_else(|| runtime(format!("output `{OUTPUT_NAME}` is not a multiarray")))
}

/// Gathers `data` (laid out by `shape` and non-negative element `strides`) into C order.
/// Core ML outputs are not guaranteed contiguous, so every read goes through the strides.
pub(crate) fn gather_strided<T: Copy>(
    data: &[T],
    shape: &[usize],
    strides: &[usize],
) -> Result<Vec<T>, String> {
    if shape.len() != strides.len() {
        return Err("shape and strides differ in rank".into());
    }
    let total: usize = shape.iter().product();
    if total == 0 {
        return Ok(Vec::new());
    }
    let reach: usize = shape
        .iter()
        .zip(strides)
        .map(|(dim, stride)| (dim - 1) * stride)
        .sum();
    if reach >= data.len() {
        return Err(format!(
            "strides reach element {reach} of a buffer of {}",
            data.len()
        ));
    }
    let mut out = Vec::with_capacity(total);
    let mut index = vec![0usize; shape.len()];
    for _ in 0..total {
        let offset: usize = index.iter().zip(strides).map(|(i, s)| i * s).sum();
        out.push(data[offset]);
        for axis in (0..shape.len()).rev() {
            index[axis] += 1;
            if index[axis] < shape[axis] {
                break;
            }
            index[axis] = 0;
        }
    }
    Ok(out)
}

/// The output array as f32 in C order, honouring dtype (float32 per the package spec; float16
/// accepted) and strides.
unsafe fn read_output(array: &MLMultiArray) -> Result<Vec<f32>, EmbedError> {
    let dimensions = |numbers: Retained<NSArray<NSNumber>>| -> Result<Vec<usize>, EmbedError> {
        numbers
            .iter()
            .map(|n| {
                usize::try_from(n.integerValue())
                    .map_err(|_| EmbedError::Output("negative shape or stride".into()))
            })
            .collect()
    };
    let shape = dimensions(array.shape())?;
    let strides = dimensions(array.strides())?;
    let reach: usize = shape
        .iter()
        .zip(&strides)
        .map(|(dim, stride)| dim.saturating_sub(1) * stride)
        .sum::<usize>()
        + 1;
    let pointer = array.dataPointer().as_ptr();
    let dtype = array.dataType();
    let output = |e: String| EmbedError::Output(e);
    if dtype == MLMultiArrayDataType::Float32 {
        let data = std::slice::from_raw_parts(pointer as *const f32, reach);
        gather_strided(data, &shape, &strides).map_err(output)
    } else if dtype == MLMultiArrayDataType::Float16 {
        let data = std::slice::from_raw_parts(pointer as *const f16, reach);
        gather_strided(data, &shape, &strides)
            .map(|values| values.into_iter().map(f16::to_f32).collect())
            .map_err(output)
    } else {
        Err(EmbedError::Output(format!(
            "unexpected output element type {}",
            dtype.0
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A table of `rows` rows where row `r` holds bf16 bits `bits(r)` in every column.
    fn small_table(rows: usize, scale: f32, bits: impl Fn(usize) -> u16) -> TokenTable {
        let mut bytes = Vec::with_capacity(rows * HIDDEN * 2);
        for row in 0..rows {
            for _ in 0..HIDDEN {
                bytes.extend(bits(row).to_le_bytes());
            }
        }
        TokenTable::from_bytes(bytes, rows, scale).unwrap()
    }

    /// bf16 bits of a small non-negative integer (exactly representable up to 256).
    fn bf16_of(value: f32) -> u16 {
        (value.to_bits() >> 16) as u16
    }

    #[test]
    fn bucket_is_the_smallest_function_that_fits() {
        assert_eq!(bucket_for(1), Some(32));
        assert_eq!(bucket_for(32), Some(32));
        assert_eq!(bucket_for(33), Some(64));
        assert_eq!(bucket_for(128), Some(128));
        assert_eq!(bucket_for(129), Some(256));
        assert_eq!(bucket_for(257), Some(512));
        assert_eq!(bucket_for(512), Some(512));
        assert_eq!(
            bucket_for(513),
            None,
            "longer texts are truncated by the caller"
        );
        assert_eq!(function_name(256), "embed_256");
        // The package also has embed_48, which the measured contract does not use.
        assert!(!BUCKETS.contains(&48));
    }

    #[test]
    fn bf16_widens_by_a_sixteen_bit_shift() {
        assert_eq!(bf16_to_f32(0x3F80), 1.0);
        assert_eq!(bf16_to_f32(0x4000), 2.0);
        assert_eq!(bf16_to_f32(0xBF80), -1.0);
        assert_eq!(bf16_to_f32(0x0000), 0.0);
        assert_eq!(bf16_to_f32(0x3E80), 0.25);
        assert!(bf16_to_f32(0x7F80).is_infinite());
        assert!(bf16_to_f32(0x7FC0).is_nan());
        assert_eq!(bf16_of(3.0), 0x4040);
    }

    #[test]
    fn rows_are_scaled_in_f32_then_rounded_to_f16() {
        let scale = 22.627417f32;
        let table = small_table(4, scale, |row| bf16_of(row as f32));
        let mut out = Vec::new();
        table.push_row(3, &mut out).unwrap();
        assert_eq!(out.len(), HIDDEN);
        let expected = f16::from_f32(3.0 * scale);
        assert!(out.iter().all(|v| *v == expected));
        // Rounding is f32 -> f16 once (no bf16 rounding of the scaled value).
        assert_ne!(expected.to_f32(), 3.0 * scale);
        assert!((expected.to_f32() - 3.0 * scale).abs() < 0.05);
        assert!(table.push_row(4, &mut out).is_err());
        assert!(table.push_row(-1, &mut out).is_err());
        assert_eq!(out.len(), HIDDEN, "a rejected id appends nothing");
    }

    #[test]
    fn a_row_that_overflows_float16_is_an_error() {
        let table = small_table(1, 22.627417, |_| bf16_of(4096.0));
        assert!(table.push_row(0, &mut Vec::new()).is_err());
    }

    #[test]
    fn table_requires_the_exact_size() {
        assert!(TokenTable::from_bytes(vec![0; HIDDEN * 2 * 3 - 2], 3, 1.0).is_err());
        assert!(TokenTable::from_bytes(vec![0; HIDDEN * 2 * 3], 3, 1.0).is_ok());
        assert!(TokenTable::from_bytes(vec![0; HIDDEN * 2 * 3], VOCAB, 1.0).is_err());
    }

    #[test]
    fn inputs_pad_to_the_bucket_with_pad_rows_and_a_zero_mask() {
        let table = small_table(8, 1.0, |row| bf16_of(row as f32));
        let (embeds, mask) = build_inputs(&table, &[2, 3, 4], 32, 0).unwrap();
        assert_eq!(embeds.len(), 32 * HIDDEN);
        assert_eq!(mask.len(), 32);
        let mask: Vec<f32> = mask.iter().map(|m| m.to_f32()).collect();
        assert_eq!(&mask[..4], &[1.0, 1.0, 1.0, 0.0]);
        assert!(mask[3..].iter().all(|m| *m == 0.0));
        for (position, expected) in [(0usize, 2.0f32), (1, 3.0), (2, 4.0), (3, 0.0), (31, 0.0)] {
            assert_eq!(
                embeds[position * HIDDEN].to_f32(),
                expected,
                "position {position}"
            );
        }
        let (_, short) = build_inputs(&table, &[2], 32, 0).unwrap();
        assert_eq!(short[0], f16::ONE);
    }

    #[test]
    fn cache_key_follows_every_package_hash_and_the_os() {
        let base = |h: &str| {
            vec![
                (ARTIFACTS[0], "m".to_owned()),
                (ARTIFACTS[1], "s".to_owned()),
                (ARTIFACTS[2], h.to_owned()),
            ]
        };
        let key = cache_key(&base("w"), (15, 0, 0));
        assert_eq!(key.len(), 32);
        assert!(key.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(key, cache_key(&base("w"), (15, 0, 0)));
        assert_ne!(key, cache_key(&base("w2"), (15, 0, 0)), "weights changed");
        assert_ne!(key, cache_key(&base("w"), (15, 1, 0)), "OS changed");
        let mut renamed = base("w");
        renamed[0].0 = "other";
        assert_ne!(key, cache_key(&renamed, (15, 0, 0)));
        assert_eq!(PACKAGE_FILES.len(), 3);
        assert!(PACKAGE_FILES.iter().all(|f| f.starts_with(PACKAGE_DIR)));
    }

    #[test]
    fn config_checks_scale_dims_table_and_functions() {
        let good = serde_json::json!({
            "hidden_size": 512,
            "embedding_dim": 768,
            "embed_scale": 22.627417,
            "token_table": {"file": "embeddings.bf16", "dtype": "bfloat16", "shape": [262144, 512]},
            "functions": {
                "embed_32": {"tokens": 32}, "embed_48": {"tokens": 48}, "embed_64": {"tokens": 64},
                "embed_128": {"tokens": 128}, "embed_256": {"tokens": 256}, "embed_512": {"tokens": 512},
                "pack_256": {"tokens": 256, "slots": 8}
            }
        });
        let bytes = |v: &serde_json::Value| serde_json::to_vec(v).unwrap();
        let scale = parse_config(&bytes(&good)).unwrap();
        assert!((scale - 22.627417).abs() < 1e-5);
        let mut bad = good.clone();
        bad["embed_scale"] = serde_json::json!(1.0);
        assert!(parse_config(&bytes(&bad)).is_err());
        let mut bad = good.clone();
        bad["token_table"]["dtype"] = serde_json::json!("float16");
        assert!(parse_config(&bytes(&bad)).is_err());
        let mut bad = good.clone();
        bad["functions"]
            .as_object_mut()
            .unwrap()
            .remove("embed_256");
        assert!(parse_config(&bytes(&bad)).is_err());
        let mut bad = good;
        bad["embedding_dim"] = serde_json::json!(512);
        assert!(parse_config(&bytes(&bad)).is_err());
        assert!(parse_config(b"not json").is_err());
    }

    #[test]
    fn strided_gather_reads_padded_and_contiguous_layouts() {
        // Contiguous [1, 4].
        assert_eq!(
            gather_strided(&[1, 2, 3, 4], &[1, 4], &[4, 1]).unwrap(),
            vec![1, 2, 3, 4]
        );
        // Rows padded to a pitch of 4: [2, 3] with strides [4, 1].
        let padded = [1, 2, 3, 0, 4, 5, 6, 0];
        assert_eq!(
            gather_strided(&padded, &[2, 3], &[4, 1]).unwrap(),
            vec![1, 2, 3, 4, 5, 6]
        );
        // Transposed storage: [2, 3] with strides [1, 2].
        assert_eq!(
            gather_strided(&[1, 4, 2, 5, 3, 6], &[2, 3], &[1, 2]).unwrap(),
            vec![1, 2, 3, 4, 5, 6]
        );
        // Strides that would read past the buffer, and rank mismatch, are errors.
        assert!(gather_strided(&[1, 2, 3], &[1, 4], &[4, 1]).is_err());
        assert!(gather_strided(&[1, 2, 3], &[3], &[1, 1]).is_err());
    }

    #[test]
    fn a_bundle_is_recognised_by_its_layout_only() {
        let dir = std::env::temp_dir().join(format!("rk-coreml-bundle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_bundle(&dir));
        std::fs::write(dir.join(TABLE_FILE), b"x").unwrap();
        assert!(!is_bundle(&dir));
        std::fs::create_dir_all(dir.join(PACKAGE_DIR)).unwrap();
        assert!(is_bundle(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
