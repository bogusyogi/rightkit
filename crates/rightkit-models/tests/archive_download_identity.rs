//! Byte-identity fixture for the archive and download layers. Every output is
//! reduced to a digest over (relative path, size, sha256) for each file, so the
//! digests pin the exact bytes produced by `archive::extract_member`,
//! `archive::unpack_wrapped`, `suite::archive::install_tar_gz`, and
//! `download::download_resumable`. A refactor that changes any extracted or
//! downloaded byte, or any file set, changes a digest and fails this test.

use rightkit_models::archive;
use rightkit_models::download::{self, DownloadRequest};
use rightkit_models::suite::archive as suite_archive;
use rightkit_qa::mock::{MockServer, Reply};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// Loopback-capable client for the local mock server. Production clients refuse
/// loopback; this test client allows exactly 127.0.0.1.
fn http() -> rightkit_models::http::ModelHttp {
    rightkit_models::http::http_client_with_options(rightkit_models::http::ModelHttpOptions {
        proxy: rightkit_models::http::ProxyMode::None,
        address_policy: rightkit_http::AddressPolicy::public_web()
            .allow_if(vec!["127.0.0.1".to_string()], |ip| ip.is_loopback()),
        ..Default::default()
    })
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn payload(len: usize, seed: u32) -> Vec<u8> {
    (0..len)
        .map(|i| {
            (i as u32)
                .wrapping_mul(2_654_435_761)
                .wrapping_add(seed)
                .rotate_left(7) as u8
        })
        .collect()
}

/// Digest of every regular file under `root`, sorted by relative path.
fn tree_digest(root: &Path) -> String {
    let mut files: Vec<(String, Vec<u8>)> = Vec::new();
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            if path.is_dir() {
                walk(base, &path, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    walk(root, root, &mut files);
    let mut all = Sha256::new();
    for (rel, bytes) in &files {
        all.update(format!("{rel}\0{}\0{}\n", bytes.len(), sha(bytes)).as_bytes());
    }
    hex(&all.finalize())
}

fn tar_gz(dir: &Path, name: &str, members: &[(&str, Option<&[u8]>)]) -> PathBuf {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in members {
        let mut header = tar::Header::new_gnu();
        match data {
            Some(bytes) => {
                header.set_entry_type(tar::EntryType::Regular);
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                builder.append_data(&mut header, path, *bytes).unwrap();
            }
            None => {
                header.set_entry_type(tar::EntryType::Directory);
                header.set_size(0);
                header.set_mode(0o755);
                builder
                    .append_data(&mut header, path, std::io::empty())
                    .unwrap();
            }
        }
    }
    let tar_bytes = builder.into_inner().unwrap();
    let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    gz.write_all(&tar_bytes).unwrap();
    let path = dir.join(name);
    fs::write(&path, gz.finish().unwrap()).unwrap();
    path
}

fn plain_tar(dir: &Path, name: &str, members: &[(&str, &[u8])]) -> PathBuf {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data) in members {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        builder.append_data(&mut header, path, *data).unwrap();
    }
    let path = dir.join(name);
    fs::write(&path, builder.into_inner().unwrap()).unwrap();
    path
}

fn zip_file(dir: &Path, name: &str, members: &[(&str, &[u8])]) -> PathBuf {
    let path = dir.join(name);
    let mut writer = zip::ZipWriter::new(fs::File::create(&path).unwrap());
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (member, data) in members {
        writer.start_file(*member, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap();
    path
}

/// Wrapped model archive with Apple metadata that must be skipped.
fn wrapped_archive(dir: &Path, name: &str, wrapper: &str, seed: u32) -> PathBuf {
    let config = payload(4_096, seed);
    let weights = payload(300_000, seed + 1);
    let tokenizer = payload(777, seed + 2);
    tar_gz(
        dir,
        name,
        &[
            (&format!("{wrapper}/"), None),
            (&format!("{wrapper}/sub/"), None),
            (&format!("{wrapper}/config.json"), Some(&config)),
            (&format!("{wrapper}/sub/weights.bin"), Some(&weights)),
            (&format!("{wrapper}/sub/tokenizer.model"), Some(&tokenizer)),
            (&format!("__MACOSX/{wrapper}/._config.json"), Some(b"apple")),
            (&format!("{wrapper}/._sub"), Some(b"apple")),
        ],
    )
}

fn digests() -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    let work = tempfile::tempdir().unwrap();
    let base = work.path();

    // 1. Single-member extraction from tar.gz, plain tar, and zip.
    let weights = payload(300_000, 9);
    let gz = tar_gz(base, "single.tar.gz", &[("weights.bin", Some(&weights))]);
    let plain = plain_tar(base, "single.tar", &[("weights.bin", &weights)]);
    let zipped = zip_file(base, "single.zip", &[("weights.bin", &weights)]);
    for (label, archive_path) in [
        ("extract_member.tar_gz", &gz),
        ("extract_member.plain_tar", &plain),
        ("extract_member.zip", &zipped),
    ] {
        let dest_dir = base.join(label);
        fs::create_dir_all(&dest_dir).unwrap();
        let dest = dest_dir.join("out.bin");
        fs::write(&dest, b"old-weights").unwrap();
        let written = archive::extract_member(archive_path, "weights.bin", &dest).unwrap();
        let bytes = fs::read(&dest).unwrap();
        out.push((label, format!("{written}:{}", sha(&bytes))));
    }
    let missing = archive::extract_member(&gz, "absent.bin", &base.join("absent.bin"));
    out.push((
        "extract_member.missing_is_err",
        missing.is_err().to_string(),
    ));

    // 2. Wrapped unpack with the wrapper stripped (root stack).
    let wrapped = wrapped_archive(base, "wrapped.tar.gz", "model-v1", 1);
    let unpack_dir = base.join("unpacked");
    archive::unpack_wrapped(&wrapped, &unpack_dir).unwrap();
    out.push(("unpack_wrapped.tree", tree_digest(&unpack_dir)));

    // 3. Foreign wrapper is rejected.
    let mixed = tar_gz(
        base,
        "mixed.tar.gz",
        &[
            ("model-v1/a.bin", Some(b"a")),
            ("model-v2/b.bin", Some(b"b")),
        ],
    );
    let mixed_dir = base.join("mixed");
    out.push((
        "unpack_wrapped.mixed_wrapper_is_err",
        archive::unpack_wrapped(&mixed, &mixed_dir)
            .is_err()
            .to_string(),
    ));

    // 4. Suite install: staged, then swapped into <root>/<name>, previous kept
    //    until commit. Two installs prove the replace path.
    let suite_root = base.join("suite-models");
    suite_archive::install_tar_gz(&wrapped, &suite_root, "demo").unwrap();
    out.push((
        "suite.install_tar_gz.first",
        tree_digest(&suite_root.join("demo")),
    ));
    let wrapped2 = wrapped_archive(base, "wrapped2.tar.gz", "model-v2", 100);
    suite_archive::install_tar_gz(&wrapped2, &suite_root, "demo").unwrap();
    out.push((
        "suite.install_tar_gz.replace",
        tree_digest(&suite_root.join("demo")),
    ));
    out.push((
        "suite.install_tar_gz.no_aside_left",
        (!suite_root.join(".demo.previous-aside").exists()).to_string(),
    ));

    // 5. Suite install of a plain (non-gzip) tar is rejected, as before.
    let plain_wrapped = plain_tar(base, "plain-wrapped.tar", &[("w/a.bin", b"a")]);
    out.push((
        "suite.install_tar_gz.plain_tar_is_err",
        suite_archive::install_tar_gz(&plain_wrapped, &suite_root, "plain")
            .is_err()
            .to_string(),
    ));

    // 6. Download through the resumable path, verified against the sha pin.
    let body = payload(1_500_000, 5);
    let route = "/org/demo/resolve/main/model.bin";
    let served = body.clone();
    let mock = MockServer::start_with(move |req, _| {
        if !req.is("GET", route) {
            return None;
        }
        Some(Reply::bytes("application/octet-stream", served.clone()))
    })
    .unwrap();
    let dl_dir = base.join("downloads");
    fs::create_dir_all(&dl_dir).unwrap();
    let dest = dl_dir.join("model.bin");
    let part = download::part_path_for(&dest);
    let expected = sha(&body);
    let url = format!("{}{route}", mock.base);
    download::download_resumable(
        &http(),
        DownloadRequest {
            url: &url,
            part_path: &part,
            dest_path: &dest,
            expected_sha256: Some(&expected),
            authorization: None,
            cancelled: &|| false,
        },
        |_, _| {},
    )
    .unwrap();
    out.push((
        "download.resumable.bytes",
        format!(
            "{}:{}",
            fs::metadata(&dest).unwrap().len(),
            sha(&fs::read(&dest).unwrap())
        ),
    ));
    out.push((
        "download.resumable.no_part_left",
        (!part.exists()).to_string(),
    ));

    out
}

/// Golden digests captured from the pre-refactor archive layer. A change here
/// means extracted or downloaded bytes changed.
const GOLDEN: &[(&str, &str)] = &[
    (
        "extract_member.tar_gz",
        "300000:76a8fb108bb9551607d0ecf6acecc43d9125361a1db05c7baacdfbef30ce4b13",
    ),
    (
        "extract_member.plain_tar",
        "300000:76a8fb108bb9551607d0ecf6acecc43d9125361a1db05c7baacdfbef30ce4b13",
    ),
    (
        "extract_member.zip",
        "300000:76a8fb108bb9551607d0ecf6acecc43d9125361a1db05c7baacdfbef30ce4b13",
    ),
    ("extract_member.missing_is_err", "true"),
    (
        "unpack_wrapped.tree",
        "325a48e52f81bd74772f9ccb789594488932ea111988ed8d30e10b691bf47892",
    ),
    ("unpack_wrapped.mixed_wrapper_is_err", "true"),
    (
        "suite.install_tar_gz.first",
        "325a48e52f81bd74772f9ccb789594488932ea111988ed8d30e10b691bf47892",
    ),
    (
        "suite.install_tar_gz.replace",
        "a2d0eefbf1885f947f026803a9c985a5748bcdf7b353d13e9fdb0d7185dc8c5f",
    ),
    ("suite.install_tar_gz.no_aside_left", "true"),
    ("suite.install_tar_gz.plain_tar_is_err", "true"),
    (
        "download.resumable.bytes",
        "1500000:3ed9833e565b850ab9fb9490bc45938ae080fd6ae0d969515cda19ff8676e1bc",
    ),
    ("download.resumable.no_part_left", "true"),
];

#[test]
fn archive_and_download_outputs_are_byte_identical() {
    let actual = digests();
    let rendered: String = actual
        .iter()
        .map(|(label, value)| format!("    (\"{label}\", \"{value}\"),\n"))
        .collect();
    let actual_pairs: Vec<(&str, &str)> = actual.iter().map(|(l, v)| (*l, v.as_str())).collect();
    assert_eq!(actual_pairs, GOLDEN, "actual table:\n{rendered}");
}
