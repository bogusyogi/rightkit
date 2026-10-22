//! Parakeet Ultra (moondream/parakeet-ultra on the Parakeet TDT v3 graphs; Orthic-Labs/rightkit#897).
//! Both the Windows and macOS archives are pinned (sealed 2026-10-22); `parakeet_ultra_spec()`
//! fails closed only where no pins exist (Linux). The spec builder is also exercised with synthetic,
//! clearly fake pins on whichever platform runs the test.

use rightkit_models::suite::catalog::{
    all_models, base_catalog, parakeet_spec, parakeet_ultra_spec, parakeet_ultra_spec_with,
    MissingParakeetUltraPins, ParakeetUltraPins, SpecFile, SpecSource, MODEL_DOWNLOAD_BASE,
    PARAKEET_ULTRA_ID, PARAKEET_ULTRA_MACOS_ID, PARAKEET_ULTRA_MACOS_PINS,
    PARAKEET_ULTRA_MACOS_ROUTE, PARAKEET_ULTRA_MACOS_URL, PARAKEET_ULTRA_NOTICE,
    PARAKEET_ULTRA_PINS, PARAKEET_ULTRA_REQUIRED_PATHS, PARAKEET_ULTRA_RETIRED_STORE_IDS,
    PARAKEET_ULTRA_WINDOWS_ID, PARAKEET_ULTRA_WINDOWS_PINS, PARAKEET_ULTRA_WINDOWS_ROUTE,
    PARAKEET_ULTRA_WINDOWS_URL,
};
use std::collections::BTreeSet;

mod common;
use common::isolated;

const FAKE_ARCHIVE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const FAKE_FILE: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn leak_files(files: Vec<SpecFile>) -> &'static [SpecFile] {
    Box::leak(files.into_boxed_slice())
}

/// One fake pin per required path of this platform, 10 bytes each.
fn sample_files() -> Vec<SpecFile> {
    PARAKEET_ULTRA_REQUIRED_PATHS
        .iter()
        .map(|path| SpecFile {
            path,
            sha256: Some(FAKE_FILE),
            size: Some(10),
        })
        .collect()
}

fn sample_pins_with(files: Vec<SpecFile>) -> ParakeetUltraPins {
    let installed: u64 = files.iter().filter_map(|f| f.size).sum();
    ParakeetUltraPins {
        archive_sha256: FAKE_ARCHIVE,
        archive_size: 1_000,
        files: leak_files(files),
        installed_size_bytes: installed,
    }
}

fn sample_pins() -> ParakeetUltraPins {
    sample_pins_with(sample_files())
}

#[test]
fn ids_routes_and_urls_follow_the_live_workers_fixed_routes() {
    assert_eq!(
        PARAKEET_ULTRA_WINDOWS_ID,
        "parakeet_tdt_ultra_static1500_fp16int4_20261022"
    );
    assert_eq!(PARAKEET_ULTRA_MACOS_ID, "parakeet-tdt-ultra");
    assert_eq!(
        PARAKEET_ULTRA_WINDOWS_ROUTE,
        "/parakeet/windows/ultra.tar.gz"
    );
    assert_eq!(PARAKEET_ULTRA_MACOS_ROUTE, "/parakeet/macos/ultra.tar.gz");
    assert_eq!(
        PARAKEET_ULTRA_WINDOWS_URL,
        format!("{MODEL_DOWNLOAD_BASE}{PARAKEET_ULTRA_WINDOWS_ROUTE}")
    );
    assert_eq!(
        PARAKEET_ULTRA_MACOS_URL,
        format!("{MODEL_DOWNLOAD_BASE}{PARAKEET_ULTRA_MACOS_ROUTE}")
    );
    // Not `<id>.tar.gz`: the worker's routes are fixed.
    assert!(!PARAKEET_ULTRA_WINDOWS_URL.contains(PARAKEET_ULTRA_WINDOWS_ID));
    assert!(!PARAKEET_ULTRA_MACOS_URL.contains(PARAKEET_ULTRA_MACOS_ID));
    assert_eq!(
        PARAKEET_ULTRA_ID,
        if cfg!(target_os = "macos") {
            PARAKEET_ULTRA_MACOS_ID
        } else {
            PARAKEET_ULTRA_WINDOWS_ID
        }
    );
    assert_eq!(
        PARAKEET_ULTRA_NOTICE,
        "moondream/parakeet-ultra, CC-BY-4.0, based on NVIDIA parakeet-tdt-0.6b-v3"
    );
}

#[test]
fn the_windows_pins_are_the_sealed_archive_values_and_cover_the_layout() {
    let pins = PARAKEET_ULTRA_WINDOWS_PINS.expect("the Windows archive is sealed");
    assert_eq!(
        pins.archive_sha256,
        "0ad7f5a2a6911ccb5315c53602f2ab14a37aa9ffd8446b2e69d20a73745d59b9"
    );
    assert_eq!(pins.archive_size, 316_610_098);
    let expected: [(&str, u64, &str); 7] = [
        (
            "config.json",
            125,
            "0cfca69c4ae6930ecfa1ccc863f846a3e5a40e120d858161c31ece009dbe9734",
        ),
        (
            "encoder-model.onnx",
            343_814_723,
            "e722a24615f5467496eea50a3f1cbb43636b7a0ec01999a1bfbe1656aaf5fd7b",
        ),
        (
            "decoder_joint-model.onnx",
            18_201_991,
            "f974081fc490ad54bd0f18e6be4f1963b174a0763204481fb2da062d2bdf83cb",
        ),
        (
            "mel_nemo.onnx",
            22_185,
            "f3beb3d21b4143cc7d1471815bd90687110a75a63984b89c86306e9284563881",
        ),
        (
            "mel_nemo.onnx.data",
            1_188_352,
            "9e04389b417c35a888c7a85ae525b733dd6ab8d3d1592c7fa66ef9d26d076a19",
        ),
        (
            "vocab.txt",
            93_939,
            "d58544679ea4bc6ac563d1f545eb7d474bd6cfa467f0a6e2c1dc1c7d37e3c35d",
        ),
        (
            "NOTICE.txt",
            688,
            "4468170d6241441b36b964ec096602b8d6920e70cdbc7c334bd2843c1c1b67d8",
        ),
    ];
    assert_eq!(pins.files.len(), expected.len());
    for (file, (path, size, sha)) in pins.files.iter().zip(expected.iter()) {
        assert_eq!(file.path, *path);
        assert_eq!(file.size, Some(*size), "{path}");
        assert_eq!(file.sha256, Some(*sha), "{path}");
    }
    assert_eq!(pins.installed_size_bytes, 363_322_003);
    assert_eq!(
        pins.installed_size_bytes,
        pins.files.iter().filter_map(|f| f.size).sum::<u64>()
    );
    // The archive's own manifest.json is an allowed extra and is not pinned.
    assert!(!pins.files.iter().any(|f| f.path == "manifest.json"));
}

#[test]
fn the_required_paths_are_the_documented_layout_for_this_platform() {
    let required: BTreeSet<&str> = PARAKEET_ULTRA_REQUIRED_PATHS.iter().copied().collect();
    assert_eq!(
        required.len(),
        PARAKEET_ULTRA_REQUIRED_PATHS.len(),
        "no duplicates"
    );
    assert!(required.contains("vocab.txt") && required.contains("NOTICE.txt"));
    if cfg!(target_os = "macos") {
        assert!(required.contains("pipeline.json"));
        // Four compiled stage bundles, each with its IR, metadata, coremldata, weights and
        // analytics; the archive's manifest.json is never listed.
        assert_eq!(required.len(), 3 + 4 * 5);
        assert!(!required.contains("manifest.json"));
        for stage in ["MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint"] {
            for file in [
                "model.mil",
                "metadata.json",
                "coremldata.bin",
                "weights/weight.bin",
                "analytics/coremldata.bin",
            ] {
                let path = format!("{stage}.mlmodelc/{file}");
                assert!(required.contains(path.as_str()), "{path}");
            }
        }
    } else {
        assert_eq!(
            required,
            [
                "config.json",
                "encoder-model.onnx",
                "decoder_joint-model.onnx",
                "mel_nemo.onnx",
                "mel_nemo.onnx.data",
                "vocab.txt",
                "NOTICE.txt",
            ]
            .into_iter()
            .collect::<BTreeSet<_>>()
        );
    }
}

#[test]
fn the_shipped_spec_fails_closed_exactly_where_no_pins_are_recorded() {
    assert_eq!(
        parakeet_ultra_spec().is_err(),
        PARAKEET_ULTRA_PINS.is_none(),
        "a spec exists exactly when pins do"
    );
    if let Err(error) = parakeet_ultra_spec() {
        assert_eq!(error, MissingParakeetUltraPins);
        assert!(error.to_string().contains("#897"));
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    assert!(parakeet_ultra_spec().is_err());
}

#[test]
fn both_platform_pin_tables_are_recorded_and_exclude_the_archive_manifest() {
    // Present on every platform (the constants are not cfg-gated); only the selected one is
    // used by `parakeet_ultra_spec()`.
    let macos = PARAKEET_ULTRA_MACOS_PINS.expect("the macOS archive is sealed");
    let windows = PARAKEET_ULTRA_WINDOWS_PINS.expect("the Windows archive is sealed");
    assert_eq!(macos.files.len(), 23);
    assert_eq!(macos.installed_size_bytes, 432_894_292);
    assert_eq!(windows.files.len(), 7);
    for pins in [macos, windows] {
        assert!(!pins.files.iter().any(|f| f.path == "manifest.json"));
    }
}

#[cfg(target_os = "macos")]
#[test]
fn the_macos_pins_are_the_sealed_archive_values_and_cover_the_layout() {
    let pins = PARAKEET_ULTRA_MACOS_PINS.expect("the macOS archive is sealed");
    assert_eq!(
        pins.archive_sha256,
        "23610964221771e5a608c76d87d57802aecdacb549e27127c4d03b8feac2d4ee"
    );
    assert_eq!(pins.archive_size, 391_192_167);
    // The issue table (24 members) minus the archive's own manifest.json: 23 rows.
    let expected: [(&str, u64, &str); 23] = [
        (
            "AudioEncoder.mlmodelc/analytics/coremldata.bin",
            243,
            "ed68cea24c04219b2f441d5975bdfb65a56cae7675ffdf6dab1739f600a747cc",
        ),
        (
            "AudioEncoder.mlmodelc/coremldata.bin",
            351,
            "27b5f9420e60909b6fd15d4858b38ac892268368b90a8af0407326f0b85e8390",
        ),
        (
            "AudioEncoder.mlmodelc/metadata.json",
            2499,
            "38231e5437b91ae01ae151871f7f0b34f5bf5f8a66b2ad30a6bcc90b74abc283",
        ),
        (
            "AudioEncoder.mlmodelc/model.mil",
            978977,
            "35e76d58412c18ff20f8fc946f751f8caaec191a7d17d81f052eb2093b604930",
        ),
        (
            "AudioEncoder.mlmodelc/weights/weight.bin",
            410797632,
            "7ef6926571fbc9f812034f5b3fac5d05a41871eb35c702f671a117781fb46f25",
        ),
        (
            "Joint.mlmodelc/analytics/coremldata.bin",
            243,
            "04360f333e3a4cd45d8a69f98f41a3b013e50e1fefc225ff8e023bc9c036723f",
        ),
        (
            "Joint.mlmodelc/coremldata.bin",
            346,
            "5a2ce6cefb422f1b9fd2af733af462c84ec7257bc6162d632370dd457ba42bb4",
        ),
        (
            "Joint.mlmodelc/metadata.json",
            2020,
            "7daea82237542055e477003acac31d94f6a5a8649c48c766e30431c93f1ea906",
        ),
        (
            "Joint.mlmodelc/model.mil",
            5740,
            "83a75420a420b00c34da8be7a8d332ab89b66d705669f918faec4465c9a13096",
        ),
        (
            "Joint.mlmodelc/weights/weight.bin",
            6359308,
            "003db529d2952645b0b36670e8b3fcc1e38f72e5b3885e96e5361520dc574158",
        ),
        (
            "MelSpectrogram.mlmodelc/analytics/coremldata.bin",
            243,
            "837a25d437aff742e4b07b0dd8bb569bceaa9ed3159cc6d55a967d091aa63d17",
        ),
        (
            "MelSpectrogram.mlmodelc/coremldata.bin",
            401,
            "67f852ced7415e6aded98a18a8edb5409d0f7753f72c9cd3e6edd744f16d95d8",
        ),
        (
            "MelSpectrogram.mlmodelc/metadata.json",
            2512,
            "223a3cb0060d72cf86356712811d3e96bdd50dcecde76d0bc72471f507b25732",
        ),
        (
            "MelSpectrogram.mlmodelc/model.mil",
            1835964,
            "044cf490cb442ba79d446505a13d180082248bf239d58140e451c8d6f314e682",
        ),
        (
            "MelSpectrogram.mlmodelc/weights/weight.bin",
            976704,
            "f3c3d600f5fc6311ceff6406c10aeb4285a7f0fda4de6ac86b5bee3e76f80e32",
        ),
        (
            "NOTICE.txt",
            700,
            "5dad4bacd0bfdd19a4bbe88a82b5c176bed8dd53669811b3c895a2216d91f1c3",
        ),
        (
            "TextDecoder.mlmodelc/analytics/coremldata.bin",
            243,
            "a1c4716bcccb2a9f9da122247ccaef2ccc56786c7b364f6c0b5c19dd1a755626",
        ),
        (
            "TextDecoder.mlmodelc/coremldata.bin",
            418,
            "cde46f5fe167ca24c131e66fe847b20c211b502d57f1ba6f5bcfce3a87bec3e2",
        ),
        (
            "TextDecoder.mlmodelc/metadata.json",
            3006,
            "af0378a73304f3e2294132b8871c033106c214a6740a0dde1ae1e03f2dccc72f",
        ),
        (
            "TextDecoder.mlmodelc/model.mil",
            15692,
            "b312e8f9550241072894d077b98a1f7bbacd55ee915d539013f44df0be4c00e7",
        ),
        (
            "TextDecoder.mlmodelc/weights/weight.bin",
            11856064,
            "a98cb2eb85da7d2a1e1e7d298e1769f27998bfba2c67b2281714b7aa5f70dd08",
        ),
        (
            "pipeline.json",
            908,
            "df6edfe91891d0516761ab72ad6b4f27ddf4ee23d5c867b2d6132b30129cacb5",
        ),
        (
            "vocab.txt",
            54078,
            "5b133ab19bf7bf03e06e61a193d2dedb96f72677850a670231bb7344c2e09d4c",
        ),
    ];
    assert_eq!(pins.files.len(), expected.len());
    for (path, size, sha) in expected {
        let file = pins
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} is pinned"));
        assert_eq!(file.size, Some(size), "{path}");
        assert_eq!(file.sha256, Some(sha), "{path}");
    }
    // Spot checks against the issue table, written out independently of the table above.
    let weight = |stage: &str| {
        let path = format!("{stage}.mlmodelc/weights/weight.bin");
        *pins.files.iter().find(|f| f.path == path).unwrap()
    };
    assert_eq!(weight("AudioEncoder").size, Some(410_797_632));
    assert_eq!(
        weight("AudioEncoder").sha256,
        Some("7ef6926571fbc9f812034f5b3fac5d05a41871eb35c702f671a117781fb46f25")
    );
    assert_eq!(weight("Joint").size, Some(6_359_308));
    assert_eq!(
        weight("Joint").sha256,
        Some("003db529d2952645b0b36670e8b3fcc1e38f72e5b3885e96e5361520dc574158")
    );
    assert_eq!(weight("MelSpectrogram").size, Some(976_704));
    assert_eq!(
        weight("MelSpectrogram").sha256,
        Some("f3c3d600f5fc6311ceff6406c10aeb4285a7f0fda4de6ac86b5bee3e76f80e32")
    );
    assert_eq!(weight("TextDecoder").size, Some(11_856_064));
    assert_eq!(
        weight("TextDecoder").sha256,
        Some("a98cb2eb85da7d2a1e1e7d298e1769f27998bfba2c67b2281714b7aa5f70dd08")
    );
    let notice = pins.files.iter().find(|f| f.path == "NOTICE.txt").unwrap();
    assert_eq!(notice.size, Some(700));
    assert_eq!(
        notice.sha256,
        Some("5dad4bacd0bfdd19a4bbe88a82b5c176bed8dd53669811b3c895a2216d91f1c3")
    );
    // The file set is exactly the required paths, and the archive's manifest.json is not pinned.
    let listed: BTreeSet<&str> = pins.files.iter().map(|f| f.path).collect();
    let required: BTreeSet<&str> = PARAKEET_ULTRA_REQUIRED_PATHS.iter().copied().collect();
    assert_eq!(listed, required);
    assert_eq!(listed.len(), 23);
    assert!(!listed.contains("manifest.json"));
    assert_eq!(pins.installed_size_bytes, 432_894_292);
    assert_eq!(
        pins.installed_size_bytes,
        pins.files.iter().filter_map(|f| f.size).sum::<u64>()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn macos_has_a_live_spec_from_the_sealed_pins() {
    let spec = parakeet_ultra_spec().unwrap();
    assert_eq!(spec.id, PARAKEET_ULTRA_MACOS_ID);
    assert_eq!(
        spec.version,
        "23610964221771e5a608c76d87d57802aecdacb549e27127c4d03b8feac2d4ee"
    );
    assert!(spec.can_download() && spec.requires_grant());
    assert_eq!(spec.files.len(), 23);
    assert_eq!(spec.size_hint_bytes, 432_894_292);
    let SpecSource::Archive {
        url,
        sha256,
        size,
        requires_auth,
    } = &spec.source
    else {
        panic!("Ultra must be one archive");
    };
    assert_eq!(*url, Some(PARAKEET_ULTRA_MACOS_URL));
    assert_eq!(
        *sha256,
        Some("23610964221771e5a608c76d87d57802aecdacb549e27127c4d03b8feac2d4ee")
    );
    assert_eq!(*size, Some(391_192_167));
    assert!(*requires_auth);
}

#[cfg(windows)]
#[test]
fn windows_has_a_live_spec_from_the_sealed_pins() {
    let spec = parakeet_ultra_spec().unwrap();
    assert_eq!(spec.id, PARAKEET_ULTRA_WINDOWS_ID);
    assert_eq!(
        spec.version,
        "0ad7f5a2a6911ccb5315c53602f2ab14a37aa9ffd8446b2e69d20a73745d59b9"
    );
    assert!(spec.can_download() && spec.requires_grant());
    assert_eq!(spec.files.len(), 7);
    assert_eq!(spec.size_hint_bytes, 363_322_003);
    let SpecSource::Archive {
        url,
        sha256,
        size,
        requires_auth,
    } = &spec.source
    else {
        panic!("Ultra must be one archive");
    };
    assert_eq!(*url, Some(PARAKEET_ULTRA_WINDOWS_URL));
    assert_eq!(
        *sha256,
        Some("0ad7f5a2a6911ccb5315c53602f2ab14a37aa9ffd8446b2e69d20a73745d59b9")
    );
    assert_eq!(*size, Some(316_610_098));
    assert!(*requires_auth);
}

#[test]
fn explicit_well_formed_pins_make_the_spec_on_this_platform() {
    let spec = parakeet_ultra_spec_with(sample_pins()).unwrap();
    assert_eq!(spec.id, PARAKEET_ULTRA_ID);
    assert_eq!(spec.version, FAKE_ARCHIVE);
    assert_eq!(
        spec.size_hint_bytes,
        10 * PARAKEET_ULTRA_REQUIRED_PATHS.len() as u64
    );
    assert!(
        spec.legacy_dirs.is_empty(),
        "Ultra has no legacy directories"
    );
    assert!(spec.retired_legacy_paths.is_empty());
    assert_eq!(spec.retired_store_ids, PARAKEET_ULTRA_RETIRED_STORE_IDS);
    assert_eq!(
        spec.retired_store_ids,
        [
            "parakeet_tdt_v3_static1500_qint8_20260722",
            "parakeet-tdt-v3-coreml-compiled-5f8888117f47"
        ]
    );
    let listed: BTreeSet<&str> = spec.files.iter().map(|f| f.path).collect();
    let required: BTreeSet<&str> = PARAKEET_ULTRA_REQUIRED_PATHS.iter().copied().collect();
    assert_eq!(listed, required);
    assert_eq!(
        spec.expected_manifest().files.len(),
        PARAKEET_ULTRA_REQUIRED_PATHS.len()
    );
    assert!(spec
        .files
        .iter()
        .all(|f| f.sha256.is_some() && f.size.is_some()));
    let SpecSource::Archive {
        url,
        sha256,
        size,
        requires_auth,
    } = &spec.source
    else {
        panic!("Ultra must be one archive");
    };
    let expected_url = if cfg!(target_os = "macos") {
        Some(PARAKEET_ULTRA_MACOS_URL)
    } else if cfg!(windows) {
        Some(PARAKEET_ULTRA_WINDOWS_URL)
    } else {
        None
    };
    assert_eq!(*url, expected_url);
    assert_eq!(*sha256, Some(FAKE_ARCHIVE));
    // The archive size is pinned so the store enforces it (cap plus exact check).
    assert_eq!(*size, Some(1_000));
    assert!(*requires_auth);
    assert_eq!(spec.can_download(), expected_url.is_some());
}

#[test]
fn malformed_pins_are_refused() {
    let refuse = |pins: ParakeetUltraPins| {
        assert_eq!(
            parakeet_ultra_spec_with(pins).unwrap_err(),
            MissingParakeetUltraPins
        );
    };
    let mut bad_sha = sample_pins();
    bad_sha.archive_sha256 = "abc";
    refuse(bad_sha);
    let mut upper = sample_pins();
    upper.archive_sha256 = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA";
    refuse(upper);
    let mut zero_archive = sample_pins();
    zero_archive.archive_size = 0;
    refuse(zero_archive);

    // A file with a missing pin, a zero size, or a bad digest.
    let mut files = sample_files();
    files[0].sha256 = None;
    refuse(sample_pins_with(files));
    let mut files = sample_files();
    files[0].size = None;
    refuse(sample_pins_with(files));
    let mut files = sample_files();
    files[0].size = Some(0);
    refuse(sample_pins_with(files));
    let mut files = sample_files();
    files[0].sha256 = Some("xyz");
    refuse(sample_pins_with(files));

    // A file set that is not exactly the layout: missing, extra, duplicated, empty.
    let mut files = sample_files();
    files.pop();
    refuse(sample_pins_with(files));
    let mut files = sample_files();
    files.push(SpecFile {
        path: "manifest.json",
        sha256: Some(FAKE_FILE),
        size: Some(10),
    });
    refuse(sample_pins_with(files));
    let mut files = sample_files();
    let first = files[0];
    *files.last_mut().unwrap() = first;
    refuse(sample_pins_with(files));
    refuse(sample_pins_with(Vec::new()));

    // The installed size must be the sum of the file sizes.
    let mut wrong_total = sample_pins();
    wrong_total.installed_size_bytes += 1;
    refuse(wrong_total);
}

#[test]
fn ultra_is_additive_parakeet_spec_and_the_catalogs_are_unchanged() {
    let tdt = parakeet_spec();
    assert_ne!(tdt.id, PARAKEET_ULTRA_ID);
    // The v3 entry of this platform is one of the ids Ultra retires.
    assert!(PARAKEET_ULTRA_RETIRED_STORE_IDS.contains(&tdt.id));
    assert!(!base_catalog().iter().any(|s| s.id == PARAKEET_ULTRA_ID));
    assert!(!all_models().iter().any(|s| s.id == PARAKEET_ULTRA_ID));
}

#[test]
fn the_store_exposes_the_ultra_spec_and_refuses_to_resolve_unpinned_or_missing() {
    isolated(
        "the_store_exposes_the_ultra_spec_and_refuses_to_resolve_unpinned_or_missing",
        || {
            let root = tempfile::tempdir().unwrap();
            let store =
                rightkit_models::suite::ModelStore::at_models_root(root.path(), "heardright");
            assert_eq!(
                store.parakeet_ultra_spec().is_ok(),
                PARAKEET_ULTRA_PINS.is_some()
            );
            // Nothing is installed here: with or without pins, resolving is an error.
            assert!(store.resolve_parakeet_ultra().is_err());
            // The v3 accessors are untouched.
            assert_eq!(store.parakeet_spec().id, parakeet_spec().id);
        },
    );
}
