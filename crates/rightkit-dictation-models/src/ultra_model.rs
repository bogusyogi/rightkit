//! Parakeet Ultra release pin: ids, worker routes and archive checksums.
//!
//! Ultra is `moondream/parakeet-ultra` (CC-BY-4.0, based on NVIDIA
//! `parakeet-tdt-0.6b-v3`) running on the app's Parakeet TDT v3 graphs. It is
//! the current ASR model on Windows and macOS; every earlier TDT id is retired
//! (`model_paths::RETIRED_TDT_MODELS`).
//!
//! A checksum that is not valid hex (a placeholder) makes `pinned_sha256` return
//! `None`, and the download, the grant request and the replacement prompt all
//! fail closed: nothing is fetched and no retired model is touched.
//!
//! The app owns the Ultra `ModelSpec` (`spec()`): the per-file pins, the
//! archive URL and the archive checksum live here, not in the `rightkit-models`
//! built-in catalog (which still describes Parakeet TDT v3 in 0.4.x). The shared
//! store therefore downloads, migrates and activates Ultra without waiting for a
//! catalog release. `check_catalog` stays as the fail-closed gate between the
//! spec the store is about to use and the pinned archive checksum.

use rightkit_models::suite::catalog::{ModelTier, SpecFile, SpecSource};
use rightkit_models::suite::ModelSpec;

/// Attribution string for notices and the archive's NOTICE.txt.
pub const ULTRA_ATTRIBUTION: &str = "moondream/parakeet-ultra, CC-BY-4.0";

/// Model id: install subdir and shared-store id (identical on each platform).
pub const ULTRA_WINDOWS_ID: &str = "parakeet_tdt_ultra_static1500_fp16int4_20261022";
pub const ULTRA_MACOS_ID: &str = "parakeet-tdt-ultra";

/// Worker download routes: the live heardright-models worker serves fixed
/// routes (`/parakeet/{platform}/<name>.tar.gz`) and matches the grant on
/// route + model + platform + sha256; Ultra lives beside the tdt routes.
pub const ULTRA_WINDOWS_ROUTE: &str = "/parakeet/windows/ultra.tar.gz";
pub const ULTRA_MACOS_ROUTE: &str = "/parakeet/macos/ultra.tar.gz";

/// SHA-256 (64 hex) of the whole release archive.
pub const ULTRA_WINDOWS_SHA256: &str =
    "0ad7f5a2a6911ccb5315c53602f2ab14a37aa9ffd8446b2e69d20a73745d59b9";
pub const ULTRA_MACOS_SHA256: &str =
    "23610964221771e5a608c76d87d57802aecdacb549e27127c4d03b8feac2d4ee";

/// Archive size in bytes (also pinned in `spec()`'s archive source).
pub const ULTRA_WINDOWS_ARCHIVE_BYTES: Option<u64> = Some(316_610_098);
pub const ULTRA_MACOS_ARCHIVE_BYTES: Option<u64> = Some(391_192_167);

/// Files that must exist in a complete Windows Ultra directory. The archive
/// carries exactly these (plus NOTICE.txt); keep in step with `spec()`.
pub const ULTRA_WINDOWS_REQUIRED_FILES: &[&str] = &[
    "config.json",
    "encoder-model.onnx",
    "decoder_joint-model.onnx",
    "mel_nemo.onnx",
    "mel_nemo.onnx.data",
    "vocab.txt",
];

#[cfg(target_os = "macos")]
pub const ULTRA_ID: &str = ULTRA_MACOS_ID;
#[cfg(not(target_os = "macos"))]
pub const ULTRA_ID: &str = ULTRA_WINDOWS_ID;

#[cfg(target_os = "macos")]
pub const ULTRA_ROUTE: &str = ULTRA_MACOS_ROUTE;
#[cfg(not(target_os = "macos"))]
pub const ULTRA_ROUTE: &str = ULTRA_WINDOWS_ROUTE;

#[cfg(target_os = "macos")]
pub const ULTRA_ARCHIVE_BYTES: Option<u64> = ULTRA_MACOS_ARCHIVE_BYTES;
#[cfg(not(target_os = "macos"))]
pub const ULTRA_ARCHIVE_BYTES: Option<u64> = ULTRA_WINDOWS_ARCHIVE_BYTES;

#[cfg(target_os = "macos")]
const ULTRA_SHA256: &str = ULTRA_MACOS_SHA256;
#[cfg(not(target_os = "macos"))]
const ULTRA_SHA256: &str = ULTRA_WINDOWS_SHA256;

#[cfg(not(target_os = "macos"))]
const ULTRA_ARCHIVE_URL: &str = concat!(
    "https://heardright-models.adrdsouza.workers.dev",
    "/parakeet/windows/ultra.tar.gz"
);
#[cfg(target_os = "macos")]
const ULTRA_ARCHIVE_URL: &str = concat!(
    "https://heardright-models.adrdsouza.workers.dev",
    "/parakeet/macos/ultra.tar.gz"
);

/// Per-file pins from the `manifest.json` inside the Windows release archive
/// (`manifest.json` itself is excluded: the store never hashes it).
#[cfg(not(target_os = "macos"))]
const ULTRA_FILES: &[SpecFile] = &[
    SpecFile {
        path: "config.json",
        sha256: Some("0cfca69c4ae6930ecfa1ccc863f846a3e5a40e120d858161c31ece009dbe9734"),
        size: Some(125),
    },
    SpecFile {
        path: "encoder-model.onnx",
        sha256: Some("e722a24615f5467496eea50a3f1cbb43636b7a0ec01999a1bfbe1656aaf5fd7b"),
        size: Some(343814723),
    },
    SpecFile {
        path: "decoder_joint-model.onnx",
        sha256: Some("f974081fc490ad54bd0f18e6be4f1963b174a0763204481fb2da062d2bdf83cb"),
        size: Some(18201991),
    },
    SpecFile {
        path: "mel_nemo.onnx",
        sha256: Some("f3beb3d21b4143cc7d1471815bd90687110a75a63984b89c86306e9284563881"),
        size: Some(22185),
    },
    SpecFile {
        path: "mel_nemo.onnx.data",
        sha256: Some("9e04389b417c35a888c7a85ae525b733dd6ab8d3d1592c7fa66ef9d26d076a19"),
        size: Some(1188352),
    },
    SpecFile {
        path: "vocab.txt",
        sha256: Some("d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d"),
        size: Some(93939),
    },
    SpecFile {
        path: "NOTICE.txt",
        sha256: Some("4468170d6241441b36b964ec096602b8d6920e70cdbc7c334bd2843c1c1b67d8"),
        size: Some(688),
    },
];

/// Per-file pins from the `manifest.json` inside the macOS release archive
/// (`manifest.json` itself is excluded: the store never hashes it).
#[cfg(target_os = "macos")]
const ULTRA_FILES: &[SpecFile] = &[
    SpecFile {
        path: "AudioEncoder.mlmodelc/analytics/coremldata.bin",
        sha256: Some("ed68cea24c04219b2f441d5975bdfb65a56cae7675ffdf6dab1739f600a747cc"),
        size: Some(243),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/coremldata.bin",
        sha256: Some("27b5f9420e60909b6fd15d4858b38ac892268368b90a8af0407326f0b85e8390"),
        size: Some(351),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/metadata.json",
        sha256: Some("38231e5437b91ae01ae151871f7f0b34f5bf5f8a66b2ad30a6bcc90b74abc283"),
        size: Some(2499),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/model.mil",
        sha256: Some("35e76d58412c18ff20f8fc946f751f8caaec191a7d17d81f052eb2093b604930"),
        size: Some(978977),
    },
    SpecFile {
        path: "AudioEncoder.mlmodelc/weights/weight.bin",
        sha256: Some("7ef6926571fbc9f812034f5b3fac5d05a41871eb35c702f671a117781fb46f25"),
        size: Some(410797632),
    },
    SpecFile {
        path: "Joint.mlmodelc/analytics/coremldata.bin",
        sha256: Some("04360f333e3a4cd45d8a69f98f41a3b013e50e1fefc225ff8e023bc9c036723f"),
        size: Some(243),
    },
    SpecFile {
        path: "Joint.mlmodelc/coremldata.bin",
        sha256: Some("5a2ce6cefb422f1b9fd2af733af462c84ec7257bc6162d632370dd457ba42bb4"),
        size: Some(346),
    },
    SpecFile {
        path: "Joint.mlmodelc/metadata.json",
        sha256: Some("7daea82237542055e477003acac31d94f6a5a8649c48c766e30431c93f1ea906"),
        size: Some(2020),
    },
    SpecFile {
        path: "Joint.mlmodelc/model.mil",
        sha256: Some("83a75420a420b00c34da8be7a8d332ab89b66d705669f918faec4465c9a13096"),
        size: Some(5740),
    },
    SpecFile {
        path: "Joint.mlmodelc/weights/weight.bin",
        sha256: Some("003db529d2952645b0b36670e8b3fcc1e38f72e5b3885e96e5361520dc574158"),
        size: Some(6359308),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/analytics/coremldata.bin",
        sha256: Some("837a25d437aff742e4b07b0dd8bb569bceaa9ed3159cc6d55a967d091aa63d17"),
        size: Some(243),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/coremldata.bin",
        sha256: Some("67f852ced7415e6aded98a18a8edb5409d0f7753f72c9cd3e6edd744f16d95d8"),
        size: Some(401),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/metadata.json",
        sha256: Some("223a3cb0060d72cf86356712811d3e96bdd50dcecde76d0bc72471f507b25732"),
        size: Some(2512),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/model.mil",
        sha256: Some("044cf490cb442ba79d446505a13d180082248bf239d58140e451c8d6f314e682"),
        size: Some(1835964),
    },
    SpecFile {
        path: "MelSpectrogram.mlmodelc/weights/weight.bin",
        sha256: Some("f3c3d600f5fc6311ceff6406c10aeb4285a7f0fda4de6ac86b5bee3e76f80e32"),
        size: Some(976704),
    },
    SpecFile {
        path: "NOTICE.txt",
        sha256: Some("5dad4bacd0bfdd19a4bbe88a82b5c176bed8dd53669811b3c895a2216d91f1c3"),
        size: Some(700),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/analytics/coremldata.bin",
        sha256: Some("a1c4716bcccb2a9f9da122247ccaef2ccc56786c7b364f6c0b5c19dd1a755626"),
        size: Some(243),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/coremldata.bin",
        sha256: Some("cde46f5fe167ca24c131e66fe847b20c211b502d57f1ba6f5bcfce3a87bec3e2"),
        size: Some(418),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/metadata.json",
        sha256: Some("af0378a73304f3e2294132b8871c033106c214a6740a0dde1ae1e03f2dccc72f"),
        size: Some(3006),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/model.mil",
        sha256: Some("b312e8f9550241072894d077b98a1f7bbacd55ee915d539013f44df0be4c00e7"),
        size: Some(15692),
    },
    SpecFile {
        path: "TextDecoder.mlmodelc/weights/weight.bin",
        sha256: Some("a98cb2eb85da7d2a1e1e7d298e1769f27998bfba2c67b2281714b7aa5f70dd08"),
        size: Some(11856064),
    },
    SpecFile {
        path: "pipeline.json",
        sha256: Some("df6edfe91891d0516761ab72ad6b4f27ddf4ee23d5c867b2d6132b30129cacb5"),
        size: Some(908),
    },
    SpecFile {
        path: "vocab.txt",
        sha256: Some("5b133ab19bf7bf03e06e61a193d2dedb96f72677850a670231bb7344c2e09d4c"),
        size: Some(54078),
    },
];

#[cfg(not(target_os = "macos"))]
const ULTRA_SIZE_HINT_BYTES: u64 = 363322003;
#[cfg(target_os = "macos")]
const ULTRA_SIZE_HINT_BYTES: u64 = 432894292;

/// The app's own shared-store spec for Parakeet Ultra. Every shared-store
/// path (activation, migration, download) uses this instead of the catalog's
/// `parakeet_spec()`. Migration sources: the app-private install dir
/// (`<install_base>/<id>`, where Ultra is hand-placed or installed today) and
/// the suite-style `asr/<id>` form.
pub fn spec() -> ModelSpec {
    #[cfg(not(target_os = "macos"))]
    let (legacy_dirs, retired_legacy_paths, retired_store_ids) = (
        &[
            "parakeet_tdt_ultra_static1500_fp16int4_20261022",
            "asr/parakeet_tdt_ultra_static1500_fp16int4_20261022",
        ][..],
        &[
            "asr/unified_static15_b128_sym_bits4_timestamp_hybrid",
            "asr/parakeet_tdt_v3_static_b64",
            "asr/parakeet_tdt_v3_b64_sym_bits4_protect",
            "asr/whisper_turbo_q5_k.bin",
            "asr/ggml-tiny.bin",
            "asr/whisper-tiny.en.bin",
            "asr/parakeet_tdt_v3_static1500_qint8_20260722",
        ][..],
        &[
            "parakeet_tdt_v3_static_b64",
            "parakeet_tdt_v3_b64_sym_bits4_protect",
            "unified_static15_b128_sym_bits4_timestamp_hybrid",
            "parakeet_tdt_v3_static1500_qint8_20260722",
            "parakeet_tdt_v3_dynamic_b64_fp16storage_20260720",
        ][..],
    );
    #[cfg(target_os = "macos")]
    let (legacy_dirs, retired_legacy_paths, retired_store_ids) = (
        &["parakeet-tdt-ultra", "asr/parakeet-tdt-ultra"][..],
        &[
            "asr/unified_static15_b128_sym_bits4_timestamp_hybrid",
            "asr/parakeet_tdt_v3_static_b64",
            "asr/parakeet_tdt_v3_b64_sym_bits4_protect",
            "asr/whisper_turbo_q5_k.bin",
            "asr/ggml-tiny.bin",
            "asr/whisper-tiny.en.bin",
            "asr/parakeet-tdt-v3",
            "asr/parakeet-tdt-v3-coreml-compiled-5f8888117f47",
        ][..],
        &[
            "parakeet_tdt_v3_static_b64",
            "parakeet_tdt_v3_b64_sym_bits4_protect",
            "unified_static15_b128_sym_bits4_timestamp_hybrid",
            "parakeet-tdt-v3-coreml-compiled-5f8888117f47",
        ][..],
    );
    ModelSpec {
        id: ULTRA_ID,
        label: "Parakeet Ultra speech model",
        tier: ModelTier::Base,
        entitlement: false,
        version: ULTRA_SHA256,
        size_hint_bytes: ULTRA_SIZE_HINT_BYTES,
        files: ULTRA_FILES,
        source: SpecSource::Archive {
            url: Some(ULTRA_ARCHIVE_URL),
            sha256: Some(ULTRA_SHA256),
            size: ULTRA_ARCHIVE_BYTES,
            requires_auth: true,
        },
        legacy_dirs,
        retired_legacy_paths,
        retired_store_ids,
    }
}

/// True for exactly 64 ASCII hex digits.
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

/// This platform's archive checksum, or `None` while it is still a placeholder.
pub fn pinned_sha256() -> Option<&'static str> {
    is_sha256_hex(ULTRA_SHA256).then_some(ULTRA_SHA256)
}

/// Fail-closed gate between a catalog entry and a download. Returns the pinned
/// checksum only when the pin is real, the catalog id is the expected Ultra id
/// and the catalog's archive checksum equals the pin.
pub fn check_catalog_against(
    expected_id: &str,
    pin: &'static str,
    catalog_id: &str,
    catalog_archive_sha256: Option<&str>,
) -> Result<&'static str, String> {
    if !is_sha256_hex(pin) {
        return Err(
            "Parakeet Ultra is not available in this build: its archive checksum is not configured. Nothing was downloaded."
                .to_string(),
        );
    }
    if catalog_id != expected_id {
        return Err(format!(
            "Parakeet Ultra is not available in this build: the model catalog offers `{catalog_id}`, not `{expected_id}`. Nothing was downloaded."
        ));
    }
    match catalog_archive_sha256 {
        Some(sha) if sha.eq_ignore_ascii_case(pin) => Ok(pin),
        _ => Err(
            "Parakeet Ultra is not available in this build: the model catalog checksum does not match the pinned archive. Nothing was downloaded."
                .to_string(),
        ),
    }
}

/// `check_catalog_against` for this platform's Ultra id and pin.
pub fn check_catalog(
    catalog_id: &str,
    catalog_archive_sha256: Option<&str>,
) -> Result<&'static str, String> {
    check_catalog_against(ULTRA_ID, ULTRA_SHA256, catalog_id, catalog_archive_sha256)
}
