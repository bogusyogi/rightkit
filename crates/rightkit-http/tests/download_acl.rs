//! Access lists under the owner-only download files (second comparative review, blocker 2).
//!
//! A mode of `0700`/`0600` does not keep bytes private where the parent directory's access list
//! is inherited: macOS evaluates an ACL `allow` entry before the mode bits and hands the
//! parent's inheritable entries to what is created in it, and Windows hands a new file the
//! parent's access. The download's directory and partial file are created through rightkit-fs,
//! which refuses on macOS a parent whose ACL would give them an `allow` entry, and creates with an
//! owner-only protected DACL on Windows. The macOS tests make ACLs with `/bin/chmod +a` as
//! `crates/rightkit-fs/tests/acl_macos.rs` does and need a volume that holds ACLs (APFS does).
//! The Windows tests read access lists with `icacls`. Neither has been run where it was written.
//!
//! The rule under test: a download is always private and nothing is copied from the file it
//! replaces or from a leftover partial; the published file is the staged file as it is.
#![allow(dead_code)]

#[cfg(any(target_os = "macos", windows))]
mod common;

#[cfg(target_os = "macos")]
mod macos {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    use super::common::{coord_dir, part_of, plant_part};
    use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
    use rightkit_http::*;
    use sha2::{Digest, Sha256};

    const INHERITING_ALLOW: &str = "everyone allow read,file_inherit,directory_inherit";

    fn cfg() -> ClientConfig {
        ClientConfig {
            retry: RetryPolicy::none(),
            request_timeout: Duration::from_secs(5),
            response_timeout: Some(Duration::from_secs(5)),
            ..loopback_config()
        }
    }

    /// A fresh directory under the target directory, canonical, with no ACL of its own.
    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("acl-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn add_acl(path: &Path, entry: &str) {
        let status = Command::new("/bin/chmod")
            .arg("+a")
            .arg(entry)
            .arg(path)
            .status()
            .unwrap();
        assert!(status.success(), "chmod +a {entry:?} {}", path.display());
    }

    /// The ACL entry lines `ls -lde` prints under the file line (` 0: group:everyone allow read`).
    fn acl_entries(path: &Path) -> Vec<String> {
        let output = Command::new("/bin/ls")
            .arg("-lde")
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .skip(1)
            .filter(|line| line.trim_start().starts_with(|c: char| c.is_ascii_digit()))
            .map(str::to_string)
            .collect()
    }

    fn has_allow_entry(path: &Path) -> bool {
        acl_entries(path)
            .iter()
            .any(|line| line.contains(" allow "))
    }

    /// A directory carrying `entry`, with the precondition that the volume holds ACLs.
    fn parent_with(name: &str, entry: &str) -> PathBuf {
        let parent = scratch(name);
        add_acl(&parent, entry);
        assert!(
            !acl_entries(&parent).is_empty(),
            "the volume holds ACLs (precondition)"
        );
        parent
    }

    fn names(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn sha_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A directory whose ACL would hand an `allow` entry to the download directory is refused
    /// with the typed error, before any request, and nothing is created in it.
    #[test]
    fn a_parent_with_an_inheritable_allow_entry_is_refused_and_nothing_is_created() {
        let server = MockServer::start(|_, _| MockResponse::text(200, "secret"));
        for (index, entry) in [
            "everyone allow read,file_inherit",
            "everyone allow read,directory_inherit",
            INHERITING_ALLOW,
        ]
        .into_iter()
        .enumerate()
        {
            let parent = parent_with(&format!("refused-{index}"), entry);
            for resume in [false, true] {
                let err = Client::new(cfg())
                    .download_to_path(
                        &server.url("/f"),
                        &parent.join("out.bin"),
                        DownloadOptions {
                            resume,
                            ..DownloadOptions::default()
                        },
                    )
                    .expect_err(entry);
                assert!(
                    matches!(&err, DownloadError::UnsafeAcl { path }
                        if path.starts_with(&parent)),
                    "{entry}: {err:?}"
                );
                assert!(
                    err.to_string().contains("access list"),
                    "the message must say why: {err}"
                );
            }
            assert!(names(&parent).is_empty(), "{entry}: {:?}", names(&parent));
        }
        assert_eq!(server.request_count(), 0, "no request may be sent");
    }

    /// A deny-only list cannot widen access, so it passes: the download succeeds and neither the
    /// download directory nor the published file carries an `allow` entry.
    #[test]
    fn a_parent_with_only_a_deny_entry_is_accepted() {
        let parent = parent_with(
            "deny-only",
            "everyone deny writeextattr,file_inherit,directory_inherit",
        );
        let server = MockServer::start(|_, _| MockResponse::text(200, "payload"));
        let dest = parent.join("out.bin");
        Client::new(cfg())
            .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
            .expect("a deny-only ACL must not stop the download");
        assert_eq!(fs::read(&dest).unwrap(), b"payload");
        assert!(!has_allow_entry(&dest), "{:?}", acl_entries(&dest));
        assert!(!has_allow_entry(&coord_dir(&dest)));
    }

    /// Publication copies nothing from the file it replaces. An existing destination with a deny
    /// entry is replaced by a file with no ACL and mode `0600`. The deny entry is not preserved,
    /// and that is safe: the new file is `0600` and carries no `allow` entry, so nobody but its
    /// owner can read it, with or without the deny entry. (The deny entry used is the one
    /// `crates/rightkit-fs/tests/acl_macos.rs` uses because it is harmless to the owner.)
    #[test]
    fn replacing_a_destination_with_a_deny_entry_publishes_a_private_file_without_an_acl() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("replace-deny");
        let dest = dir.join("out.bin");
        fs::write(&dest, b"old").unwrap();
        fs::set_permissions(&dest, fs::Permissions::from_mode(0o644)).unwrap();
        add_acl(&dest, "everyone deny writeextattr");
        assert!(!acl_entries(&dest).is_empty(), "precondition");
        let server = MockServer::start(|_, _| MockResponse::text(200, "payload"));
        Client::new(cfg())
            .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
            .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"payload");
        assert_eq!(
            fs::metadata(&dest).unwrap().permissions().mode() & 0o7777,
            0o600
        );
        assert!(acl_entries(&dest).is_empty(), "{:?}", acl_entries(&dest));
    }

    /// An existing download directory that carries an `allow` entry is refused, not repaired.
    #[test]
    fn an_existing_download_directory_with_an_allow_entry_is_refused() {
        let server = MockServer::start(|_, _| MockResponse::text(200, "secret"));
        let dir = scratch("coord-allow");
        let dest = dir.join("out.bin");
        fs::create_dir(coord_dir(&dest)).unwrap();
        add_acl(&coord_dir(&dest), "everyone allow read");
        let err = Client::new(cfg())
            .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
            .expect_err("a download directory with an allow entry must be refused");
        assert!(matches!(&err, DownloadError::UnsafeAcl { .. }), "{err:?}");
        assert_eq!(server.request_count(), 0);
        assert!(!dest.exists());
        assert!(has_allow_entry(&coord_dir(&dest)), "the ACL is left alone");
    }

    /// A leftover partial that is `0600` but carries an `allow` entry is made private by this
    /// call or refused: on macOS `restrict_owner_only` does not remove an ACL entry (that could
    /// not revoke a descriptor somebody holds), it refuses. The file is not appended to, not
    /// deleted, and no request is sent.
    #[test]
    fn a_resumed_partial_with_an_allow_entry_is_refused_and_left_alone() {
        use std::os::unix::fs::PermissionsExt;
        let data = b"0123456789".to_vec();
        let server = MockServer::start(|_, _| MockResponse::text(200, "must not be asked"));
        let dir = scratch("partial-allow");
        let dest = dir.join("out.bin");
        plant_part(&dest, &data[..4]);
        let part = part_of(&dest);
        fs::set_permissions(&part, fs::Permissions::from_mode(0o600)).unwrap();
        add_acl(&part, "everyone allow read");
        assert!(has_allow_entry(&part), "precondition");

        let err = Client::new(cfg())
            .download_to_path(
                &server.url("/f"),
                &dest,
                DownloadOptions {
                    resume: true,
                    expected_sha256: Some(sha_hex(&data)),
                    ..DownloadOptions::default()
                },
            )
            .expect_err("a partial readable through an ACL must not be used");
        assert!(
            matches!(&err, DownloadError::UnsafeAcl { path } if path == &part),
            "{err:?}"
        );
        assert_eq!(server.request_count(), 0, "no request may be sent");
        assert_eq!(fs::read(&part).unwrap(), &data[..4], "nothing appended");
        assert!(
            has_allow_entry(&part),
            "the partial is neither repaired nor deleted"
        );
        assert!(!dest.exists());
    }
}

#[cfg(windows)]
mod windows {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::Duration;

    use std::sync::{Arc, Mutex};

    use super::common::{part_of, plant_part};
    use rightkit_http::testing::{loopback_config, MockResponse, MockServer};
    use rightkit_http::*;
    use sha2::{Digest, Sha256};

    fn cfg() -> ClientConfig {
        ClientConfig {
            retry: RetryPolicy::none(),
            request_timeout: Duration::from_secs(5),
            response_timeout: Some(Duration::from_secs(5)),
            ..loopback_config()
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("acl-{name}"));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sha_hex(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// `icacls <path>`: one line per ACE, an inherited ACE marked `(I)`.
    fn icacls(path: &Path) -> String {
        let output = Command::new("icacls").arg(path).output().unwrap();
        assert!(output.status.success(), "icacls {}", path.display());
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// The parent grants Everyone read to what is created in it (inheritable). A plain file made
    /// there inherits the grant, which proves the fixture; the downloaded file, staged with an
    /// owner-only protected DACL, has no inherited entry and does not list Everyone.
    ///
    /// This reads `icacls` text (`(I)` and `Everyone`), not the DACL itself:
    /// `crates/rightkit-fs/tests/atomic_dacl_windows.rs` reads the DACL through
    /// `GetNamedSecurityInfoW`, which needs about 45 lines of declarations that are not copied
    /// here.
    #[test]
    fn a_download_under_an_inherited_grant_is_not_readable_through_it() {
        let parent = scratch("inherit");
        let status = Command::new("icacls")
            .arg(&parent)
            .args(["/grant", "*S-1-1-0:(OI)(CI)(R)"])
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "icacls could not grant Everyone on the parent"
        );
        let plain = parent.join("plain.bin");
        fs::write(&plain, b"x").unwrap();
        let plain_acl = icacls(&plain);
        assert!(
            plain_acl.contains("(I)") && plain_acl.contains("Everyone"),
            "precondition: an ordinary file inherits the grant: {plain_acl}"
        );

        let server = MockServer::start(|_, _| MockResponse::text(200, "secret"));
        let dest = parent.join("out.bin");
        Client::new(cfg())
            .download_to_path(&server.url("/f"), &dest, DownloadOptions::default())
            .unwrap();
        assert_eq!(fs::read(&dest).unwrap(), b"secret");
        let published = icacls(&dest);
        assert!(
            !published.contains("(I)") && !published.contains("Everyone"),
            "the downloaded file must not carry the parent's grant: {published}"
        );
    }
    /// A leftover partial that carries an explicit (not inherited) Everyone read entry is made
    /// private by the resumed call before the request and before any byte is appended:
    /// `restrict_owner_only` replaces the whole DACL. `is_owner_only` cannot test on Windows, so
    /// this replacement is the whole defence. The published file then carries no Everyone entry
    /// either.
    #[test]
    fn a_resumed_partial_with_an_explicit_everyone_entry_is_made_private() {
        let data = b"0123456789".to_vec();
        let dir = scratch("resume-everyone");
        let dest = dir.join("out.bin");
        plant_part(&dest, &data[..4]);
        let part = part_of(&dest);
        // Explicit entry: stop inheriting first (keeping copies), then grant Everyone read.
        for args in [vec!["/inheritance:d"], vec!["/grant", "*S-1-1-0:(R)"]] {
            let status = Command::new("icacls")
                .arg(&part)
                .args(&args)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "icacls {args:?}");
        }
        assert!(
            icacls(&part).contains("Everyone"),
            "precondition: {}",
            icacls(&part)
        );

        let at_request: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let (seen, watched, served) = (at_request.clone(), part.clone(), data.clone());
        let server = MockServer::start(move |req, _| {
            seen.lock().unwrap().push(icacls(&watched));
            if req.header("range") == Some("bytes=4-") {
                MockResponse::new(206)
                    .header("Content-Range", "bytes 4-9/10")
                    .body(served[4..].to_vec())
            } else {
                MockResponse::new(200).body(served.clone())
            }
        });
        let report = Client::new(cfg())
            .download_to_path(
                &server.url("/f"),
                &dest,
                DownloadOptions {
                    resume: true,
                    expected_sha256: Some(sha_hex(&data)),
                    ..DownloadOptions::default()
                },
            )
            .unwrap();
        assert_eq!(
            report.resumed_from, 4,
            "the leftover partial must be resumed"
        );
        assert_eq!(fs::read(&dest).unwrap(), data);
        let at_request = at_request.lock().unwrap();
        assert_eq!(at_request.len(), 1);
        assert!(
            !at_request[0].contains("Everyone"),
            "the partial must be private when the request arrives: {}",
            at_request[0]
        );
        let published = icacls(&dest);
        assert!(
            !published.contains("Everyone") && !published.contains("(I)"),
            "{published}"
        );
    }
}
