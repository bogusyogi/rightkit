//! EmbeddingGemma 2 E40 bundle: static fp16 graphs sharing ONE weights file (`model.onnx_data`,
//! never copied), plus the token-embedding table that is looked up on the host. Export and
//! contract: `scripts/embed-export/eg2-directml/README.md`.
//!
//! A bundle with a top-level `embed-manifest.json` (or, in older bundles, `manifest.json`; the
//! first wins when both exist) (schema 2) names everything: `graphs.query` (batch 1,
//! ascending lengths, a `file` template with `{L}`), `graphs.ingest` (entries of batch 4 and 8,
//! each with its own lengths and template, e.g. `ingest-b8_{L}.onnx`), `weights` and `token_table`
//! (`int8-rowwise` with `table` and `scales`, or `fp16`; a missing `token_table` is fp16). It is
//! parsed strictly: an unknown schema, a malformed entry, an unknown table format, a missing file
//! or an ingest batch other than 4 or 8 (16 is the typed refusal) fails the load. A bundle with no
//! manifest is the legacy layout: `model_<L>.onnx` for 32, 64, 128, 256, 512, the fp16 table, no
//! ingest graphs.
//!
//! Contract of every graph:
//!
//! - input `inputs_embeds` float16 `[B, L, 512]` = the table row of each token id, unscaled
//!   (int8 rows are dequantised at lookup, `fp16 = i8 * scale`);
//! - input `attention_mask` int64 `[B, L]`, 1 on real tokens and 0 on padding;
//! - output `sentence_embedding` float32 `[B, 768]`: mean pooling over the mask, projection and
//!   L2 normalisation all in graph, with an fp32 tail so an all-zero input gives zeros, not NaN.
//!
//! A query runs through the smallest batch-1 graph whose length is at least its token count.
//! `Embedder::embed_batch` on DirectML sorts documents by length and takes chunks of the
//! remaining ones: batch 8 for five or more, batch 4 for two to four, the batch-1 graph for one
//! (only batches the bundle ships and the configured cap allows). A chunk runs on the smallest
//! length that holds its longest text, is filled up to the batch with rows of attention mask 0,
//! and those rows are dropped; input order is kept. Graphs are built on first use and kept;
//! [`E40Engine::release`] drops them.

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
/// Width of one graph output row (the native embedding).
const OUTPUT_DIM: usize = 768;
/// Rows in the token table (the full Gemma vocabulary).
pub(crate) const VOCAB: usize = 262_144;
/// Host-side fp16 token table, NumPy `.npy`, little-endian float16, shape `[VOCAB, HIDDEN]`.
pub(crate) const TABLE_FILE: &str = "embed_tokens.f16.npy";
/// Host-side int8 token table, NumPy `.npy`, int8, shape `[VOCAB, HIDDEN]`.
pub(crate) const TABLE_I8_FILE: &str = "embed_tokens.i8.npy";
/// Default name of the shared fp16 weights (ONNX external data) of a legacy bundle; a manifest names it.
pub(crate) const DATA_FILE: &str = "model.onnx_data";
/// Bundle manifest (schema 2): graph files, shared weights and token table. The name of older
/// bundles and of the adopt-only flat directories.
pub(crate) const MANIFEST_FILE: &str = "manifest.json";
/// The same schema-2 bundle manifest under the name the suite-store-installed E43 bundle uses:
/// the store writes its own entry record to `manifest.json` in every entry directory, which
/// would replace a bundle manifest of that name (bogusyogi/rightkit#224). Preferred over
/// [`MANIFEST_FILE`] when both exist.
pub(crate) const EMBED_MANIFEST_FILE: &str = "embed-manifest.json";

/// The bundle manifest present in `dir`: [`EMBED_MANIFEST_FILE`] when it exists, else
/// [`MANIFEST_FILE`], else `None` (the legacy layout). Both may exist in a store entry, where
/// `manifest.json` is the store's own record, and the embed manifest then wins.
pub(crate) fn manifest_name_in(dir: &Path) -> Option<&'static str> {
    [EMBED_MANIFEST_FILE, MANIFEST_FILE]
        .into_iter()
        .find(|name| dir.join(name).is_file())
}

/// The name a bundle file contributes to the pipeline id: the two manifest names are the same
/// file, so a rename never changes the id of the same bytes.
pub(crate) fn pipeline_name(name: &str) -> &str {
    if name == EMBED_MANIFEST_FILE {
        MANIFEST_FILE
    } else {
        name
    }
}
/// Default ingest batch: 176 docs/s against 166 (batch 4) and 100 (batch 1), measured.
pub(crate) const DEFAULT_INGEST_BATCH: usize = 8;

/// Refuses an ingest batch other than 4 or 8, with a typed error. 16 is named: it measured 64
/// docs/s on an 8 GB card, below batch 1.
pub(crate) fn check_ingest_batch(batch: usize) -> Result<usize, EmbedError> {
    match batch {
        4 | 8 => Ok(batch),
        16 => Err(EmbedError::UnsupportedIngestBatch {
            requested: batch,
            reason: "batch 16 regresses on 8 GB VRAM cards (64 docs/s against 176 at batch 8); use 4 or 8",
        }),
        _ => Err(EmbedError::UnsupportedIngestBatch {
            requested: batch,
            reason: "ingest graphs exist for batch 4 and 8 only",
        }),
    }
}

/// The only manifest schema version this crate reads.
pub(crate) const MANIFEST_SCHEMA: u64 = 2;

/// Which token table a bundle ships, with its file names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TableSpec {
    /// fp16 `[VOCAB, HIDDEN]`.
    F16 { table: String },
    /// int8 `[VOCAB, HIDDEN]` with fp16 per-row scales `[VOCAB, 1]`.
    Int8 { table: String, scales: String },
}

/// One family of static graphs of one batch size: file names are `file` with `{L}` replaced by
/// each length.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GraphSet {
    pub(crate) batch: usize,
    /// Ascending graph lengths.
    pub(crate) lengths: Vec<usize>,
    /// File name template containing `{L}`.
    pub(crate) file: String,
}

impl GraphSet {
    /// File name of the graph of length `length`.
    pub(crate) fn file_name(&self, length: usize) -> String {
        self.file.replace("{L}", &length.to_string())
    }

    /// Every file of the set, in length order.
    pub(crate) fn file_names(&self) -> Vec<String> {
        self.lengths.iter().map(|l| self.file_name(*l)).collect()
    }
}

/// What a bundle directory holds: the batch-1 query graphs, the optional ingest graphs, the
/// shared weights file and the token table. Read from the bundle's `embed-manifest.json` or
/// `manifest.json` (schema 2), or the legacy fixed layout when there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Layout {
    /// True when read from a manifest; the manifest then belongs to the pipeline fingerprint.
    pub(crate) from_manifest: bool,
    /// Which manifest file it was read from (`manifest.json` for the legacy layout, unused).
    pub(crate) manifest_file: &'static str,
    pub(crate) weights: String,
    pub(crate) query: GraphSet,
    pub(crate) ingest: Vec<GraphSet>,
    pub(crate) table: TableSpec,
}

fn bad(manifest: &str) -> EmbedError {
    EmbedError::InvalidConfig(format!("{MANIFEST_FILE}: {manifest}"))
}

/// A file name from a manifest: one path component, no separators, nothing hidden or empty.
fn file_name_of(value: &serde_json::Value, what: &str) -> Result<String, EmbedError> {
    let name = value
        .as_str()
        .ok_or_else(|| bad(&format!("{what} must be a string")))?;
    if name.is_empty()
        || name.starts_with('.')
        || name.contains(['/', '\\', ':'])
        || name.contains("..")
    {
        return Err(bad(&format!("{what} is not a plain file name: {name:?}")));
    }
    Ok(name.to_owned())
}

fn parse_graph_set(
    value: &serde_json::Value,
    what: &str,
    batches: &[usize],
) -> Result<GraphSet, EmbedError> {
    let batch = value
        .get("batch")
        .and_then(|b| b.as_u64())
        .ok_or_else(|| bad(&format!("{what} has no batch")))? as usize;
    if !batches.contains(&batch) {
        if batches != [1] {
            check_ingest_batch(batch)?;
        }
        return Err(bad(&format!(
            "{what} has batch {batch}, expected {batches:?}"
        )));
    }
    let lengths: Vec<usize> = value
        .get("lengths")
        .and_then(|l| l.as_array())
        .ok_or_else(|| bad(&format!("{what} has no lengths")))?
        .iter()
        .map(|l| {
            l.as_u64()
                .map(|l| l as usize)
                .ok_or_else(|| bad(&format!("{what} has a non-numeric length")))
        })
        .collect::<Result<_, _>>()?;
    if lengths.is_empty() || lengths.windows(2).any(|w| w[0] >= w[1]) {
        return Err(bad(&format!(
            "{what} lengths must be ascending and non-empty: {lengths:?}"
        )));
    }
    if lengths.iter().any(|l| *l == 0 || *l > MAX_TOKENS) {
        return Err(bad(&format!(
            "{what} lengths must be 1..={MAX_TOKENS}: {lengths:?}"
        )));
    }
    let file = file_name_of(
        value
            .get("file")
            .ok_or_else(|| bad(&format!("{what} has no file template")))?,
        &format!("{what} file"),
    )?;
    if file.matches("{L}").count() != 1 {
        return Err(bad(&format!(
            "{what} file template must contain {{L}} once: {file:?}"
        )));
    }
    Ok(GraphSet {
        batch,
        lengths,
        file,
    })
}

impl Layout {
    /// The layout of a bundle without a manifest: `model_<L>.onnx` for the five fixed lengths,
    /// the fp16 table, no ingest graphs.
    pub(crate) fn legacy() -> Self {
        Self {
            from_manifest: false,
            manifest_file: MANIFEST_FILE,
            weights: DATA_FILE.to_owned(),
            query: GraphSet {
                batch: 1,
                lengths: BUCKETS.to_vec(),
                file: "model_{L}.onnx".to_owned(),
            },
            ingest: Vec::new(),
            table: TableSpec::F16 {
                table: TABLE_FILE.to_owned(),
            },
        }
    }

    /// Parses a schema-2 manifest strictly: an unknown or missing `schema`, a missing or
    /// malformed `graphs.query` (batch 1, ascending lengths ending at 512, a `{L}` template),
    /// an ingest entry that is not batch 4 or 8 (16 is the typed refusal), a duplicate batch, an
    /// unknown token-table format or a `dtype_out` other than float16 is an error. A missing
    /// `token_table`, or `{"format": "fp16"}`, is the fp16 table.
    pub(crate) fn parse(json: &str) -> Result<Self, EmbedError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| bad(&format!("unreadable manifest: {e}")))?;
        match value.get("schema").and_then(|s| s.as_u64()) {
            Some(MANIFEST_SCHEMA) => {}
            Some(other) => return Err(bad(&format!("unsupported schema version {other}"))),
            None => return Err(bad("missing or non-numeric schema version")),
        }
        let weights = file_name_of(
            value
                .get("weights")
                .ok_or_else(|| bad("missing weights file"))?,
            "weights",
        )?;
        let graphs = value.get("graphs").ok_or_else(|| bad("missing graphs"))?;
        let query = parse_graph_set(
            graphs
                .get("query")
                .ok_or_else(|| bad("missing graphs.query"))?,
            "graphs.query",
            &[1],
        )?;
        if query.lengths.last() != Some(&MAX_TOKENS) {
            return Err(bad(&format!(
                "graphs.query must reach {MAX_TOKENS} tokens: {:?}",
                query.lengths
            )));
        }
        let mut ingest: Vec<GraphSet> = Vec::new();
        if let Some(entries) = graphs.get("ingest") {
            let entries = entries
                .as_array()
                .ok_or_else(|| bad("graphs.ingest must be an array"))?;
            for entry in entries {
                let set = parse_graph_set(entry, "graphs.ingest entry", &[4, 8, 16])?;
                check_ingest_batch(set.batch)?;
                if ingest.iter().any(|other| other.batch == set.batch) {
                    return Err(bad(&format!("duplicate ingest batch {}", set.batch)));
                }
                ingest.push(set);
            }
            ingest.sort_by_key(|set| set.batch);
        }
        let table = match value.get("token_table") {
            None => TableSpec::F16 {
                table: TABLE_FILE.to_owned(),
            },
            Some(spec) => {
                if let Some(out) = spec.get("dtype_out") {
                    if out.as_str() != Some("float16") {
                        return Err(bad(&format!(
                            "token_table.dtype_out must be float16: {out}"
                        )));
                    }
                }
                let name = |key: &str, default: Option<&str>| match spec.get(key) {
                    Some(v) => file_name_of(v, &format!("token_table.{key}")),
                    None => default
                        .map(str::to_owned)
                        .ok_or_else(|| bad(&format!("token_table has no {key}"))),
                };
                match spec.get("format").and_then(|f| f.as_str()) {
                    Some("fp16") => TableSpec::F16 {
                        table: name("table", Some(TABLE_FILE))?,
                    },
                    Some("int8-rowwise") => TableSpec::Int8 {
                        table: name("table", None)?,
                        scales: name("scales", None)?,
                    },
                    other => {
                        return Err(bad(&format!("unknown token_table.format {other:?}")));
                    }
                }
            }
        };
        Ok(Self {
            from_manifest: true,
            manifest_file: MANIFEST_FILE,
            weights,
            query,
            ingest,
            table,
        })
    }

    /// The layout of the bundle in `dir`: its `embed-manifest.json`, else its `manifest.json`,
    /// or the legacy layout when there is neither. Every file the layout names, the manifest
    /// that was read included, must exist.
    pub(crate) fn load(dir: &Path) -> Result<Self, EmbedError> {
        let layout = match manifest_name_in(dir) {
            Some(name) => {
                let path = dir.join(name);
                let json = std::fs::read_to_string(&path)
                    .map_err(|e| EmbedError::InvalidConfig(format!("{}: {e}", path.display())))?;
                let mut layout = Self::parse(&json)?;
                layout.manifest_file = name;
                layout
            }
            None => Self::legacy(),
        };
        for name in layout.files() {
            let file = dir.join(&name);
            if !file.is_file() {
                return Err(EmbedError::MissingArtifact(file));
            }
        }
        Ok(layout)
    }

    /// The ingest graphs with batch at most `cap`, ascending by batch.
    pub(crate) fn ingest_up_to(&self, cap: usize) -> Vec<&GraphSet> {
        self.ingest.iter().filter(|set| set.batch <= cap).collect()
    }

    /// The graph set of `batch` (1 is the query set).
    pub(crate) fn graphs(&self, batch: usize) -> Option<&GraphSet> {
        if batch == 1 {
            Some(&self.query)
        } else {
            self.ingest.iter().find(|set| set.batch == batch)
        }
    }

    /// Every file of the bundle that the layout names and the pipeline id covers: the manifest
    /// it was read from (when there is one), the shared weights, every query and ingest graph, the token table
    /// files and the tokenizer.
    pub(crate) fn files(&self) -> Vec<String> {
        let mut files = Vec::new();
        if self.from_manifest {
            files.push(self.manifest_file.to_owned());
        }
        files.extend(self.query.file_names());
        for set in &self.ingest {
            files.extend(set.file_names());
        }
        files.push(self.weights.clone());
        match &self.table {
            TableSpec::F16 { table } => files.push(table.clone()),
            TableSpec::Int8 { table, scales } => files.extend([table.clone(), scales.clone()]),
        }
        files.push("tokenizer.json".to_owned());
        files
    }
}

/// Files that must exist for a directory to be an E40 bundle and that the pipeline id covers.
pub(crate) fn artifacts(dir: &Path) -> Result<Vec<String>, EmbedError> {
    Ok(Layout::load(dir)?.files())
}

/// True when `dir` holds the E40 layout: a token table (fp16 or int8) beside the fixed-length
/// graphs. This is how the bundle is chosen, from the directory and never from a setting.
pub(crate) fn is_bundle(dir: &Path) -> bool {
    (dir.join(TABLE_FILE).is_file() || dir.join(TABLE_I8_FILE).is_file())
        && dir.join("model_32.onnx").is_file()
}

/// The smallest graph length that holds `tokens`, or `None` when it exceeds [`MAX_TOKENS`].
#[cfg_attr(
    not(any(target_os = "macos", feature = "bench-internals")),
    allow(dead_code)
)]
pub(crate) fn bucket_for(tokens: usize) -> Option<usize> {
    BUCKETS.iter().copied().find(|bucket| *bucket >= tokens)
}

/// The smallest of the ascending `lengths` that holds `tokens`, or `None` when none does.
pub(crate) fn bucket_in(lengths: &[usize], tokens: usize) -> Option<usize> {
    lengths.iter().copied().find(|length| *length >= tokens)
}

/// Byte offset of the data and the dimensions of a `.npy` file whose dtype is `descr` (for
/// example `<f2` or `|i1`) in C order. Accepts format versions 1.0 and 2.0.
pub(crate) fn parse_npy_header(bytes: &[u8], descr: &str) -> Result<(usize, Vec<usize>), String> {
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
    if !header.contains(&format!("'descr': '{descr}'")) {
        return Err(format!("expected dtype {descr}, header is {header:?}"));
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
    Ok((end, dims))
}

/// Byte offset of the data and the `(rows, columns)` shape of a `.npy` file that must be
/// little-endian float16 in C order. Accepts format versions 1.0 and 2.0.
pub(crate) fn parse_npy_f16_header(bytes: &[u8]) -> Result<(usize, usize, usize), String> {
    let (offset, dims) = parse_npy_header(bytes, "<f2")?;
    let [rows, columns] = dims[..] else {
        return Err(format!("expected a 2-D table, shape is {dims:?}"));
    };
    Ok((offset, rows, columns))
}

/// The host-side token-embedding table, kept as the file's bytes (fp16: 268 MB; int8: 134 MB
/// plus 0.5 MB of scales) and converted per looked-up row.
pub(crate) struct TokenTable {
    bytes: Vec<u8>,
    offset: usize,
    rows: usize,
    /// Per-row fp16 scales (little-endian bytes from the offset); `Some` for an int8 table.
    scales: Option<(Vec<u8>, usize)>,
}

impl TokenTable {
    /// Loads the token table the layout names, from the bundle in `dir`.
    pub(crate) fn load_bundle(dir: &Path, layout: &Layout) -> Result<Self, EmbedError> {
        match &layout.table {
            TableSpec::F16 { table } => Self::load(&dir.join(table)),
            TableSpec::Int8 { table, scales } => {
                Self::load_int8(&dir.join(table), &dir.join(scales))
            }
        }
    }

    pub(crate) fn load(path: &Path) -> Result<Self, EmbedError> {
        let bytes = std::fs::read(path).map_err(|e| {
            EmbedError::InvalidConfig(format!("token table {}: {e}", path.display()))
        })?;
        Self::from_bytes(bytes)
            .map_err(|e| EmbedError::InvalidConfig(format!("token table {}: {e}", path.display())))
    }

    pub(crate) fn load_int8(table: &Path, scales: &Path) -> Result<Self, EmbedError> {
        let read = |path: &Path| {
            std::fs::read(path).map_err(|e| {
                EmbedError::InvalidConfig(format!("token table {}: {e}", path.display()))
            })
        };
        let (table_bytes, scale_bytes) = (read(table)?, read(scales)?);
        Self::from_int8_bytes(table_bytes, scale_bytes).map_err(|e| {
            EmbedError::InvalidConfig(format!(
                "int8 token table {} and {}: {e}",
                table.display(),
                scales.display()
            ))
        })
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
            scales: None,
        })
    }

    /// An int8 `[VOCAB, HIDDEN]` table and its fp16 `[VOCAB]` per-row scales. A wrong dtype,
    /// shape or byte length of either file is refused.
    pub(crate) fn from_int8_bytes(bytes: Vec<u8>, scales: Vec<u8>) -> Result<Self, String> {
        Self::from_int8_checked(bytes, scales, VOCAB)
    }

    /// [`Self::from_int8_bytes`] for a table of `vocab` rows (tests use a small vocabulary).
    fn from_int8_checked(bytes: Vec<u8>, scales: Vec<u8>, vocab: usize) -> Result<Self, String> {
        let (offset, dims) = parse_npy_header(&bytes, "|i1").map_err(|e| format!("table: {e}"))?;
        let [rows, columns] = dims[..] else {
            return Err(format!("table: expected a 2-D table, shape is {dims:?}"));
        };
        if columns != HIDDEN {
            return Err(format!("table: expected {HIDDEN} columns, found {columns}"));
        }
        if rows != vocab {
            return Err(format!("table: expected {vocab} rows, found {rows}"));
        }
        if bytes.len() - offset != rows * columns {
            return Err(format!(
                "table: expected {} data bytes, found {}",
                rows * columns,
                bytes.len() - offset
            ));
        }
        let (scale_offset, scale_dims) =
            parse_npy_header(&scales, "<f2").map_err(|e| format!("scales: {e}"))?;
        // One scale per row: shape `[rows, 1]` (or a flat `[rows]`).
        let scale_rows = match scale_dims[..] {
            [rows] | [rows, 1] => rows,
            _ => {
                return Err(format!(
                    "scales: expected shape [{rows}, 1], found {scale_dims:?}"
                ))
            }
        };
        if scale_rows != rows {
            return Err(format!(
                "scales: expected {rows} values (one per table row), found {scale_rows}"
            ));
        }
        if scales.len() - scale_offset != scale_rows * 2 {
            return Err(format!(
                "scales: expected {} data bytes, found {}",
                scale_rows * 2,
                scales.len() - scale_offset
            ));
        }
        Ok(Self {
            bytes,
            offset,
            rows,
            scales: Some((scales, scale_offset)),
        })
    }

    /// Appends the embedding row of `id` (unscaled fp16) to `out`. An int8 table is dequantised
    /// here: `fp16 = i8 * scale` with the row's fp16 scale.
    pub(crate) fn push_row(&self, id: i64, out: &mut Vec<f16>) -> Result<(), EmbedError> {
        let row = usize::try_from(id)
            .ok()
            .filter(|row| *row < self.rows)
            .ok_or_else(|| {
                EmbedError::Tokenizer(format!("token id {id} is outside the token table"))
            })?;
        match &self.scales {
            None => {
                let start = self.offset + row * HIDDEN * 2;
                out.extend(
                    self.bytes[start..start + HIDDEN * 2]
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|pair| f16::from_bits(u16::from_le_bytes([pair[0], pair[1]]))),
                );
            }
            Some((scales, scale_offset)) => {
                let at = scale_offset + row * 2;
                let scale =
                    f16::from_bits(u16::from_le_bytes([scales[at], scales[at + 1]])).to_f32();
                let start = self.offset + row * HIDDEN;
                out.extend(
                    self.bytes[start..start + HIDDEN]
                        .iter()
                        .map(|byte| f16::from_f32(f32::from(*byte as i8) * scale)),
                );
            }
        }
        Ok(())
    }
}

/// `inputs_embeds` (`batch * bucket * HIDDEN` values) and the int64 mask (`batch * bucket`) of a
/// static batch-`batch` graph. Each of `rows` is padded to `bucket` with `pad_id` rows and a zero
/// mask; rows `rows.len()..batch` are all padding (every position `pad_id`, mask 0), so they
/// carry no attention and the caller drops their outputs.
pub(crate) fn build_batch_inputs(
    table: &TokenTable,
    rows: &[&[i64]],
    batch: usize,
    bucket: usize,
    pad_id: i64,
) -> Result<(Vec<f16>, Vec<i64>), EmbedError> {
    if rows.len() > batch {
        return Err(EmbedError::Output(format!(
            "{} rows do not fit a batch-{batch} graph",
            rows.len()
        )));
    }
    let mut embeds = Vec::with_capacity(batch * bucket * HIDDEN);
    let mut mask = vec![0i64; batch * bucket];
    for row in 0..batch {
        let ids = rows.get(row).copied().unwrap_or(&[]);
        if ids.len() > bucket {
            return Err(EmbedError::Output(format!(
                "{} tokens exceed the {bucket}-token graph",
                ids.len()
            )));
        }
        for position in 0..bucket {
            match ids.get(position) {
                Some(id) => {
                    table.push_row(*id, &mut embeds)?;
                    mask[row * bucket + position] = 1;
                }
                None => table.push_row(pad_id, &mut embeds)?,
            }
        }
    }
    Ok((embeds, mask))
}

/// The first `real` of `batch` output rows of `dim` values; the padding rows after them are
/// dropped. Refuses an output whose size is not `batch * dim`.
pub(crate) fn split_batch_output(
    values: &[f32],
    batch: usize,
    real: usize,
    dim: usize,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    if real > batch || values.len() != batch * dim {
        return Err(EmbedError::Output(format!(
            "expected {batch} rows of {dim} values, got {} values for {real} real rows",
            values.len()
        )));
    }
    Ok(values
        .chunks_exact(dim)
        .take(real)
        .map(<[f32]>::to_vec)
        .collect())
}

/// The graph batch for a chunk of `remaining` documents: 8 for five or more, 4 for two to four,
/// 1 (the query graph) for one, each only when `available` (the ingest batches the bundle ships
/// within the configured cap) has it. Without batch 8, five or more use batch 4; without
/// batch 4, two to four use batch 1.
pub(crate) fn select_batch(remaining: usize, available: &[usize]) -> usize {
    if remaining >= 5 && available.contains(&8) {
        8
    } else if remaining >= 2 && available.contains(&4) {
        4
    } else {
        1
    }
}

/// Runs `rows` through `run` in groups, shortest first so a group's longest document is as short
/// as it can be, and returns one vector per row in input order. Each group takes the next
/// `select(remaining)` rows (at most that many, fewer for the last); `run` receives the graph
/// batch and the group and must return exactly one vector per row of the group.
pub(crate) fn run_ingest_groups(
    rows: &[&[i64]],
    select: impl Fn(usize) -> usize,
    mut run: impl FnMut(usize, &[&[i64]]) -> Result<Vec<Vec<f32>>, EmbedError>,
) -> Result<Vec<Vec<f32>>, EmbedError> {
    let mut order: Vec<usize> = (0..rows.len()).collect();
    order.sort_by_key(|&i| rows[i].len());
    let mut out: Vec<Option<Vec<f32>>> = vec![None; rows.len()];
    let mut start = 0;
    while start < order.len() {
        let batch = select(order.len() - start).max(1);
        let group = &order[start..(start + batch).min(order.len())];
        let members: Vec<&[i64]> = group.iter().map(|&i| rows[i]).collect();
        let vectors = run(batch, &members)?;
        if vectors.len() != members.len() {
            return Err(EmbedError::Output(format!(
                "a batch of {} rows returned {} vectors",
                members.len(),
                vectors.len()
            )));
        }
        for (&index, vector) in group.iter().zip(vectors) {
            out[index] = Some(vector);
        }
        start += group.len();
    }
    out.into_iter()
        .map(|row| row.ok_or_else(|| EmbedError::Output("missing embedding row".into())))
        .collect()
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

/// The graphs of one bundle on one provider, with the shared token table: the five batch-1
/// graphs and, when the bundle ships them, the static batch-N ingest graphs.
pub(crate) struct E40Engine {
    dir: PathBuf,
    table: Arc<TokenTable>,
    token_table_read: Duration,
    provider: ExecutionProvider,
    profile: E40SessionProfile,
    layout: Arc<Layout>,
    /// Largest ingest batch used (the configured cap, 4 or 8).
    ingest_cap: usize,
    /// Built sessions by `(batch, length)`.
    sessions: Mutex<BTreeMap<(usize, usize), Session>>,
    log: Arc<SessionLog>,
}

impl E40Engine {
    /// Reads the bundle's layout (its schema-2 `manifest.json`, or the legacy fixed layout when
    /// there is none), checks that every file it names exists, reads the token table it names
    /// (fp16, or int8 with its scales) and prepares an engine; no graph is built yet.
    /// `ingest_batch` (4 or 8; anything else is refused) caps the ingest graphs used; a bundle
    /// without ingest graphs ingests on the batch-1 graphs. All graphs share the one weights
    /// file the layout names.
    pub(crate) fn open(
        dir: &Path,
        provider: ExecutionProvider,
        profile: E40SessionProfile,
        ingest_batch: usize,
    ) -> Result<Self, EmbedError> {
        let ingest_cap = check_ingest_batch(ingest_batch)?;
        let started = Instant::now();
        let layout = Layout::load(dir)?;
        let table = Arc::new(TokenTable::load_bundle(dir, &layout)?);
        Ok(Self {
            dir: dir.to_path_buf(),
            table,
            token_table_read: started.elapsed(),
            provider,
            profile,
            layout: Arc::new(layout),
            ingest_cap,
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
            layout: Arc::clone(&self.layout),
            ingest_cap: self.ingest_cap,
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

    /// The ingest graph sets this engine runs documents through: those of the bundle with batch
    /// at most the cap, on DirectML only (CPU stays on the batch-1 graphs). Empty when the
    /// bundle ships none.
    fn ingest_sets(&self) -> Vec<&GraphSet> {
        if self.provider == ExecutionProvider::DirectMl {
            self.layout.ingest_up_to(self.ingest_cap)
        } else {
            Vec::new()
        }
    }

    /// Smallest batch-1 graph length that holds `tokens`; `None` above the longest.
    pub(crate) fn query_bucket(&self, tokens: usize) -> Option<usize> {
        bucket_in(&self.layout.query.lengths, tokens)
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
        let bucket = self.query_bucket(ids.len()).ok_or_else(|| {
            EmbedError::Output(format!(
                "{} tokens exceed the longest E40 graph ({MAX_TOKENS})",
                ids.len()
            ))
        })?;
        self.run_graph(1, &[ids], pad_id, bucket)
    }

    /// Documents through the static batch-N ingest graphs: `Some(vectors)`, one per row in input
    /// order, or `None` when this engine has no ingest graphs (the caller runs the rows one by
    /// one). Rows are taken shortest first; each chunk of the remaining documents uses batch 8
    /// for five or more, batch 4 for two to four and the batch-1 query graph for one (limited to
    /// the batches the bundle ships within the configured cap). A chunk runs on the smallest
    /// length that holds its longest row, is filled up to the batch with rows of attention mask
    /// 0, and those rows' outputs are dropped. A chunk longer than the longest ingest graph runs
    /// row by row on the query graphs.
    pub(crate) fn run_ingest(
        &self,
        rows: &[&[i64]],
        pad_id: i64,
    ) -> Result<Option<Vec<Vec<f32>>>, EmbedError> {
        let sets = self.ingest_sets();
        if sets.is_empty() {
            return Ok(None);
        }
        let available: Vec<usize> = sets.iter().map(|set| set.batch).collect();
        let vectors = run_ingest_groups(
            rows,
            |remaining| select_batch(remaining, &available),
            |batch, group| {
                let set = sets.iter().find(|set| set.batch == batch);
                let longest = group.iter().map(|row| row.len()).max().unwrap_or(0);
                let Some((set, bucket)) =
                    set.and_then(|set| bucket_in(&set.lengths, longest).map(|b| (*set, b)))
                else {
                    return group.iter().map(|ids| self.run(ids, pad_id)).collect();
                };
                let (values, _) = self.run_graph(set.batch, group, pad_id, bucket)?;
                split_batch_output(&values, set.batch, group.len(), OUTPUT_DIM)
            },
        )?;
        Ok(Some(vectors))
    }

    /// Builds every graph that is not built yet and runs one dummy text (a single `pad_id`
    /// token) through each, so the first real query per bucket pays neither a session build nor
    /// the provider's first-run cost. Returns the time it took. Any build or run error is
    /// returned: a graph that cannot serve here must not be admitted. The ingest graphs, when
    /// this engine uses them, are warmed the same way.
    pub(crate) fn prewarm(&self, pad_id: i64) -> Result<Duration, EmbedError> {
        let started = Instant::now();
        for bucket in &self.layout.query.lengths {
            self.run_graph(1, &[&[pad_id]], pad_id, *bucket)?;
        }
        for set in self.ingest_sets() {
            for length in &set.lengths {
                self.run_graph(set.batch, &[&[pad_id]], pad_id, *length)?;
            }
        }
        Ok(started.elapsed())
    }

    /// One call of the static batch-`batch` graph of length `bucket` over `rows` (at most
    /// `batch`, each at most `bucket` tokens; missing rows are padding). Returns the graph's raw
    /// `[batch, 768]` output, padding rows included.
    fn run_graph(
        &self,
        batch: usize,
        rows: &[&[i64]],
        pad_id: i64,
        bucket: usize,
    ) -> Result<(Vec<f32>, RunSplit), EmbedError> {
        let started = Instant::now();
        let (embeds, mask) = build_batch_inputs(&self.table, rows, batch, bucket, pad_id)?;
        let build_inputs_time = started.elapsed();

        let phase = Instant::now();
        let runtime = |e: &dyn std::fmt::Display| EmbedError::Runtime(e.to_string());
        let shape = [batch as i64, bucket as i64];
        let embeds = ort::value::Tensor::from_array(([shape[0], shape[1], HIDDEN as i64], embeds))
            .map_err(|e| runtime(&e))?;
        let mask = ort::value::Tensor::from_array((shape, mask)).map_err(|e| runtime(&e))?;
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
        let session = match sessions.entry((batch, bucket)) {
            std::collections::btree_map::Entry::Occupied(entry) => entry.into_mut(),
            std::collections::btree_map::Entry::Vacant(entry) => {
                built_session = true;
                entry.insert(self.build_session(batch, bucket)?)
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
                tokens: rows.iter().map(|row| row.len()).max().unwrap_or(0),
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

    /// One graph on this engine's provider: `model_<bucket>.onnx` of the bundle for batch 1, of
    /// the ingest directory for the ingest batch. The CPU fallback of `SessionOptions` is never
    /// used here: a graph that cannot be built on the requested provider is an error, so the
    /// provider the embedder reports is the provider that ran. The build time is logged for
    /// `Embedder::load_timings`.
    fn build_session(&self, batch: usize, bucket: usize) -> Result<Session, EmbedError> {
        let started = Instant::now();
        let set = self.layout.graphs(batch).ok_or_else(|| {
            EmbedError::Runtime(format!("no batch-{batch} graphs in this bundle"))
        })?;
        if !set.lengths.contains(&bucket) {
            return Err(EmbedError::Runtime(format!(
                "no batch-{batch} graph of length {bucket} in this bundle"
            )));
        }
        let session = SessionOptions::new(self.provider)
            .with_threads(self.profile.intra_threads)
            .with_cpu_fallback(false)
            // Default: the measured configuration of the DirectML runs in the export README.
            .with_directml_sequential(self.profile.sequential)
            .with_directml_memory_pattern(self.profile.disable_memory_pattern)
            .build_from_file(&self.dir.join(set.file_name(bucket)))
            .map_err(|e| EmbedError::Runtime(e.to_string()))?;
        self.log.push(SessionCreate {
            provider: provider_name(self.provider),
            graph_tokens: bucket,
            duration: started.elapsed(),
        });
        Ok(session)
    }
}
