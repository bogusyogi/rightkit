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

/// Whether every install needs a model or it is an optional extra.
///
/// The grouping is data on the spec, never a model's position in a list: code that asks for
/// "what this install needs" asks for [`ModelTier::Base`], and an optional download is offered
/// separately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ModelTier {
    /// Every install needs it (the base set: [`base_catalog`], and [`catalog`]).
    Base,
    /// Optional and usually entitlement-gated; offered on demand ([`add_ons`]). A missing
    /// add-on never makes setup incomplete and never counts toward onboarding size.
    AddOn,
}

/// Static description of one shared-store model.
#[derive(Debug, Clone)]
pub struct ModelSpec {
    /// Store id = entry dir name under `<models-root>/`.
    pub id: &'static str,
    /// Human label for onboarding/Settings UI.
    pub label: &'static str,
    /// Base (every install) or add-on (optional); see [`ModelTier`].
    pub tier: ModelTier,
    /// With `source` requiring auth: the bearer is the app's signed Pro entitlement, not a
    /// worker download grant. Grants are never requested for such a model. Leave `false`
    /// for grant-gated models; `requires_auth()` stays the one "needs a bearer" fact.
    pub entitlement: bool,
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

    /// True when the bearer is the app's signed Pro entitlement (no grant exists for it).
    pub fn requires_entitlement(&self) -> bool {
        self.entitlement && self.requires_auth()
    }

    /// True when the bearer is a worker download grant (`/v1/model-grants`).
    pub fn requires_grant(&self) -> bool {
        self.requires_auth() && !self.entitlement
    }

    /// True for an optional add-on.
    pub fn is_add_on(&self) -> bool {
        self.tier == ModelTier::AddOn
    }

    /// Label for the UI: add-ons say so, so a list never presents one as required.
    pub fn display_label(&self) -> String {
        match self.tier {
            ModelTier::Base => self.label.to_string(),
            ModelTier::AddOn => format!("{} (optional)", self.label),
        }
    }

    /// Status text for a model that is not installed.
    pub fn missing_status_label(&self) -> &'static str {
        match self.tier {
            ModelTier::Base => "Not installed",
            ModelTier::AddOn => "Optional, not installed",
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
        tier: ModelTier::Base,
        entitlement: false,
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
        tier: ModelTier::Base,
        entitlement: false,
        version: "pp-ocrv6",
        size_hint_bytes: 83_200_000,
        files: OCR_FILES,
        source: SpecSource::Files(OCR_REMOTE_FILES),
        legacy_dirs: &["ocr"],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

// ---------------------------------------------------------------------------
// Whisper large-v3-turbo (Pro multilingual) — shared by HeardRight and
// ScrapeRight. whisper.cpp 1.9.3 loads the ggml `.bin` below; it does not read
// GGUF. Resolution lives in `suite::whisper`; the contract is in the README.
// ---------------------------------------------------------------------------

/// The ggml model file inside the `whisper-turbo` entry dir.
pub const WHISPER_GGML_FILE: &str = "whisper_turbo_q5_k.bin";

/// SHA-256 of the whole `whisper-win.tar.gz` archive (HeardRight
/// `whisper_model.rs:78-89`). The archive is the download trust anchor and the
/// entry's version tag.
pub const WHISPER_GGML_ARCHIVE_SHA256: &str =
    "e4fa834a3b6000ca51b71ac42799277b33496e2f86c43e95b3da57b170f76f9f";

/// SHA-256 of `whisper_turbo_q5_k.bin` itself. Pinned so the installed file is
/// proven, not only the archive that carried it. Recorded in
/// `heardright/model_registry/FINAL_MANIFEST_SHA256.csv` (`final\asr\`) and
/// matched by the local ScrapeRight copy; the archive was not reachable to
/// confirm it against `WHISPER_GGML_ARCHIVE_SHA256` (see CHANGELOG 0.2.9).
pub const WHISPER_GGML_FILE_SHA256: &str =
    "fcacbe37c8ddd49a137b3ef7cccd2743bea1f9a9dd20d2b9755978972a02bb0b";

/// Byte size of `whisper_turbo_q5_k.bin` (large-v3-turbo, q5_k).
pub const WHISPER_GGML_FILE_SIZE: u64 = 574_041_195;

const WHISPER_GGML_FILES: &[SpecFile] = &[SpecFile {
    path: WHISPER_GGML_FILE,
    sha256: Some(WHISPER_GGML_FILE_SHA256),
    size: Some(WHISPER_GGML_FILE_SIZE),
}];

/// The shared ggml large-v3-turbo entry at `<models>/whisper-turbo/`. The
/// archive has one top-level `whisper-win/` wrapper, stripped on install, so
/// the `.bin` lands directly in the entry dir. Pro-gated: `requires_auth`.
pub fn whisper_turbo_ggml_spec() -> ModelSpec {
    ModelSpec {
        id: "whisper-turbo",
        label: "Whisper large-v3-turbo (ggml q5_k)",
        tier: ModelTier::AddOn,
        entitlement: true,
        version: WHISPER_GGML_ARCHIVE_SHA256,
        size_hint_bytes: WHISPER_GGML_FILE_SIZE,
        files: WHISPER_GGML_FILES,
        source: SpecSource::Archive {
            url: Some("https://heardright-models.adrdsouza.workers.dev/whisper-win.tar.gz"),
            sha256: Some(WHISPER_GGML_ARCHIVE_SHA256),
            size: None,
            requires_auth: true,
        },
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

/// A Whisper archive pinned by its archive digest only. It is NOT a store
/// entry: `verify_dir` marks any file without a size and SHA-256 pin as
/// `Unverified`, so an entry built from these could never reach `Ready`. Per-file
/// pins have not been measured (the worker does not serve the archive), so
/// nothing installs from this descriptor yet.
#[derive(Debug, Clone, Copy)]
pub struct WhisperArchivePin {
    /// Platform that consumes the bundle.
    pub platform: &'static str,
    /// Worker route the app downloads from (Pro-gated).
    pub url: &'static str,
    /// Lowercase SHA-256 of the whole `.tar.gz`.
    pub sha256: &'static str,
    /// Wrapper directory name inside the archive and the app's install dir.
    pub directory_name: &'static str,
    /// Entries the native loader requires beside or inside the bundle.
    pub required_entries: &'static [&'static str],
}

/// macOS CoreML `whisper-multi` bundle (HeardRight `whisper_model.rs:72-76`).
pub const WHISPER_COREML_ARCHIVE: WhisperArchivePin = WhisperArchivePin {
    platform: "macos",
    url: "https://heardright-models.adrdsouza.workers.dev/whisper-multi.tar.gz",
    sha256: "046ce2aea772e2b102a31bfb52143b58b8ab97637dadc661d4723e4fd5a7f1d7",
    directory_name: "whisper-multi",
    required_entries: &[
        "AudioEncoder.mlmodelc",
        "TextDecoder.mlmodelc",
        "tokenizer.json",
    ],
};

/// iOS CoreML `whisper-multi` bundle (HeardRight `ModelArtifactInstaller.swift`
/// `iosWhisperMulti`, lines 63-71). A different archive from the macOS one.
pub const WHISPER_IOS_ARCHIVE: WhisperArchivePin = WhisperArchivePin {
    platform: "ios",
    url: "https://heardright-models.adrdsouza.workers.dev/whisper/ios/multi.tar.gz",
    sha256: "a37fc293046bc9e77eafd592fc089bd75cb93079cda43bb1646461dee72de60c",
    directory_name: "whisper-multi",
    required_entries: &[
        "AudioEncoder.mlmodelc",
        "TextDecoder.mlmodelc",
        "config.json",
        "generation_config.json",
        "tokenizer.json",
    ],
};

// ---------------------------------------------------------------------------
// EmbeddingGemma 2 Q4 (b64acc4) — local embedding bundle loaded by rightkit-embed.
// NOT SERVED: no download route exists for this bundle. Install is adopt-only:
// place the six files byte-identical in `<models-root>/embeddinggemma2-q4/`, and
// the store's verify-before-adopt step checks them against the pins below. No
// network request is made. See README "EmbeddingGemma 2 Q4".
// ---------------------------------------------------------------------------

/// Store id and entry dir name. Equal to `rightkit_embed::EMBEDDINGGEMMA2_MODEL_ID`;
/// rightkit-models does not depend on rightkit-embed, so the literal is repeated here.
pub const EMBEDDINGGEMMA2_Q4_ID: &str = "embeddinggemma2-q4";

/// Intended grant route, following the Parakeet route shape. NOT SERVED: the model
/// worker (`rightsites/workers/heardright-models/src/index.ts`, `MODEL_SPECS`) has no
/// entry for it, and a grant needs one whole-object SHA-256 that this bundle lacks
/// (only per-file digests are pinned here). Not referenced by any download path.
pub const EMBEDDINGGEMMA2_Q4_INTENDED_ROUTE: &str = "/embeddinggemma2-q4/embeddinggemma2-q4.tar.gz";

/// EmbeddingGemma 2 Q4 file set, revision b64acc4 (flat layout, as rightkit-embed loads it).
/// Total 187,632,172 bytes.
const EMBEDDINGGEMMA2_Q4_FILES: &[SpecFile] = &[
    SpecFile {
        path: "model_q4.onnx",
        sha256: Some("8c7c392bc8a6989a0dbfb524d819a635de088812691acc61e898e13597e36336"),
        size: Some(475_574),
    },
    SpecFile {
        path: "model_q4.onnx_data",
        sha256: Some("e976952e890a9c515e62ecf67a4129aecbe92c7994f7cc7b9f6d33a5fb33c04e"),
        size: Some(154_979_328),
    },
    SpecFile {
        path: "tokenizer.json",
        sha256: Some("4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4"),
        size: Some(32_170_510),
    },
    SpecFile {
        path: "tokenizer_config.json",
        sha256: Some("17bd5d6e9364ca49a534e1502076593317c298d4a663623091ed45388f004874"),
        size: Some(1_599),
    },
    SpecFile {
        path: "config.json",
        sha256: Some("8d011bfe08b5e345bbe0b81e5c6fd02c381920b345b986047bc2a33ce7b90d1d"),
        size: Some(5_031),
    },
    SpecFile {
        path: "special_tokens_map.json",
        sha256: Some("61f5ea3e7ed0547b2d45ef93c1321de2b9e3429463041dc97d26057db11382d0"),
        size: Some(130),
    },
];

/// The same six files as `RemoteFile`s with no `url`. `can_download()` is therefore false,
/// so `ensure` never reaches the download path for this spec. Its only route to `Ready` is the
/// verify-before-adopt step on files already in the entry dir.
const EMBEDDINGGEMMA2_Q4_REMOTE_FILES: &[RemoteFile] = &[
    RemoteFile {
        path: "model_q4.onnx",
        url: None,
        sha256: Some("8c7c392bc8a6989a0dbfb524d819a635de088812691acc61e898e13597e36336"),
        size: Some(475_574),
    },
    RemoteFile {
        path: "model_q4.onnx_data",
        url: None,
        sha256: Some("e976952e890a9c515e62ecf67a4129aecbe92c7994f7cc7b9f6d33a5fb33c04e"),
        size: Some(154_979_328),
    },
    RemoteFile {
        path: "tokenizer.json",
        url: None,
        sha256: Some("4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4"),
        size: Some(32_170_510),
    },
    RemoteFile {
        path: "tokenizer_config.json",
        url: None,
        sha256: Some("17bd5d6e9364ca49a534e1502076593317c298d4a663623091ed45388f004874"),
        size: Some(1_599),
    },
    RemoteFile {
        path: "config.json",
        url: None,
        sha256: Some("8d011bfe08b5e345bbe0b81e5c6fd02c381920b345b986047bc2a33ce7b90d1d"),
        size: Some(5_031),
    },
    RemoteFile {
        path: "special_tokens_map.json",
        url: None,
        sha256: Some("61f5ea3e7ed0547b2d45ef93c1321de2b9e3429463041dc97d26057db11382d0"),
        size: Some(130),
    },
];

/// EmbeddingGemma 2 Q4 shared-store spec (MailRight's local embedder).
///
/// Not in [`base_catalog`] (not every install needs it) and not in [`add_ons`] (that list
/// asserts entitlement gating, and this bundle has none). Reach it with this function.
pub fn embedding_gemma_q4_spec() -> ModelSpec {
    ModelSpec {
        id: EMBEDDINGGEMMA2_Q4_ID,
        label: "EmbeddingGemma 2 Q4 embedding model",
        tier: ModelTier::AddOn,
        entitlement: false,
        version: "b64acc4",
        size_hint_bytes: 187_632_172,
        files: EMBEDDINGGEMMA2_Q4_FILES,
        source: SpecSource::Files(EMBEDDINGGEMMA2_Q4_REMOTE_FILES),
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

/// What every install needs on this platform: the base set, in dependency-free order.
/// Onboarding, setup-complete checks and size estimates iterate this. Every entry carries
/// per-file size and SHA-256 pins (the `builtin_catalog_pins_every_file` test enforces it).
pub fn base_catalog() -> Vec<ModelSpec> {
    #[allow(unused_mut)]
    let mut specs = vec![parakeet_spec()];
    #[cfg(windows)]
    specs.push(ocr_spec());
    specs
}

/// Optional downloads, each offered on demand and never part of setup. Currently the
/// entitlement-gated Whisper large-v3-turbo.
pub fn add_ons() -> Vec<ModelSpec> {
    vec![whisper_turbo_ggml_spec()]
}

/// Base set followed by the add-ons: every model the crate knows, for code that manages the
/// whole store (pruning, migration, a "downloads" settings page).
pub fn all_models() -> Vec<ModelSpec> {
    let mut specs = base_catalog();
    specs.extend(add_ons());
    specs
}

/// The models every install needs. Same as [`base_catalog`], and the meaning of this function
/// in 0.2.8 (Parakeet, plus OCR on Windows); add-ons are reached only through [`add_ons`].
pub fn catalog() -> Vec<ModelSpec> {
    base_catalog()
}
