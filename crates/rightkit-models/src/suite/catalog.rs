//! The built-in model catalog — one `ModelSpec` per model ScrapeRight knows
//! how to find, verify, download, and migrate in the shared store.
//!
//! Every ready variant has trusted per-file SHA-256 & byte-size pins.
//! Mac pins come from the archive verified against HeardRight’s archive digest;
//! local installation records never define their own approved identity.

/// One file the installed entry dir must contain.
#[derive(Debug, Clone, Copy)]
pub struct SpecFile {
    /// Relative path inside the entry dir (forward slashes).
    pub path: &'static str,
    /// Pinned lowercase-hex SHA-256, when one is recorded elsewhere.
    pub sha256: Option<&'static str>,
    /// Pinned size in bytes.
    pub size: Option<u64>,
}

/// How a remote file reaches the entry dir during a download.
#[derive(Debug, Clone, Copy)]
pub struct RemoteFile {
    pub path: &'static str,
    /// Direct download URL. `None` = bundled seed file (copied from the
    /// caller-provided resource dir — e.g. the small OCR configs/dict that
    /// are git-tracked app resources and have no direct URL).
    pub url: Option<&'static str>,
    pub sha256: Option<&'static str>,
    pub size: Option<u64>,
}

/// How the model is fetched when the store has no verified copy.
#[derive(Debug, Clone)]
pub enum SpecSource {
    /// One `.tar.gz` archive (single wrapper dir inside) → staged extract →
    /// atomic swap. The `sha256`/`size` describe the *archive* bytes.
    Archive {
        /// `None` = no download endpoint configured for this platform.
        url: Option<&'static str>,
        sha256: Option<&'static str>,
        size: Option<u64>,
        /// The worker route is bearer-gated; `ensure` needs a token from the
        /// caller (`authorization` param) or fails with a 401 message.
        requires_auth: bool,
    },
    /// Individual remote + bundled-seed files fetched/copied to their
    /// relative paths (the OCR layout — no archive endpoint exists).
    Files(&'static [RemoteFile]),
}

/// Static description of one shared-store model.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    /// Store id = entry dir name under `<models-root>/`.
    pub id: &'static str,
    /// Human label for onboarding/Settings UI.
    pub label: &'static str,
    /// Version tag written into `manifest.json`.
    pub version: &'static str,
    /// Approximate installed size, for "downloads X GB" UX copy.
    pub size_hint_bytes: u64,
    /// Expected file set — becomes the manifest expectation on install.
    pub files: &'static [SpecFile],
    pub source: SpecSource,
    /// Candidate dirs under the legacy app-private `models/` tree that hold a
    /// migratable copy of THIS spec (relative paths, e.g. `asr/<dir>`).
    /// The first candidate that verifies is migrated; the rest are left alone.
    pub legacy_dirs: &'static [&'static str],
    /// Historical paths checked during migration; only byte-identical
    /// duplicate directories are removed after verifying current payload.
    pub retired_legacy_paths: &'static [&'static str],
    /// Store-side entry ids superseded by this spec (older shared-store ids;
    /// their dirs are pruned only when byte-identical to this verified spec).
    pub retired_store_ids: &'static [&'static str],
}

impl ModelSpec {
    /// The manifest this spec expects inside the entry dir.
    pub fn expected_manifest(&self) -> crate::suite::manifest::Manifest {
        crate::suite::manifest::Manifest {
            id: self.id.to_string(),
            version: self.version.to_string(),
            files: self
                .files
                .iter()
                .map(|f| crate::suite::manifest::ManifestFile {
                    path: f.path.to_string(),
                    sha256: f.sha256.map(str::to_string),
                    size: f.size,
                })
                .collect(),
        }
    }

    /// True when a download path exists for this platform.
    pub fn can_download(&self) -> bool {
        match &self.source {
            SpecSource::Archive { url, .. } => url.is_some(),
            SpecSource::Files(files) => files.iter().any(|f| f.url.is_some()),
        }
    }

    /// True when the download route needs an Authorization bearer.
    pub fn requires_auth(&self) -> bool {
        match &self.source {
            SpecSource::Archive { requires_auth, .. } => *requires_auth,
            SpecSource::Files(_) => false,
        }
    }
}

// ---------------------------------------------------------------------------
// Parakeet TDT v3 — the one locked ASR model.
// ---------------------------------------------------------------------------

/// Model worker base URL. Override with `SR_MODEL_DOWNLOAD_BASE` (dev/staging
/// worker or a local test server); the path layout is appended after it.
pub const MODEL_DOWNLOAD_BASE: &str = "https://heardright-models.adrdsouza.workers.dev";

#[cfg(windows)]
const PARAKEET_ARCHIVE_URL: Option<&str> = Some(concat!(
    "https://heardright-models.adrdsouza.workers.dev",
    "/parakeet/windows/tdt.tar.gz"
));
#[cfg(target_os = "macos")]
const PARAKEET_ARCHIVE_URL: Option<&str> = Some(concat!(
    "https://heardright-models.adrdsouza.workers.dev",
    "/parakeet/macos/tdt.tar.gz"
));
// No Linux route exists on the model worker yet — honest "unavailable".
#[cfg(not(any(windows, target_os = "macos")))]
const PARAKEET_ARCHIVE_URL: Option<&str> = None;

/// package.ps1 `$TdtSha256` — SHA-256 of the whole release archive. The pin is
/// the trust anchor for the Windows download path (the worker serves the file
/// from private R2 behind the app bearer token).
#[cfg(windows)]
const PARAKEET_ARCHIVE_SHA256: Option<&str> =
    Some("f73e8671086bfe369e263468a91826be24cd59d826851ae531dddd5033c4b99e");
#[cfg(target_os = "macos")]
const PARAKEET_ARCHIVE_SHA256: Option<&str> =
    Some("5f8888117f4750828afeb7aa185a706299d0a0ef144190ce9554d82962d51899");
#[cfg(not(any(windows, target_os = "macos")))]
const PARAKEET_ARCHIVE_SHA256: Option<&str> = None;

/// Windows/Linux file set — the HeardRight 2026-07-22 static1500-QInt8 ONNX
/// TDT (docs/PACKAGING.md: "exactly config.json, encoder-model.onnx,
/// decoder_joint-model.onnx, mel_nemo.onnx, mel_nemo.onnx.data, vocab.txt",
/// 654,626,874 bytes total).
#[cfg(not(target_os = "macos"))]
const PARAKEET_FILES: &[SpecFile] = &[
    SpecFile {
        path: "config.json",
        sha256: Some("0cfca69c4ae6930ecfa1ccc863f846a3e5a40e120d858161c31ece009dbe9734"),
        size: Some(125),
    },
    SpecFile {
        path: "encoder-model.onnx",
        sha256: Some("08aea64ea46749d0c6287b69dc74d680ea7e4cd87b98d38722cd073913d358ae"),
        size: Some(635_120_272),
    },
    SpecFile {
        path: "decoder_joint-model.onnx",
        sha256: Some("518fb9649d92479360926ddd57697490cf8d7167848d6879994941d5386fda36"),
        size: Some(18_202_001),
    },
    SpecFile {
        path: "mel_nemo.onnx",
        sha256: Some("f3beb3d21b4143cc7d1471815bd90687110a75a63984b89c86306e9284563881"),
        size: Some(22_185),
    },
    SpecFile {
        path: "mel_nemo.onnx.data",
        sha256: Some("9e04389b417c35a888c7a85ae525b733dd6ab8d3d1592c7fa66ef9d26d076a19"),
        size: Some(1_188_352),
    },
    SpecFile {
        path: "vocab.txt",
        sha256: Some("d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d"),
        size: Some(93_939),
    },
];

/// macOS file set — the locked CoreML TDT bundle. Same required-file list as
/// `seeding.rs::parakeet_model_seed_complete`: root metadata plus the four
/// compiled stage bundles' IR/blob/weights/analytics payloads.
#[cfg(target_os = "macos")]
const PARAKEET_FILES: &[SpecFile] = &[
    SpecFile {
        path: "vocab.txt",
        sha256: Some("5b133ab19bf7bf03e06e61a193d2dedb96f72677850a670231bb7344c2e09d4c"),
        size: Some(54078),
    },
    SpecFile {
        path: "pipeline.json",
        sha256: Some("a855a08233cf433d1ae8b92602776efe6dfbe702201174a81d191fe05833026c"),
        size: Some(759),
    },
    SpecFile {
        path: "NOTICE.txt",
        sha256: Some("0d026f56d1fef425906977b7a2e5841860f35bfbe1c1ac5600def14f3588fe18"),
        size: Some(1667),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/model.mil",
        sha256: Some("a1726e1c9617b2e74c69013f6dbe6e5fa8999e010fd0c17eba653a419f8dc321"),
        size: Some(1836615),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/coremldata.bin",
        sha256: Some("bee7a24b165db726082b18dfb71f3a1aabe7f1a0581ac063d0a0ecc478980b08"),
        size: Some(401),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/weights/weight.bin",
        sha256: Some("ae93948d09e8b7710c71e583c3364416e5855afe77f07a47ccb05e3ca92ff1d6"),
        size: Some(367296),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/analytics/coremldata.bin",
        sha256: Some("68c291326457b158acc4fd904fdca65337129781f58c2b16424fb68e804cbd5d"),
        size: Some(243),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/model.mil",
        sha256: Some("bbbba144052cbfe8b335befdeafa076e8a6fe3710c051ff10115cd77bb8865aa"),
        size: Some(967959),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/coremldata.bin",
        sha256: Some("ca7de06a90cc443668214c40fb070bf359adda92df714156db2a2435dacebb75"),
        size: Some(419),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/weights/weight.bin",
        sha256: Some("5e4edecb5938c94aaf2bfaff18d837f508bd0623756e5201b9376356bc9b70c3"),
        size: Some(410797632),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/analytics/coremldata.bin",
        sha256: Some("e750587b525b6c7e03893415434a344d5f570abf8fe282cd817c46676405e603"),
        size: Some(243),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/model.mil",
        sha256: Some("2044c6db8c0a70a7ab919c29c050ca9807ea6c2ae4cb39fb3aacb13cbc342562"),
        size: Some(11436),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/coremldata.bin",
        sha256: Some("daeea71939a6c9c80ebaba02f2f3a4ecd4bc9a39dd393012822acfd56d40b44f"),
        size: Some(486),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/weights/weight.bin",
        sha256: Some("48adf0f0d47c406c8253d4f7fef967436a39da14f5a65e66d5a4b407be355d41"),
        size: Some(23604992),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/analytics/coremldata.bin",
        sha256: Some("8735b4f02e978a6fa98d44ab00c57914dc68cc403322140b0929b21fd0054f6b"),
        size: Some(243),
    },
    SpecFile {
        path: "Joint.mlmodelc/model.mil",
        sha256: Some("e18483311b90d85c19bd6d3a2b3778e39d2bfbb719fd14e700835ff2f6cae9d7"),
        size: Some(5630),
    },
    SpecFile {
        path: "Joint.mlmodelc/coremldata.bin",
        sha256: Some("730f502016870d7ff222de0d7c5c58724eadec4252f7c7765a5d6dbad767f6bb"),
        size: Some(414),
    },
    SpecFile {
        path: "Joint.mlmodelc/weights/weight.bin",
        sha256: Some("ca5699b9f87cf3c3b09e9e19491477a2a410f9e4050c425a3b8d8ea575705e95"),
        size: Some(6359308),
    },
    SpecFile {
        path: "Joint.mlmodelc/analytics/coremldata.bin",
        sha256: Some("14b014d3bf77b2543349d245ec9725dc89aacddf9de454ccd30714b503cbfa9a"),
        size: Some(243),
    },
];

/// Parakeet TDT v3 shared-store spec — the production ASR model for every
/// Right Suite app on this machine.
pub fn parakeet_spec() -> ModelSpec {
    #[cfg(not(target_os = "macos"))]
    let (id, legacy_dirs) = (
        "parakeet_tdt_v3_static1500_qint8_20260722",
        &["asr/parakeet_tdt_v3_static1500_qint8_20260722"][..],
    );
    #[cfg(target_os = "macos")]
    let (id, legacy_dirs) = (
        "parakeet-tdt-v3-coreml-compiled-5f8888117f47",
        &[
            "asr/parakeet-tdt-v3",
            "asr/parakeet-tdt-v3-coreml-compiled-5f8888117f47",
        ][..],
    );

    ModelSpec {
        id,
        label: "Parakeet TDT v3 speech model",
        version: PARAKEET_ARCHIVE_SHA256.unwrap_or(id),
        size_hint_bytes: if cfg!(target_os = "macos") {
            444010064
        } else {
            654_626_874
        },
        files: PARAKEET_FILES,
        source: SpecSource::Archive {
            url: PARAKEET_ARCHIVE_URL,
            sha256: PARAKEET_ARCHIVE_SHA256,
            size: None,
            requires_auth: true,
        },
        legacy_dirs,
        // package.ps1's retired list plus the no-longer-shipped extras that
        // account for the 2.7 GB the plan calls out (whisper turbo, ggml
        // tiny, older TDT variants). Files AND dirs are both handled.
        retired_legacy_paths: &[
            "asr/unified_static15_b128_sym_bits4_timestamp_hybrid",
            "asr/parakeet_tdt_v3_static_b64",
            "asr/parakeet_tdt_v3_b64_sym_bits4_protect",
            "asr/whisper_turbo_q5_k.bin",
            "asr/ggml-tiny.bin",
            "asr/whisper-tiny.en.bin",
        ],
        retired_store_ids: &[
            "parakeet_tdt_v3_static_b64",
            "parakeet_tdt_v3_b64_sym_bits4_protect",
            "unified_static15_b128_sym_bits4_timestamp_hybrid",
        ],
    }
}

// ---------------------------------------------------------------------------
// PP-OCRv6 — Windows-only OCR fallback (macOS uses Apple Vision).
// ---------------------------------------------------------------------------

#[cfg(windows)]
const OCR_REMOTE_FILES: &[RemoteFile] = &[
    RemoteFile {
        path: "det.onnx",
        url: Some(
            "https://huggingface.co/PaddlePaddle/PP-OCRv6_medium_det_onnx/resolve/main/inference.onnx",
        ),
        sha256: Some("eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1"),
        size: Some(62_032_837),
    },
    RemoteFile {
        path: "rec.onnx",
        url: Some(
            "https://huggingface.co/PaddlePaddle/PP-OCRv6_small_rec_onnx/resolve/main/inference.onnx",
        ),
        sha256: Some("5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634"),
        size: Some(21_159_378),
    },
    // Small metadata files are supplied from runtime/ocr-metadata; model
    // weights above always download into shared store.
    RemoteFile {
        path: "en_dict.txt",
        url: None,
        sha256: Some("b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d"),
        size: Some(74_947),
    },
    RemoteFile {
        path: "det_config.json",
        url: None,
        sha256: Some("34b1fbbc8b4fc716a8427090c115afbb87f8873403c5d02564c576febd72d228"),
        size: Some(513),
    },
    RemoteFile {
        path: "rec_config.json",
        url: None,
        sha256: Some("020ba2ab9e953400c91b0d61f7684f42fcfdd549e15d13ff707f783ad8d7aae8"),
        size: Some(656),
    },
];

#[cfg(windows)]
const OCR_FILES: &[SpecFile] = &[
    SpecFile {
        path: "det.onnx",
        sha256: Some("eb13b44b25bb36f89528b68720af8a61d9cf381176107f465db1757b65d086e1"),
        size: Some(62_032_837),
    },
    SpecFile {
        path: "rec.onnx",
        sha256: Some("5435fd747c9e0efe15a96d0b378d5bd157e9492ed8fd80edf08f30d02fa24634"),
        size: Some(21_159_378),
    },
    SpecFile {
        path: "en_dict.txt",
        sha256: Some("b5f2bfe2bdd9448429e3e82b51c789775d9b42f2403d082b00662eb77e401c5d"),
        size: Some(74_947),
    },
    SpecFile {
        path: "det_config.json",
        sha256: Some("34b1fbbc8b4fc716a8427090c115afbb87f8873403c5d02564c576febd72d228"),
        size: Some(513),
    },
    SpecFile {
        path: "rec_config.json",
        sha256: Some("020ba2ab9e953400c91b0d61f7684f42fcfdd549e15d13ff707f783ad8d7aae8"),
        size: Some(656),
    },
];

/// PP-OCRv6 fallback weights (`det.onnx` + `rec.onnx` ~80 MB) — Windows only.
/// Fetched in onboarding per the plan's tools-policy table.
#[cfg(windows)]
pub fn ocr_spec() -> ModelSpec {
    ModelSpec {
        id: "pp-ocrv6",
        label: "PP-OCRv6 image-text fallback",
        version: "pp-ocrv6",
        size_hint_bytes: 83_200_000,
        files: OCR_FILES,
        source: SpecSource::Files(OCR_REMOTE_FILES),
        legacy_dirs: &["ocr"],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

/// Every model this app uses on this platform — the catalog the shell status,
/// migration, and onboarding iterate.
pub fn catalog() -> Vec<ModelSpec> {
    #[allow(unused_mut)]
    let mut specs = vec![parakeet_spec()];
    #[cfg(windows)]
    specs.push(ocr_spec());
    specs
}
