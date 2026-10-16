//! EmbeddingGemma 2 E40 bundle: five fixed-length fp16 graphs (32, 64, 128, 256, 512 tokens,
//! batch 1) sharing one weights file, plus the fp16 token-embedding table that is looked up on
//! the host. Export and contract: `scripts/embed-export/eg2-directml/README.md`.
//!
//! Contract of every graph:
//!
//! - input `inputs_embeds` float16 `[1, L, 512]` = `embed_tokens.f16.npy[token_id]`, unscaled;
//! - input `attention_mask` int64 `[1, L]`, 1 on real tokens and 0 on padding;
//! - output `sentence_embedding` float32 `[1, 768]`: mean pooling over the mask, projection and
//!   L2 normalisation all in graph, with an fp32 tail so an all-zero input gives zeros, not NaN.
//!
//! A text runs through the smallest graph whose length is at least its token count. Graphs are
//! built on first use and kept; [`E40Engine::release`] drops them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use rightkit_onnx::half::f16;
use rightkit_onnx::ort;
use rightkit_onnx::ort::session::Session;
use rightkit_onnx::{ExecutionProvider, SessionOptions};

use crate::embedder::{provider_name, EmbedError, SessionCreate};

/// Fixed graph lengths, ascending. Every file is `model_<length>.onnx`.
pub(crate) const BUCKETS: [usize; 5] = [32, 64, 128, 256, 512];
/// Longest text a graph accepts; longer token sequences are truncated by the caller.
pub(crate) const MAX_TOKENS: usize = 512;
/// Width of one token-embedding row.
pub(crate) const HIDDEN: usize = 512;
/// Rows in the token table (the full Gemma vocabulary).
pub(crate) const VOCAB: usize = 262_144;
/// Host-side token table, NumPy `.npy`, little-endian float16, shape `[VOCAB, HIDDEN]`.
pub(crate) const TABLE_FILE: &str = "embed_tokens.f16.npy";
/// Shared fp16 weights of all five graphs (ONNX external data; the graphs reference it by name).
pub(crate) const DATA_FILE: &str = "model.onnx_data";
/// Files that must exist for a directory to be an E40 bundle and that the pipeline id covers.
pub(crate) const ARTIFACTS: [&str; 8] = [
    "model_32.onnx",
    "model_64.onnx",
    "model_128.onnx",
    "model_256.onnx",
    "model_512.onnx",
    DATA_FILE,
    TABLE_FILE,
    "tokenizer.json",
];

/// True when `dir` holds the E40 layout: the token table beside the fixed-length graphs. This
/// is how the bundle is chosen, from the directory and never from a setting.
pub(crate) fn is_bundle(dir: &Path) -> bool {
    dir.join(TABLE_FILE).is_file() && dir.join("model_32.onnx").is_file()
}

/// The smallest graph length that holds `tokens`, or `None` when it exceeds [`MAX_TOKENS`].
pub(crate) fn bucket_for(tokens: usize) -> Option<usize> {
    BUCKETS.iter().copied().find(|bucket| *bucket >= tokens)
}

/// Byte offset of the data and the `(rows, columns)` shape of a `.npy` file that must be
/// little-endian float16 in C order. Accepts format versions 1.0 and 2.0.
pub(crate) fn parse_npy_f16_header(bytes: &[u8]) -> Result<(usize, usize, usize), String> {
    const MAGIC: &[u8] = b"\x93NUMPY";
    if bytes.len() < 12 || &bytes[..6] != MAGIC {
        return Err("not a NumPy .npy file".into());
    }
    let (header_len, start): (usize, usize) = match bytes[6] {
        1 => (usize::from(u16::from_le_bytes([bytes[8], bytes[9]])), 10),
        2 => (
            u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize,
            12,
        ),
        other => return Err(format!("unsupported .npy format version {other}")),
    };
    let end = start
        .checked_add(header_len)
        .filter(|end| *end <= bytes.len())
        .ok_or("truncated .npy header")?;
    let header =
        std::str::from_utf8(&bytes[start..end]).map_err(|_| "non-UTF-8 .npy header".to_owned())?;
    if !header.contains("'descr': '<f2'") {
        return Err(format!(
            "expected little-endian float16 data, header is {header:?}"
        ));
    }
    if !header.contains("'fortran_order': False") {
        return Err("expected C-order data".into());
    }
    let shape = header
        .split("'shape': (")
        .nth(1)
        .and_then(|rest| rest.split(')').next())
        .ok_or("missing .npy shape")?;
    let dims: Vec<usize> = shape
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::parse)
        .collect::<Result<_, _>>()
        .map_err(|_| format!("unreadable .npy shape ({shape})"))?;
    let [rows, columns] = dims[..] else {
        return Err(format!("expected a 2-D table, shape is ({shape})"));
    };
    Ok((end, rows, columns))
}

/// The host-side fp16 token-embedding table, kept as the file's little-endian bytes (268 MB)
/// and converted per looked-up row.
pub(crate) struct TokenTable {
    bytes: Vec<u8>,
    offset: usize,
    rows: usize,
}

impl TokenTable {
    pub(crate) fn load(path: &Path) -> Result<Self, EmbedError> {
        let bytes = std::fs::read(path).map_err(|e| {
            EmbedError::InvalidConfig(format!("token table {}: {e}", path.display()))
        })?;
        Self::from_bytes(bytes)
            .map_err(|e| EmbedError::InvalidConfig(format!("token table {}: {e}", path.display())))
    }

    pub(crate) fn from_bytes(bytes: Vec<u8>) -> Result<Self, String> {
        let (offset, rows, columns) = parse_npy_f16_header(&bytes)?;
        if columns != HIDDEN {
            return Err(format!("expected {HIDDEN} columns, found {columns}"));
        }
        if rows != VOCAB {
            return Err(format!("expected {VOCAB} rows, found {rows}"));
        }
        let expected = rows * columns * 2;
        if bytes.len() - offset != expected {
            return Err(format!(
                "expected {expected} data bytes, found {}",
                bytes.len() - offset
            ));
        }
        Ok(Self {
            bytes,
            offset,
            rows,
        })
    }

    /// Appends the embedding row of `id` (unscaled fp16) to `out`.
    pub(crate) fn push_row(&self, id: i64, out: &mut Vec<f16>) -> Result<(), EmbedError> {
        let row = usize::try_from(id)
            .ok()
            .filter(|row| *row < self.rows)
            .ok_or_else(|| {
                EmbedError::Tokenizer(format!("token id {id} is outside the token table"))
            })?;
        let start = self.offset + row * HIDDEN * 2;
        out.extend(
            self.bytes[start..start + HIDDEN * 2]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| f16::from_bits(u16::from_le_bytes([pair[0], pair[1]]))),
        );
        Ok(())
    }
}

/// `inputs_embeds` (`bucket * HIDDEN` values) and the int64 mask for `ids` padded to `bucket`
/// with `pad_id` rows and a zero mask.
pub(crate) fn build_inputs(
    table: &TokenTable,
    ids: &[i64],
    bucket: usize,
    pad_id: i64,
) -> Result<(Vec<f16>, Vec<i64>), EmbedError> {
    let mut embeds = Vec::with_capacity(bucket * HIDDEN);
    let mut mask = vec![0i64; bucket];
    for (position, flag) in mask.iter_mut().enumerate() {
        match ids.get(position) {
            Some(id) => {
                table.push_row(*id, &mut embeds)?;
                *flag = 1;
            }
            None => table.push_row(pad_id, &mut embeds)?,
        }
    }
    Ok((embeds, mask))
}

/// Graph builds during `Embedder::load`, shared by an engine and its provider twins. Recording
/// stops when the load finishes, so graphs rebuilt later on first use are not logged.
struct SessionLog {
    recording: AtomicBool,
    entries: Mutex<Vec<SessionCreate>>,
}

impl SessionLog {
    fn new() -> Self {
        Self {
            recording: AtomicBool::new(true),
            entries: Mutex::new(Vec::new()),
        }
    }

    fn push(&self, entry: SessionCreate) {
        if self.recording.load(Ordering::Relaxed) {
            if let Ok(mut entries) = self.entries.lock() {
                entries.push(entry);
            }
        }
    }

    fn finish(&self) -> Vec<SessionCreate> {
        self.recording.store(false, Ordering::Relaxed);
        self.entries
            .lock()
            .map(|mut entries| std::mem::take(&mut *entries))
            .unwrap_or_default()
    }
}

/// Where one `E40Engine::run_split` call spent its time. Phases run in this order; `total`
/// covers all of them.
#[cfg_attr(not(feature = "bench-internals"), allow(dead_code))]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunSplit {
    /// Graph length chosen for the text.
    pub bucket: usize,
    /// Token count of the text.
    pub tokens: usize,
    /// Token-table lookup and padding (`build_inputs`).
    pub build_inputs: Duration,
    /// Input tensor creation and run options.
    pub tensors: Duration,
    /// Session mutex and map lookup (includes a graph build when `built_session`).
    pub session_lookup: Duration,
    /// `Session::run_with_options` alone.
    pub session_run: Duration,
    /// Output tensor extraction, copy to a `Vec` and output release.
    pub extract: Duration,
    /// The whole call.
    pub total: Duration,
    /// True when this call had to build the graph.
    pub built_session: bool,
}

/// ONNX Runtime session options of the E40 graphs. The default (what
/// [`E40SessionProfile::shipped`] returns) is the measured production configuration; the other
/// values exist for the option sweep (`bench-internals` selects one by name). Only what
/// `rightkit-onnx` exposes can be varied here: graph optimisation level and inter-op threads
/// stay at the ORT defaults (all, and unused in sequential mode).
#[cfg_attr(not(feature = "bench-internals"), allow(dead_code))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct E40SessionProfile {
    /// Intra-op threads; `None` leaves the ORT default.
    pub intra_threads: Option<usize>,
    /// DirectML: apply `with_parallel_execution(false)` (sequential execution).
    pub sequential: bool,
    /// DirectML: apply `with_memory_pattern(false)` (memory pattern off).
    pub disable_memory_pattern: bool,
}

#[cfg_attr(not(feature = "bench-internals"), allow(dead_code))]
impl E40SessionProfile {
    /// The production options: `intra_threads` threads, sequential, memory pattern off.
    pub fn shipped(intra_threads: usize) -> Self {
        Self {
            intra_threads: Some(intra_threads),
            sequential: true,
            disable_memory_pattern: true,
        }
    }

    /// The options `eg2run.py` ran: ORT default threads, sequential, memory pattern left on
    /// (ORT's DirectML provider overrides it to off; see the matrix notes).
    pub fn eg2run() -> Self {
        Self {
            intra_threads: None,
            sequential: true,
            disable_memory_pattern: false,
        }
    }

    /// Parses a profile spec: `rust` / `shipped`, `eg2run`, or a comma list of overrides applied
    /// to the shipped profile: `intra=<N>` (`0` or `default` = ORT default), `mem=on|off`,
    /// `seq=on|off`. Example: `intra=0,mem=on`. An empty spec is the shipped profile.
    pub fn parse(spec: &str) -> Result<Self, String> {
        let spec = spec.trim();
        let mut profile = Self::shipped(1);
        match spec {
            "" | "rust" | "shipped" => return Ok(profile),
            "eg2run" => return Ok(Self::eg2run()),
            _ => {}
        }
        for part in spec.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            let (key, value) = part
                .split_once('=')
                .ok_or_else(|| format!("expected key=value, got {part:?}"))?;
            let on_off = |value: &str| match value {
                "on" => Ok(true),
                "off" => Ok(false),
                other => Err(format!("{key}: expected on or off, got {other:?}")),
            };
            match key {
                "intra" => {
                    profile.intra_threads = match value {
                        "default" | "0" => None,
                        n => Some(
                            n.parse::<usize>()
                                .map_err(|_| format!("intra: expected a number, got {n:?}"))?,
                        ),
                    }
                }
                // `mem=on` is memory pattern enabled, i.e. the disable flag is false.
                "mem" => profile.disable_memory_pattern = !on_off(value)?,
                "seq" => profile.sequential = on_off(value)?,
                other => return Err(format!("unknown session option {other:?}")),
            }
        }
        Ok(profile)
    }
}

/// The five graphs of one bundle on one provider, with the shared token table.
pub(crate) struct E40Engine {
    dir: PathBuf,
    table: Arc<TokenTable>,
    token_table_read: Duration,
    provider: ExecutionProvider,
    profile: E40SessionProfile,
    sessions: Mutex<BTreeMap<usize, Session>>,
    log: Arc<SessionLog>,
}

impl E40Engine {
    /// Reads the token table and prepares an engine; no graph is built yet.
    pub(crate) fn open(
        dir: &Path,
        provider: ExecutionProvider,
        profile: E40SessionProfile,
    ) -> Result<Self, EmbedError> {
        let started = Instant::now();
        let table = Arc::new(TokenTable::load(&dir.join(TABLE_FILE))?);
        Ok(Self {
            dir: dir.to_path_buf(),
            table,
            token_table_read: started.elapsed(),
            provider,
            profile,
            sessions: Mutex::new(BTreeMap::new()),
            log: Arc::new(SessionLog::new()),
        })
    }

    /// An engine over the same bundle and the same token table (not read again) on `provider`.
    /// It shares the load-time session log with `self`.
    pub(crate) fn with_provider(&self, provider: ExecutionProvider) -> Self {
        Self {
            dir: self.dir.clone(),
            table: Arc::clone(&self.table),
            token_table_read: self.token_table_read,
            provider,
            profile: self.profile,
            sessions: Mutex::new(BTreeMap::new()),
            log: Arc::clone(&self.log),
        }
    }

    /// How long reading and validating the token table took in `open`.
    pub(crate) fn token_table_read(&self) -> Duration {
        self.token_table_read
    }

    /// The graph builds logged since `open` (this engine and its twins), in build order. Stops
    /// recording: call it once, when the load is over.
    pub(crate) fn finish_session_log(&self) -> Vec<SessionCreate> {
        self.log.finish()
    }

    pub(crate) fn provider(&self) -> ExecutionProvider {
        self.provider
    }

    /// Drops every built graph; the next text rebuilds the one it needs.
    pub(crate) fn release(&self) {
        if let Ok(mut sessions) = self.sessions.lock() {
            sessions.clear();
        }
    }

    /// Native-dimension (768) vector for one tokenized text, as the graph returns it
    /// (already L2-normalised in graph).
    pub(crate) fn run(&self, ids: &[i64], pad_id: i64) -> Result<Vec<f32>, EmbedError> {
        self.run_split(ids, pad_id).map(|(vector, _)| vector)
    }

    /// [`Self::run`] with a per-phase timing of the same code path (a few clock reads, no
    /// extra work), so the benchmark measures exactly what production runs.
    pub(crate) fn run_split(
        &self,
        ids: &[i64],
        pad_id: i64,
    ) -> Result<(Vec<f32>, RunSplit), EmbedError> {
        let bucket = bucket_for(ids.len()).ok_or_else(|| {
            EmbedError::Output(format!(
                "{} tokens exceed the longest E40 graph ({MAX_TOKENS})",
                ids.len()
            ))
        })?;
        self.run_split_in(ids, pad_id, bucket)
    }

    /// Builds every graph that is not built yet and runs one dummy text (a single `pad_id`
    /// token) through each, so the first real query per bucket pays neither a session build nor
    /// the provider's first-run cost. Returns the time it took. Any build or run error is
    /// returned: a graph that cannot serve here must not be admitted.
    pub(crate) fn prewarm(&self, pad_id: i64) -> Result<Duration, EmbedError> {
        let started = Instant::now();
        for bucket in BUCKETS {
            self.run_split_in(&[pad_id], pad_id, bucket)?;
        }
        Ok(started.elapsed())
    }

    /// [`Self::run_split`] on the graph of length `bucket` (which must hold `ids`).
    fn run_split_in(
        &self,
        ids: &[i64],
        pad_id: i64,
        bucket: usize,
    ) -> Result<(Vec<f32>, RunSplit), EmbedError> {
        let started = Instant::now();
        let (embeds, mask) = build_inputs(&self.table, ids, bucket, pad_id)?;
        let build_inputs_time = started.elapsed();

        let phase = Instant::now();
        let runtime = |e: &dyn std::fmt::Display| EmbedError::Runtime(e.to_string());
        let embeds = ort::value::Tensor::from_array(([1i64, bucket as i64, HIDDEN as i64], embeds))
            .map_err(|e| runtime(&e))?;
        let mask = ort::value::Tensor::from_array(([1i64, bucket as i64], mask))
            .map_err(|e| runtime(&e))?;
        let options = ort::session::RunOptions::new()
            .map_err(|e| runtime(&e))?
            .with_outputs(ort::session::OutputSelector::no_default().with("sentence_embedding"));
        let tensors_time = phase.elapsed();

        let phase = Instant::now();
        let mut built_session = false;
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| EmbedError::Runtime("model mutex poisoned".into()))?;
        let session = match sessions.entry(bucket) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                built_session = true;
                entry.insert(self.build_session(bucket)?)
            }
        };
        let session_lookup_time = phase.elapsed();

        let phase = Instant::now();
        let outputs = session
            .run_with_options(
                ort::inputs!["inputs_embeds" => embeds, "attention_mask" => mask],
                &options,
            )
            .map_err(|e| runtime(&e))?;
        let session_run_time = phase.elapsed();

        let phase = Instant::now();
        let output = outputs
            .get("sentence_embedding")
            .ok_or_else(|| EmbedError::Output("missing sentence_embedding output".into()))?;
        let (_, values) = output
            .try_extract_tensor::<f32>()
            .map_err(|e| EmbedError::Output(e.to_string()))?;
        let vector = values.to_vec();
        drop(outputs);
        let extract_time = phase.elapsed();

        Ok((
            vector,
            RunSplit {
                bucket,
                tokens: ids.len(),
                build_inputs: build_inputs_time,
                tensors: tensors_time,
                session_lookup: session_lookup_time,
                session_run: session_run_time,
                extract: extract_time,
                total: started.elapsed(),
                built_session,
            },
        ))
    }

    /// One graph on this engine's provider. The CPU fallback of `SessionOptions` is never used
    /// here: a graph that cannot be built on the requested provider is an error, so the
    /// provider the embedder reports is the provider that ran. The build time is logged for
    /// `Embedder::load_timings`.
    fn build_session(&self, bucket: usize) -> Result<Session, EmbedError> {
        let started = Instant::now();
        let session = SessionOptions::new(self.provider)
            .with_threads(self.profile.intra_threads)
            .with_cpu_fallback(false)
            // Default: the measured configuration of the DirectML runs in the export README.
            .with_directml_sequential(self.profile.sequential)
            .with_directml_memory_pattern(self.profile.disable_memory_pattern)
            .build_from_file(&self.dir.join(format!("model_{bucket}.onnx")))
            .map_err(|e| EmbedError::Runtime(e.to_string()))?;
        self.log.push(SessionCreate {
            provider: provider_name(self.provider),
            graph_tokens: bucket,
            duration: started.elapsed(),
        });
        Ok(session)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.npy` v1.0 image with the given shape string and `data_bytes` of zero payload.
    fn npy(descr: &str, order: &str, shape: &str, data_bytes: usize) -> Vec<u8> {
        let mut header =
            format!("{{'descr': '{descr}', 'fortran_order': {order}, 'shape': ({shape}), }}");
        // NumPy pads the header with spaces and a newline to a multiple of 64 (data aligned).
        while (10 + header.len() + 1) % 64 != 0 {
            header.push(' ');
        }
        header.push('\n');
        let mut out = b"\x93NUMPY\x01\x00".to_vec();
        out.extend((header.len() as u16).to_le_bytes());
        out.extend(header.as_bytes());
        out.extend(vec![0u8; data_bytes]);
        out
    }

    #[test]
    fn bucket_is_the_smallest_graph_that_fits() {
        assert_eq!(bucket_for(1), Some(32));
        assert_eq!(bucket_for(32), Some(32));
        assert_eq!(bucket_for(33), Some(64));
        assert_eq!(bucket_for(64), Some(64));
        assert_eq!(bucket_for(65), Some(128));
        assert_eq!(bucket_for(129), Some(256));
        assert_eq!(bucket_for(257), Some(512));
        assert_eq!(bucket_for(512), Some(512));
        assert_eq!(bucket_for(513), None);
    }

    #[test]
    fn npy_header_of_the_real_table_shape_parses() {
        // The real file is 268,435,584 bytes: a 128-byte header and 262144 * 512 * 2 of data.
        let image = npy("<f2", "False", "262144, 512", 0);
        assert_eq!(image.len() % 64, 0);
        let (offset, rows, columns) = parse_npy_f16_header(&image).unwrap();
        assert_eq!((rows, columns), (VOCAB, HIDDEN));
        assert_eq!(offset, image.len());
    }

    #[test]
    fn npy_header_rejects_other_dtypes_orders_shapes_and_junk() {
        assert!(parse_npy_f16_header(&npy(">f2", "False", "4, 512", 0)).is_err());
        assert!(parse_npy_f16_header(&npy("<f4", "False", "4, 512", 0)).is_err());
        assert!(parse_npy_f16_header(&npy("<f2", "True", "4, 512", 0)).is_err());
        assert!(parse_npy_f16_header(&npy("<f2", "False", "4,", 0)).is_err());
        assert!(parse_npy_f16_header(&npy("<f2", "False", "4, 5, 6", 0)).is_err());
        assert!(parse_npy_f16_header(b"not an npy file at all").is_err());
        let mut truncated = npy("<f2", "False", "4, 512", 0);
        truncated.truncate(20);
        assert!(parse_npy_f16_header(&truncated).is_err());
    }

    #[test]
    fn table_requires_the_exact_vocabulary_width_and_length() {
        assert!(TokenTable::from_bytes(npy("<f2", "False", "4, 512", 4 * 512 * 2)).is_err());
        assert!(TokenTable::from_bytes(npy("<f2", "False", "262144, 256", 0)).is_err());
        // Right header, payload one byte short.
        let short = npy("<f2", "False", "262144, 512", VOCAB * HIDDEN * 2 - 1);
        assert!(TokenTable::from_bytes(short).is_err());
    }

    /// A tiny table that bypasses the vocabulary-size check: row `r` holds `r` in every column.
    fn small_table(rows: usize) -> TokenTable {
        let mut image = npy("<f2", "False", &format!("{rows}, {HIDDEN}"), 0);
        let offset = image.len();
        for row in 0..rows {
            let bits = f16::from_f32(row as f32).to_bits().to_le_bytes();
            for _ in 0..HIDDEN {
                image.extend(bits);
            }
        }
        TokenTable {
            bytes: image,
            offset,
            rows,
        }
    }

    #[test]
    fn row_lookup_returns_the_row_and_rejects_ids_outside_the_table() {
        let table = small_table(8);
        let mut out = Vec::new();
        table.push_row(5, &mut out).unwrap();
        assert_eq!(out.len(), HIDDEN);
        assert!(out.iter().all(|v| v.to_f32() == 5.0));
        assert!(table.push_row(8, &mut out).is_err());
        assert!(table.push_row(-1, &mut out).is_err());
        assert_eq!(out.len(), HIDDEN, "a rejected id appends nothing");
    }

    #[test]
    fn inputs_pad_to_the_bucket_with_pad_rows_and_a_zero_mask() {
        let table = small_table(8);
        let (embeds, mask) = build_inputs(&table, &[2, 3, 4], 32, 0).unwrap();
        assert_eq!(embeds.len(), 32 * HIDDEN);
        assert_eq!(mask.len(), 32);
        assert_eq!(&mask[..4], &[1, 1, 1, 0]);
        assert!(mask[3..].iter().all(|m| *m == 0));
        for (position, expected) in [(0usize, 2.0f32), (1, 3.0), (2, 4.0), (3, 0.0), (31, 0.0)] {
            assert_eq!(
                embeds[position * HIDDEN].to_f32(),
                expected,
                "position {position}"
            );
        }
    }

    #[test]
    fn a_bundle_is_recognised_by_its_files_only() {
        let dir = std::env::temp_dir().join(format!("rk-e40-bundle-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(!is_bundle(&dir));
        std::fs::write(dir.join(TABLE_FILE), b"x").unwrap();
        assert!(!is_bundle(&dir));
        std::fs::write(dir.join("model_32.onnx"), b"x").unwrap();
        assert!(is_bundle(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
