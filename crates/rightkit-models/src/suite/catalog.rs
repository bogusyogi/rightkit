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
    /// Store-side entry ids superseded by this spec (older shared-store ids). Once this spec
    /// is Ready, an ensure by an app removes each such entry unless an app OTHER than that
    /// one still references it, whether or not the old payload matches this spec's bytes
    /// (a real model change never does); the app's own ref to it is dropped. An install in
    /// progress, a held entry lock or an active runtime-family lease leaves it for later.
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
// Parakeet Ultra — moondream/parakeet-ultra weights on the Parakeet TDT v3 graphs
// (Orthic-Labs/rightkit#897; CC-BY-4.0, based on NVIDIA parakeet-tdt-0.6b-v3).
// Additive: `parakeet_spec()` is unchanged. Ultra is reached only through
// `parakeet_ultra_spec()`, and only on a platform whose pins are recorded below.
// ---------------------------------------------------------------------------

/// Store id of the Windows Ultra entry (and its archive's top directory).
pub const PARAKEET_ULTRA_WINDOWS_ID: &str = "parakeet_tdt_ultra_static1500_fp16int4_20261022";

/// Store id of the macOS Ultra entry (and its archive's top directory).
pub const PARAKEET_ULTRA_MACOS_ID: &str = "parakeet-tdt-ultra";

/// Store id of the Ultra entry on this platform.
#[cfg(target_os = "macos")]
pub const PARAKEET_ULTRA_ID: &str = PARAKEET_ULTRA_MACOS_ID;
/// Store id of the Ultra entry on this platform.
#[cfg(not(target_os = "macos"))]
pub const PARAKEET_ULTRA_ID: &str = PARAKEET_ULTRA_WINDOWS_ID;

/// Worker route of the Windows Ultra archive. The live worker serves fixed routes, not
/// `<id>.tar.gz`; the existing `/parakeet/windows/tdt.tar.gz` route is unchanged.
pub const PARAKEET_ULTRA_WINDOWS_ROUTE: &str = "/parakeet/windows/ultra.tar.gz";

/// Worker route of the macOS Ultra archive.
pub const PARAKEET_ULTRA_MACOS_ROUTE: &str = "/parakeet/macos/ultra.tar.gz";

/// Full Windows archive URL: [`MODEL_DOWNLOAD_BASE`] plus [`PARAKEET_ULTRA_WINDOWS_ROUTE`].
pub const PARAKEET_ULTRA_WINDOWS_URL: &str = concat!(
    "https://heardright-models.adrdsouza.workers.dev",
    "/parakeet/windows/ultra.tar.gz"
);

/// Full macOS archive URL: [`MODEL_DOWNLOAD_BASE`] plus [`PARAKEET_ULTRA_MACOS_ROUTE`].
pub const PARAKEET_ULTRA_MACOS_URL: &str = concat!(
    "https://heardright-models.adrdsouza.workers.dev",
    "/parakeet/macos/ultra.tar.gz"
);

/// The attribution line the archive's `NOTICE.txt` carries (the file holds more; its own
/// size and SHA-256 are pinned like any other file, this text is not a pin).
pub const PARAKEET_ULTRA_NOTICE: &str =
    "moondream/parakeet-ultra, CC-BY-4.0, based on NVIDIA parakeet-tdt-0.6b-v3";

/// The store ids Ultra supersedes: the Parakeet TDT v3 entries of both platforms. Ultra is not in
/// [`all_models`], so the store reads this list directly when it prunes after a release.
pub const PARAKEET_ULTRA_RETIRED_STORE_IDS: &[&str] = &[
    "parakeet_tdt_v3_static1500_qint8_20260722",
    "parakeet-tdt-v3-coreml-compiled-5f8888117f47",
];

/// Files the Ultra entry must hold on this platform: exactly what a pins table must list, no
/// more and no fewer. (An archive may carry extras such as a `manifest.json`; they are never
/// listed, so they are never verified, and the store writes its own `manifest.json` over any
/// that the archive carries.)
#[cfg(not(target_os = "macos"))]
pub const PARAKEET_ULTRA_REQUIRED_PATHS: &[&str] = &[
    "config.json",
    "encoder-model.onnx",
    "decoder_joint-model.onnx",
    "mel_nemo.onnx",
    "mel_nemo.onnx.data",
    "vocab.txt",
    "NOTICE.txt",
];

/// Files the Ultra entry must hold on this platform: exactly what a pins table must list, no
/// more and no fewer. Root metadata, then each compiled stage bundle's IR, `metadata.json`,
/// `coremldata.bin`, weights and analytics. The archive's own `manifest.json` is deliberately not
/// listed (the store writes its own `<entry>/manifest.json` record over it), as on Windows.
#[cfg(target_os = "macos")]
pub const PARAKEET_ULTRA_REQUIRED_PATHS: &[&str] = &[
    "vocab.txt",
    "pipeline.json",
    "NOTICE.txt",
    "MelSpectrogram.mlmodelc/model.mil",
    "MelSpectrogram.mlmodelc/metadata.json",
    "MelSpectrogram.mlmodelc/coremldata.bin",
    "MelSpectrogram.mlmodelc/weights/weight.bin",
    "MelSpectrogram.mlmodelc/analytics/coremldata.bin",
    "AudioEncoder.mlmodelc/model.mil",
    "AudioEncoder.mlmodelc/metadata.json",
    "AudioEncoder.mlmodelc/coremldata.bin",
    "AudioEncoder.mlmodelc/weights/weight.bin",
    "AudioEncoder.mlmodelc/analytics/coremldata.bin",
    "TextDecoder.mlmodelc/model.mil",
    "TextDecoder.mlmodelc/metadata.json",
    "TextDecoder.mlmodelc/coremldata.bin",
    "TextDecoder.mlmodelc/weights/weight.bin",
    "TextDecoder.mlmodelc/analytics/coremldata.bin",
    "Joint.mlmodelc/model.mil",
    "Joint.mlmodelc/metadata.json",
    "Joint.mlmodelc/coremldata.bin",
    "Joint.mlmodelc/weights/weight.bin",
    "Joint.mlmodelc/analytics/coremldata.bin",
];

/// Every pin the Ultra archive spec needs, all required. There is deliberately no `Default` and
/// no optional field: a partial pin cannot be built.
#[derive(Debug, Clone, Copy)]
pub struct ParakeetUltraPins {
    /// Lowercase-hex SHA-256 of the whole `.tar.gz` (the bytes the worker serves).
    pub archive_sha256: &'static str,
    /// Size of the whole `.tar.gz` in bytes; the download is capped at it and checked exactly.
    pub archive_size: u64,
    /// Per-file size and SHA-256 of the extracted entry: exactly
    /// [`PARAKEET_ULTRA_REQUIRED_PATHS`] for the platform. Each entry must carry both pins.
    pub files: &'static [SpecFile],
    /// Installed size in bytes: the sum of `files`' sizes.
    pub installed_size_bytes: u64,
}

const PARAKEET_ULTRA_WINDOWS_FILES: &[SpecFile] = &[
    SpecFile {
        path: "config.json",
        sha256: Some("0cfca69c4ae6930ecfa1ccc863f846a3e5a40e120d858161c31ece009dbe9734"),
        size: Some(125),
    },
    SpecFile {
        path: "encoder-model.onnx",
        sha256: Some("e722a24615f5467496eea50a3f1cbb43636b7a0ec01999a1bfbe1656aaf5fd7b"),
        size: Some(343_814_723),
    },
    SpecFile {
        path: "decoder_joint-model.onnx",
        sha256: Some("f974081fc490ad54bd0f18e6be4f1963b174a0763204481fb2da062d2bdf83cb"),
        size: Some(18_201_991),
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
    SpecFile {
        path: "NOTICE.txt",
        sha256: Some("4468170d6241441b36b964ec096602b8d6920e70cdbc7c334bd2843c1c1b67d8"),
        size: Some(688),
    },
];

/// Windows Ultra pins: `parakeet_tdt_ultra_static1500_fp16int4_20261022.tar.gz`, sealed by the
/// archive owner 2026-10-22 (issue #897), top directory
/// `parakeet_tdt_ultra_static1500_fp16int4_20261022/`. The archive also carries a
/// 1,252-byte `manifest.json` that is deliberately not listed (the store writes its own).
pub const PARAKEET_ULTRA_WINDOWS_PINS: Option<ParakeetUltraPins> = Some(ParakeetUltraPins {
    archive_sha256: "0ad7f5a2a6911ccb5315c53602f2ab14a37aa9ffd8446b2e69d20a73745d59b9",
    archive_size: 316_610_098,
    files: PARAKEET_ULTRA_WINDOWS_FILES,
    installed_size_bytes: 363_322_003,
});

const PARAKEET_ULTRA_MACOS_FILES: &[SpecFile] = &[
    SpecFile {
        path: "vocab.txt",
        sha256: Some("5b133ab19bf7bf03e06e61a193d2dedb96f72677850a670231bb7344c2e09d4c"),
        size: Some(54_078),
    },
    SpecFile {
        path: "pipeline.json",
        sha256: Some("df6edfe91891d0516761ab72ad6b4f27ddf4ee23d5c867b2d6132b30129cacb5"),
        size: Some(908),
    },
    SpecFile {
        path: "NOTICE.txt",
        sha256: Some("5dad4bacd0bfdd19a4bbe88a82b5c176bed8dd53669811b3c895a2216d91f1c3"),
        size: Some(700),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/model.mil",
        sha256: Some("044cf490cb442ba79d446505a13d180082248bf239d58140e451c8d6f314e682"),
        size: Some(1_835_964),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/metadata.json",
        sha256: Some("223a3cb0060d72cf86356712811d3e96bdd50dcecde76d0bc72471f507b25732"),
        size: Some(2_512),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/coremldata.bin",
        sha256: Some("67f852ced7415e6aded98a18a8edb5409d0f7753f72c9cd3e6edd744f16d95d8"),
        size: Some(401),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/weights/weight.bin",
        sha256: Some("f3c3d600f5fc6311ceff6406c10aeb4285a7f0fda4de6ac86b5bee3e76f80e32"),
        size: Some(976_704),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/analytics/coremldata.bin",
        sha256: Some("837a25d437aff742e4b07b0dd8bb569bceaa9ed3159cc6d55a967d091aa63d17"),
        size: Some(243),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/model.mil",
        sha256: Some("35e76d58412c18ff20f8fc946f751f8caaec191a7d17d81f052eb2093b604930"),
        size: Some(978_977),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/metadata.json",
        sha256: Some("38231e5437b91ae01ae151871f7f0b34f5bf5f8a66b2ad30a6bcc90b74abc283"),
        size: Some(2_499),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/coremldata.bin",
        sha256: Some("27b5f9420e60909b6fd15d4858b38ac892268368b90a8af0407326f0b85e8390"),
        size: Some(351),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/weights/weight.bin",
        sha256: Some("7ef6926571fbc9f812034f5b3fac5d05a41871eb35c702f671a117781fb46f25"),
        size: Some(410_797_632),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/analytics/coremldata.bin",
        sha256: Some("ed68cea24c04219b2f441d5975bdfb65a56cae7675ffdf6dab1739f600a747cc"),
        size: Some(243),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/model.mil",
        sha256: Some("b312e8f9550241072894d077b98a1f7bbacd55ee915d539013f44df0be4c00e7"),
        size: Some(15_692),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/metadata.json",
        sha256: Some("af0378a73304f3e2294132b8871c033106c214a6740a0dde1ae1e03f2dccc72f"),
        size: Some(3_006),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/coremldata.bin",
        sha256: Some("cde46f5fe167ca24c131e66fe847b20c211b502d57f1ba6f5bcfce3a87bec3e2"),
        size: Some(418),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/weights/weight.bin",
        sha256: Some("a98cb2eb85da7d2a1e1e7d298e1769f27998bfba2c67b2281714b7aa5f70dd08"),
        size: Some(11_856_064),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/analytics/coremldata.bin",
        sha256: Some("a1c4716bcccb2a9f9da122247ccaef2ccc56786c7b364f6c0b5c19dd1a755626"),
        size: Some(243),
    },
    SpecFile {
        path: "Joint.mlmodelc/model.mil",
        sha256: Some("83a75420a420b00c34da8be7a8d332ab89b66d705669f918faec4465c9a13096"),
        size: Some(5_740),
    },
    SpecFile {
        path: "Joint.mlmodelc/metadata.json",
        sha256: Some("7daea82237542055e477003acac31d94f6a5a8649c48c766e30431c93f1ea906"),
        size: Some(2_020),
    },
    SpecFile {
        path: "Joint.mlmodelc/coremldata.bin",
        sha256: Some("5a2ce6cefb422f1b9fd2af733af462c84ec7257bc6162d632370dd457ba42bb4"),
        size: Some(346),
    },
    SpecFile {
        path: "Joint.mlmodelc/weights/weight.bin",
        sha256: Some("003db529d2952645b0b36670e8b3fcc1e38f72e5b3885e96e5361520dc574158"),
        size: Some(6_359_308),
    },
    SpecFile {
        path: "Joint.mlmodelc/analytics/coremldata.bin",
        sha256: Some("04360f333e3a4cd45d8a69f98f41a3b013e50e1fefc225ff8e023bc9c036723f"),
        size: Some(243),
    },
];

/// macOS Ultra pins: `parakeet-tdt-ultra.tar.gz`, sealed 2026-10-22 (issue #897), top directory
/// `parakeet-tdt-ultra/`, 24 members. The archive's 3,977-byte `manifest.json` is deliberately
/// not listed (the store writes its own `<entry>/manifest.json` record), leaving the 23 files of
/// [`PARAKEET_ULTRA_REQUIRED_PATHS`].
pub const PARAKEET_ULTRA_MACOS_PINS: Option<ParakeetUltraPins> = Some(ParakeetUltraPins {
    archive_sha256: "23610964221771e5a608c76d87d57802aecdacb549e27127c4d03b8feac2d4ee",
    archive_size: 391_192_167,
    files: PARAKEET_ULTRA_MACOS_FILES,
    installed_size_bytes: 432_894_292,
});

/// The Ultra pins for this platform: [`PARAKEET_ULTRA_MACOS_PINS`] on macOS,
/// [`PARAKEET_ULTRA_WINDOWS_PINS`] on Windows, `None` elsewhere (no Ultra route exists).
#[cfg(target_os = "macos")]
pub const PARAKEET_ULTRA_PINS: Option<ParakeetUltraPins> = PARAKEET_ULTRA_MACOS_PINS;
/// The Ultra pins for this platform: [`PARAKEET_ULTRA_MACOS_PINS`] on macOS,
/// [`PARAKEET_ULTRA_WINDOWS_PINS`] on Windows, `None` elsewhere (no Ultra route exists).
#[cfg(windows)]
pub const PARAKEET_ULTRA_PINS: Option<ParakeetUltraPins> = PARAKEET_ULTRA_WINDOWS_PINS;
/// The Ultra pins for this platform: [`PARAKEET_ULTRA_MACOS_PINS`] on macOS,
/// [`PARAKEET_ULTRA_WINDOWS_PINS`] on Windows, `None` elsewhere (no Ultra route exists).
#[cfg(not(any(target_os = "macos", windows)))]
pub const PARAKEET_ULTRA_PINS: Option<ParakeetUltraPins> = None;

#[cfg(target_os = "macos")]
const PARAKEET_ULTRA_URL: Option<&str> = Some(PARAKEET_ULTRA_MACOS_URL);
#[cfg(windows)]
const PARAKEET_ULTRA_URL: Option<&str> = Some(PARAKEET_ULTRA_WINDOWS_URL);
#[cfg(not(any(target_os = "macos", windows)))]
const PARAKEET_ULTRA_URL: Option<&str> = None;

/// The Parakeet Ultra pins for this platform are not in the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingParakeetUltraPins;

impl std::fmt::Display for MissingParakeetUltraPins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "the Parakeet Ultra archive SHA-256, size and file pins are not recorded for this platform (Orthic-Labs/rightkit#897)",
        )
    }
}

impl std::error::Error for MissingParakeetUltraPins {}

fn ultra_pins_are_well_formed(pins: &ParakeetUltraPins) -> bool {
    let hex = |s: &str| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let mut paths: Vec<&str> = pins.files.iter().map(|f| f.path).collect();
    paths.sort_unstable();
    let mut required: Vec<&str> = PARAKEET_ULTRA_REQUIRED_PATHS.to_vec();
    required.sort_unstable();
    hex(pins.archive_sha256)
        && pins.archive_size > 0
        // Exactly the required layout: a missing file, an extra file or a duplicate is refused.
        && paths == required
        && pins
            .files
            .iter()
            .all(|f| f.sha256.is_some_and(hex) && f.size.is_some_and(|n| n > 0))
        && pins.files.iter().filter_map(|f| f.size).sum::<u64>() == pins.installed_size_bytes
}

/// The Parakeet Ultra shared-store spec built from explicit pins: one grant-gated archive,
/// whole-archive SHA-256 and size checked by the download before extraction, no legacy
/// directories, retiring [`PARAKEET_ULTRA_RETIRED_STORE_IDS`]. Returns
/// [`MissingParakeetUltraPins`] when the pins are malformed (not 64 lowercase hex, a zero size,
/// a file without both pins, a file set that is not exactly [`PARAKEET_ULTRA_REQUIRED_PATHS`],
/// or an installed size that is not the sum of the file sizes). Not in [`base_catalog`] or
/// [`all_models`].
pub fn parakeet_ultra_spec_with(
    pins: ParakeetUltraPins,
) -> Result<ModelSpec, MissingParakeetUltraPins> {
    if !ultra_pins_are_well_formed(&pins) {
        return Err(MissingParakeetUltraPins);
    }
    Ok(ModelSpec {
        id: PARAKEET_ULTRA_ID,
        label: "Parakeet Ultra speech model",
        tier: ModelTier::Base,
        entitlement: false,
        version: pins.archive_sha256,
        size_hint_bytes: pins.installed_size_bytes,
        files: pins.files,
        source: SpecSource::Archive {
            url: PARAKEET_ULTRA_URL,
            sha256: Some(pins.archive_sha256),
            size: Some(pins.archive_size),
            requires_auth: true,
        },
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: PARAKEET_ULTRA_RETIRED_STORE_IDS,
    })
}

/// The Parakeet Ultra spec from [`PARAKEET_ULTRA_PINS`]. `Err(MissingParakeetUltraPins)` on a
/// platform whose pins are `None` (Linux; both macOS and Windows are pinned),
/// so no caller can download an unpinned archive. `parakeet_spec` is unchanged.
pub fn parakeet_ultra_spec() -> Result<ModelSpec, MissingParakeetUltraPins> {
    match PARAKEET_ULTRA_PINS {
        Some(pins) => parakeet_ultra_spec_with(pins),
        None => Err(MissingParakeetUltraPins),
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
/// Not in [`base_catalog`] (not every install needs it) and not in [`add_ons`] (adopt-only: no
/// route is served, so there is nothing to offer for download). Reach it with this function.
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

// ---------------------------------------------------------------------------
// EmbeddingGemma 2, suite model: Core ML build (macOS, an add-on) and E40 fp16 DirectML
// export (Windows, reached only by its own function). Both are per-file bundles pinned by
// size and SHA-256.
// ---------------------------------------------------------------------------

/// Expands one table of `(path, url, sha256, bytes)` rows into the matching
/// `SpecFile` and `RemoteFile` constants, so the two can never disagree.
macro_rules! pinned_files {
    ($spec:ident, $remote:ident; $(($path:literal, $url:expr, $sha:literal, $size:literal)),+ $(,)?) => {
        pub const $spec: &[SpecFile] = &[
            $(SpecFile { path: $path, sha256: Some($sha), size: Some($size) }),+
        ];
        pub const $remote: &[RemoteFile] = &[
            $(RemoteFile { path: $path, url: Some($url), sha256: Some($sha), size: Some($size) }),+
        ];
    };
}

/// Store id and entry dir name of the Core ML build. Equal to
/// `rightkit_embed::EMBEDDINGGEMMA2_COREML_MODEL_ID` (no dependency on rightkit-embed).
pub const EMBEDDINGGEMMA2_COREML_ID: &str = "embeddinggemma2-coreml";

/// Hugging Face commit of `FluidInference/embeddinggemma-2-coreml` every Core ML URL and
/// pin below was taken from (repository `sha` and the tree listing at that commit, 2026-10-16).
pub const EMBEDDINGGEMMA2_COREML_COMMIT: &str = "3f978ddf92cac9aa07eb2dcf6eb36cf450f3dba4";

macro_rules! coreml_url {
    ($path:literal) => {
        concat!(
            "https://huggingface.co/FluidInference/embeddinggemma-2-coreml/resolve/",
            "3f978ddf92cac9aa07eb2dcf6eb36cf450f3dba4/",
            $path
        )
    };
}

// The text encoder only. The repository also holds `EmbeddingGemma2Audio.mlpackage` (561 MB),
// `EmbeddingGemma2Vision.mlpackage` (292 MB), `position_embeddings.f16` and a README; none is
// needed for text embeddings and none is listed. An `.mlpackage` is a directory: its three
// files are listed by their path inside it. LFS files carry the SHA-256 the Hub records
// (`lfs.oid`); `Manifest.json` and `config.json` are not LFS, so their SHA-256 was computed
// over the bytes served at the commit.
// Total 584,748,275 bytes (557.7 MiB).
pinned_files! {
    EMBEDDINGGEMMA2_COREML_FILES, EMBEDDINGGEMMA2_COREML_REMOTE_FILES;
    (
        "EmbeddingGemma2Text.mlpackage/Manifest.json",
        coreml_url!("EmbeddingGemma2Text.mlpackage/Manifest.json"),
        "83eace80b69b46f8415af620ba1b40d7c490d00f7c00ba9dedd2e29da6e9ac19",
        617
    ),
    (
        "EmbeddingGemma2Text.mlpackage/Data/com.apple.CoreML/model.mlmodel",
        coreml_url!("EmbeddingGemma2Text.mlpackage/Data/com.apple.CoreML/model.mlmodel"),
        "0df9aca02807cf490db6d4f1c68607a8c7f486ad23b376d1968fd3be5eb14677",
        7_323_423
    ),
    (
        "EmbeddingGemma2Text.mlpackage/Data/com.apple.CoreML/weights/weight.bin",
        coreml_url!("EmbeddingGemma2Text.mlpackage/Data/com.apple.CoreML/weights/weight.bin"),
        "75b97ab036ca87eae60560c4034a63d604201498dc4b6f6971dd0b1e4558d61f",
        276_815_424
    ),
    (
        "embeddings.bf16",
        coreml_url!("embeddings.bf16"),
        "56ebbdbfd706c827f3fae1d97aa39135c7ddc0f1e6d9b12f8565f84bbfbda877",
        268_435_456
    ),
    (
        "tokenizer.json",
        coreml_url!("tokenizer.json"),
        "4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4",
        32_170_510
    ),
    (
        "config.json",
        coreml_url!("config.json"),
        "30668aba89f54a200a3fa38913f21e2a2f18aba7215de67c1806542a75b9629b",
        2_845
    ),
}

/// Total bytes of [`EMBEDDINGGEMMA2_COREML_FILES`].
pub const EMBEDDINGGEMMA2_COREML_SIZE: u64 = 584_748_275;

/// EmbeddingGemma 2 Core ML shared-store spec (text encoder, macOS only; Apache-2.0 per the
/// repository card). An add-on: in [`add_ons`] and [`all_models`] on macOS (so it is reachable
/// by id through [`spec_by_id`] and `ModelStore::ensure_by_id`), never in [`base_catalog`]; the
/// app that needs it ensures it on demand. Download URLs are live (Hugging Face, pinned to a commit). Running the model needs a Core ML
/// binding that `rightkit-embed` does not have yet (see its README), so this entry only
/// stages the files.
#[cfg(target_os = "macos")]
pub fn embedding_gemma2_coreml_spec() -> ModelSpec {
    ModelSpec {
        id: EMBEDDINGGEMMA2_COREML_ID,
        label: "EmbeddingGemma 2 Core ML embedding model",
        tier: ModelTier::AddOn,
        entitlement: false,
        version: "3f978dd",
        size_hint_bytes: EMBEDDINGGEMMA2_COREML_SIZE,
        files: EMBEDDINGGEMMA2_COREML_FILES,
        source: SpecSource::Files(EMBEDDINGGEMMA2_COREML_REMOTE_FILES),
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

/// Store id and entry dir name of the Windows E40 export. Equal to
/// `rightkit_embed::EMBEDDINGGEMMA2_E40_MODEL_ID`.
pub const EMBEDDINGGEMMA2_E40_DML_ID: &str = "embeddinggemma2-e40-dml";

/// Revision label of the E40 export in the route path and the manifest version: the pinned
/// upstream source revision (`onnx-community/embeddinggemma-2-ONNX` @ `daa72c5`) and the
/// variant. Scripts: `scripts/embed-export/eg2-directml/README.md`.
pub const EMBEDDINGGEMMA2_E40_DML_REVISION: &str = "e40-daa72c5";

// ROUTE PENDING. The URLs below follow the Parakeet layout on the model worker
// (`<MODEL_DOWNLOAD_BASE>/<model>/<...>/<file>`), here
// `/embeddinggemma2-e40-dml/e40-daa72c5/<file>`. The worker
// (`rightsites/workers/heardright-models`) has no such route and the files have not been
// uploaded, so a download answers 404 until both exist. The pins are real: they are the
// expected-output table of the export README (one reproducible build) plus the upstream
// `tokenizer.json`. The route is public-read like the OCR files (no bearer), which is also
// still to be decided with the worker.
macro_rules! e40_url {
    ($file:literal) => {
        concat!(
            "https://heardright-models.adrdsouza.workers.dev",
            "/embeddinggemma2-e40-dml/e40-daa72c5/",
            $file
        )
    };
}

// Five fixed-length graphs (batch 1) sharing `model.onnx_data`, the fp16 token table
// for host-side lookup, and the tokenizer. `manifest.json` (build record, not byte-stable
// because it holds the build time) is not part of the bundle contract and is not listed.
// Total 592,894,933 bytes.
pinned_files! {
    EMBEDDINGGEMMA2_E40_DML_FILES, EMBEDDINGGEMMA2_E40_DML_REMOTE_FILES;
    (
        "model_32.onnx",
        e40_url!("model_32.onnx"),
        "8e1aaddd28ca5437e2da0da5f9dac5ea54766dc26b0bb760f104f5963c3bb418",
        416_097
    ),
    (
        "model_64.onnx",
        e40_url!("model_64.onnx"),
        "bcc8b63ea05c9963130cd2995be013f8dd516e40fc6bdac81b1313ea6ca5c2cc",
        416_179
    ),
    (
        "model_128.onnx",
        e40_url!("model_128.onnx"),
        "7afb92bf83831e3c496a993555f82f85afa9b107b77ef13297aa12d2f9d7e1f8",
        417_636
    ),
    (
        "model_256.onnx",
        e40_url!("model_256.onnx"),
        "1874b05842faf3c9b3badc6261765d95ad809a0ec2afc6f7dbb647aa161f8ce0",
        417_638
    ),
    (
        "model_512.onnx",
        e40_url!("model_512.onnx"),
        "f6fd91519cf1023da0adcedeab9fb7c77b01913f84ed647bb79d7f839994b741",
        417_641
    ),
    (
        "model.onnx_data",
        e40_url!("model.onnx_data"),
        "c398505134b2bb717becc109c00408ecd9faef0a8418981a549653aea6bb73ac",
        290_203_648
    ),
    (
        "embed_tokens.f16.npy",
        e40_url!("embed_tokens.f16.npy"),
        "b574d03a53a4cfbcf76d07dc291d2ffe614ca7720c79cb655ec338f583ee53e8",
        268_435_584
    ),
    (
        "tokenizer.json",
        e40_url!("tokenizer.json"),
        "4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4",
        32_170_510
    ),
}

/// Total bytes of [`EMBEDDINGGEMMA2_E40_DML_FILES`].
pub const EMBEDDINGGEMMA2_E40_DML_SIZE: u64 = 592_894_933;

/// EmbeddingGemma 2 E40 fp16 DirectML shared-store spec (Windows only). Reach it with this
/// function: not in [`base_catalog`] or [`add_ons`]. The download route is pending (see
/// [`EMBEDDINGGEMMA2_E40_DML_REVISION`] and the comment on the URLs): `can_download()` is true
/// because the URLs are set, but they are not served yet. Placing the eight files byte-identical
/// in the entry dir adopts them without a download.
#[cfg(windows)]
pub fn embedding_gemma2_e40_dml_spec() -> ModelSpec {
    ModelSpec {
        id: EMBEDDINGGEMMA2_E40_DML_ID,
        label: "EmbeddingGemma 2 DirectML embedding model",
        tier: ModelTier::AddOn,
        entitlement: false,
        version: EMBEDDINGGEMMA2_E40_DML_REVISION,
        size_hint_bytes: EMBEDDINGGEMMA2_E40_DML_SIZE,
        files: EMBEDDINGGEMMA2_E40_DML_FILES,
        source: SpecSource::Files(EMBEDDINGGEMMA2_E40_DML_REMOTE_FILES),
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    }
}

// ---------------------------------------------------------------------------
// EmbeddingGemma 2 E43 (int8 token table, batched ingest graphs) as ONE worker-served archive
// (Orthic-Labs/rightkit#224). The model choice is E43 with the int8 table (H01/H02, #288);
// `rightkit-embed` 0.6.0 reads this layout (`recipe: "E43"` manifest, see its `e40.rs`).
//
// The bundle is a single `.tar.gz` with ONE top-level wrapper directory
// (`embeddinggemma2-e43-dml/`, stripped on install), so the grant and the download verify ONE
// whole-archive SHA-256 and size before any byte is extracted. The 21 per-file pins below are
// final (archive owner, 2026-10-22, #224), `tokenizer.json` included, with the bundle manifest
// named `embed-manifest.json` so the store's own `manifest.json` record cannot replace it. The
// archive digest and size of the resealed `.tar.gz` are pending, so the pins stay `None`.
// ---------------------------------------------------------------------------

/// Store id and entry dir name of the E43 DirectML bundle. Equal to the id `rightkit-embed`
/// uses for the Windows bundle's entry dir (no dependency on rightkit-embed).
pub const EMBEDDINGGEMMA2_E43_DML_ID: &str = "embeddinggemma2-e43-dml";

/// Revision label of the E43 export in the route path and the manifest version. Only the
/// recipe name: the upstream source commit is not recorded in the repository for E43, so none
/// is claimed here.
pub const EMBEDDINGGEMMA2_E43_DML_REVISION: &str = "e43";

/// Worker route of the archive, following the Parakeet layout
/// (`<MODEL_DOWNLOAD_BASE>/<model>/<revision>/<archive>`). Public-read like the E40 and OCR
/// files (no bearer, so no grant is requested); the worker's `MODEL_SPECS` entry and the upload
/// are the worker owner's part of #224 and are not served by this crate.
pub const EMBEDDINGGEMMA2_E43_DML_ROUTE: &str =
    "/embeddinggemma2-e43-dml/e43/embeddinggemma2-e43-dml.tar.gz";

/// Full archive URL: [`MODEL_DOWNLOAD_BASE`] plus [`EMBEDDINGGEMMA2_E43_DML_ROUTE`].
pub const EMBEDDINGGEMMA2_E43_DML_URL: &str = concat!(
    "https://heardright-models.adrdsouza.workers.dev",
    "/embeddinggemma2-e43-dml/e43/embeddinggemma2-e43-dml.tar.gz"
);

/// Every pin the E43 archive spec needs, all required. There is deliberately no `Default`
/// and no optional field: a partial pin cannot be built.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddingGemma2E43Pins {
    /// Lowercase-hex SHA-256 of the whole `.tar.gz` (the bytes the worker serves).
    pub archive_sha256: &'static str,
    /// Size of the whole `.tar.gz` in bytes.
    pub archive_size: u64,
    /// Per-file size and SHA-256 of the extracted bundle: the query graphs, the batch-4 and
    /// batch-8 ingest graphs, `model.onnx_data`, `embed_tokens.i8.npy`, `embed_scales.f16.npy`
    /// and `tokenizer.json`. Each entry must carry both pins.
    pub files: &'static [SpecFile],
    /// Installed size in bytes (sum of `files`), for "downloads X MB" copy.
    pub installed_size_bytes: u64,
}

/// The 21 per-file pins of the E43 bundle (the resealed layout, archive owner 2026-10-22, #224),
/// exactly what `rightkit-embed` reads: ten query graphs, six batch-4/8 ingest graphs,
/// `model.onnx_data`, the int8 table with its scales, `tokenizer.json`, and the schema-2 bundle
/// manifest under the name `embed-manifest.json` (the suite store writes its own entry record to
/// `manifest.json`, which would replace a bundle manifest of that name; `rightkit-embed` 0.6.1
/// reads `embed-manifest.json` first). Sorted by path; 494,393,592 bytes in all
/// ([`EMBEDDINGGEMMA2_E43_DML_INSTALLED_SIZE`]).
pub const EMBEDDINGGEMMA2_E43_DML_FILES: &[SpecFile] = &[
    SpecFile {
        path: "embed_scales.f16.npy",
        sha256: Some("02054cb67b74594d34376066082ba799d479153f3c8aa81819754f3821621a54"),
        size: Some(524_416),
    },
    SpecFile {
        path: "embed_tokens.i8.npy",
        sha256: Some("20552c4c9ac12e97024df2d0e4f045169d53b630f3314e59577291174c4f8398"),
        size: Some(134_217_856),
    },
    SpecFile {
        path: "ingest-b4_128.onnx",
        sha256: Some("063d2f26f9147028127d6222b1717fd1bf55503d4490de516da0fb00526cc3d4"),
        size: Some(417_636),
    },
    SpecFile {
        path: "ingest-b4_32.onnx",
        sha256: Some("eb4d33be7ba1eedd52978441f5f237781b4779eac13af07ddc78729b0ae35fe4"),
        size: Some(940_858),
    },
    SpecFile {
        path: "ingest-b4_512.onnx",
        sha256: Some("37d8cc88427a66a6dcf042300ba0e1302e93f16da9abfcd8b595a9a4175b3708"),
        size: Some(9_067_836),
    },
    SpecFile {
        path: "ingest-b8_128.onnx",
        sha256: Some("9cfdc7e9c90fc2ab45764c19187e97993e40f9cf7dc10ab3f917a9f71aba4436"),
        size: Some(417_636),
    },
    SpecFile {
        path: "ingest-b8_32.onnx",
        sha256: Some("a669dee404d8c0b7e12026ee781bc6ddbf562cb9a9a8c4776a78ca84da1d82cc"),
        size: Some(940_858),
    },
    SpecFile {
        path: "ingest-b8_512.onnx",
        sha256: Some("8aca9485b1ecf1ee3c21438f63f3db2d90604049772da0cecd7225e7ce4915f6"),
        size: Some(9_067_836),
    },
    SpecFile {
        path: "embed-manifest.json",
        sha256: Some("6b6c02a1d78c006d9632710432b34bf7a853a1a9b9d8874677c799c462c7c509"),
        size: Some(1_041),
    },
    SpecFile {
        path: "model.onnx_data",
        sha256: Some("ea5d1697d42f9f1020fb7a728fc6c207a168d568e4f637c431fec97b4c70ee08"),
        size: Some(302_458_112),
    },
    SpecFile {
        path: "model_128.onnx",
        sha256: Some("bb94a188d8c07cb8a7f07c5c260cc1ca9d3318e308a12e40f8c71eefc51a6bb1"),
        size: Some(417_636),
    },
    SpecFile {
        path: "model_16.onnx",
        sha256: Some("40baf986d261f1352b984a8c45c3c19c77262f30de685419ddb6533bb412f312"),
        size: Some(416_255),
    },
    SpecFile {
        path: "model_192.onnx",
        sha256: Some("121d8df8279c4b59bae92d73566161bb9a4b6964771f3d07fd091e26187ee4eb"),
        size: Some(417_636),
    },
    SpecFile {
        path: "model_256.onnx",
        sha256: Some("5c8cf7ecf8ac1a91f7627e6c69c40a7b4c6c74b0d4348ba9eeec47142491ecf0"),
        size: Some(417_638),
    },
    SpecFile {
        path: "model_32.onnx",
        sha256: Some("09d60eb1cf60102735f1c3acba42babe9bc1bf8ee10dcebbed319fd38870e688"),
        size: Some(416_097),
    },
    SpecFile {
        path: "model_384.onnx",
        sha256: Some("6499768325ec5b5e8f307eb1bbd24d1888667707b928955b36dbabd159448203"),
        size: Some(417_639),
    },
    SpecFile {
        path: "model_48.onnx",
        sha256: Some("38f705fcaabf58596f379337b4af95a14354022612163d2dbb9b499af0068def"),
        size: Some(416_097),
    },
    SpecFile {
        path: "model_512.onnx",
        sha256: Some("3b3ea55e35030ba6d9f35e32384df4f3c3505b0a302da8d7892ac1fac88357da"),
        size: Some(417_641),
    },
    SpecFile {
        path: "model_64.onnx",
        sha256: Some("d424b335a5bc7fa84e16b8182424b82f2d0204f726822ac4c934c5e9ca777ee8"),
        size: Some(416_179),
    },
    SpecFile {
        path: "model_96.onnx",
        sha256: Some("8aa8597ddaf2872a6a559289fe4ad53881d4269b0a60ac86b34d2ccba58ea27a"),
        size: Some(416_179),
    },
    SpecFile {
        path: "tokenizer.json",
        sha256: Some("4d777ef5bdc1aa36227abdfb77c3e49e7b9c892d16e1b6bda41c393504828be4"),
        size: Some(32_170_510),
    },
];

/// Sum of the sizes of [`EMBEDDINGGEMMA2_E43_DML_FILES`] (the installed size).
pub const EMBEDDINGGEMMA2_E43_DML_INSTALLED_SIZE: u64 = 494_393_592;

/// The pins of the E43 archive `embeddinggemma2-e43-dml.tar.gz` as sealed by the archive owner on
/// 2026-10-22 (bogusyogi/rightkit#224): one wrapper directory `embeddinggemma2-e43-dml/`, the 21
/// files of [`EMBEDDINGGEMMA2_E43_DML_FILES`] with `embed-manifest.json` as the bundle manifest,
/// deterministic tar. The earlier archives (the plain `.tar` `c8ad75d9…`, the flat `.tar.gz`
/// `9b892851…` and the `manifest.json` `.tar.gz` `54f32f8e…`) are superseded and must not be
/// served at the route.
pub const EMBEDDINGGEMMA2_E43_DML_PINS: Option<EmbeddingGemma2E43Pins> =
    Some(EmbeddingGemma2E43Pins {
        archive_sha256: "b181f45c201be7068d5b1c011da6a5350c14c343a3b75707931ad8baadf2d128",
        archive_size: 370_860_871,
        files: EMBEDDINGGEMMA2_E43_DML_FILES,
        installed_size_bytes: EMBEDDINGGEMMA2_E43_DML_INSTALLED_SIZE,
    });

/// The E43 archive pins are missing or malformed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MissingE43Pins;

impl std::fmt::Display for MissingE43Pins {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(
            "the EmbeddingGemma 2 E43 archive SHA-256, size and file pins are not recorded (Orthic-Labs/rightkit#224)",
        )
    }
}

impl std::error::Error for MissingE43Pins {}

fn pins_are_well_formed(pins: &EmbeddingGemma2E43Pins) -> bool {
    let hex = |s: &str| s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    hex(pins.archive_sha256)
        && pins.archive_size > 0
        && !pins.files.is_empty()
        && pins
            .files
            .iter()
            .all(|f| f.sha256.is_some_and(hex) && f.size.is_some_and(|n| n > 0))
}

/// The E43 DirectML shared-store spec built from explicit pins: one archive, whole-archive
/// SHA-256 and size checked by the download before extraction, public-read. Returns
/// [`MissingE43Pins`] when the pins are malformed (not 64 lowercase hex, a zero size, a file
/// without both pins). Not in [`base_catalog`] or [`add_ons`].
pub fn embedding_gemma2_e43_dml_spec_with(
    pins: EmbeddingGemma2E43Pins,
) -> Result<ModelSpec, MissingE43Pins> {
    if !pins_are_well_formed(&pins) {
        return Err(MissingE43Pins);
    }
    Ok(ModelSpec {
        id: EMBEDDINGGEMMA2_E43_DML_ID,
        label: "EmbeddingGemma 2 DirectML embedding model",
        tier: ModelTier::AddOn,
        entitlement: false,
        version: EMBEDDINGGEMMA2_E43_DML_REVISION,
        size_hint_bytes: pins.installed_size_bytes,
        files: pins.files,
        source: SpecSource::Archive {
            url: Some(EMBEDDINGGEMMA2_E43_DML_URL),
            sha256: Some(pins.archive_sha256),
            size: Some(pins.archive_size),
            requires_auth: false,
        },
        legacy_dirs: &[],
        retired_legacy_paths: &[],
        retired_store_ids: &[],
    })
}

/// The E43 DirectML spec from [`EMBEDDINGGEMMA2_E43_DML_PINS`]. `Err(MissingE43Pins)` until
/// those pins are recorded, so no caller can download an unpinned archive.
pub fn embedding_gemma2_e43_dml_spec() -> Result<ModelSpec, MissingE43Pins> {
    match EMBEDDINGGEMMA2_E43_DML_PINS {
        Some(pins) => embedding_gemma2_e43_dml_spec_with(pins),
        None => Err(MissingE43Pins),
    }
}

/// The EmbeddingGemma 2 accelerator-bundle specs this platform can hold: the Core ML build on
/// macOS, the E40 DirectML export on Windows, none elsewhere.
#[cfg(target_os = "macos")]
pub fn embedding_gemma2_platform_specs() -> Vec<ModelSpec> {
    vec![embedding_gemma2_coreml_spec()]
}

/// The EmbeddingGemma 2 accelerator-bundle specs this platform can hold (Windows: E40).
#[cfg(windows)]
pub fn embedding_gemma2_platform_specs() -> Vec<ModelSpec> {
    vec![embedding_gemma2_e40_dml_spec()]
}

/// The EmbeddingGemma 2 accelerator-bundle specs this platform can hold (none here).
#[cfg(not(any(target_os = "macos", windows)))]
pub fn embedding_gemma2_platform_specs() -> Vec<ModelSpec> {
    Vec::new()
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

/// Optional downloads, each offered on demand and never part of setup: the entitlement-gated
/// Whisper large-v3-turbo, and on macOS the EmbeddingGemma 2 Core ML text encoder
/// (`embedding_gemma2_coreml_spec`, macOS only, public-read, no entitlement). The app that needs an
/// add-on ensures it on demand; an add-on is never in [`base_catalog`], so a base install does
/// not download it.
pub fn add_ons() -> Vec<ModelSpec> {
    #[allow(unused_mut)]
    let mut specs = vec![whisper_turbo_ggml_spec()];
    #[cfg(target_os = "macos")]
    specs.push(embedding_gemma2_coreml_spec());
    specs
}

/// The spec in [`all_models`] with store id `id` (for example [`EMBEDDINGGEMMA2_COREML_ID`] on
/// macOS), or `None`. Specs that are reached only through their own pin-gated function
/// ([`parakeet_ultra_spec`], the E43 and Q4 EmbeddingGemma specs) are not in `all_models` and
/// are not found here.
pub fn spec_by_id(id: &str) -> Option<ModelSpec> {
    all_models().into_iter().find(|spec| spec.id == id)
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
