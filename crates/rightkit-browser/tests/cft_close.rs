//! Launch through the Chrome for Testing resolver, close gracefully, and prove nothing is
//! left behind: no process, no temporary profile, no new macOS code-sign clone.
//!
//! The browser is the installed Chrome for Testing (a tool installation, not user data). Each
//! session runs on a temporary profile directory that is removed when the session closes; no
//! test opens a real browser profile.
use rightkit_browser::{chrome_for_testing_path, BrowserSession, LaunchOptions};
use std::collections::BTreeSet;
use std::path::PathBuf;

/// Required native qualification must never pass without exercising Chrome.
fn required_chrome() -> Option<PathBuf> {
    match chrome_for_testing_path(None) {
        Ok(path) => Some(path),
        Err(error) => {
            let assets_required =
                std::env::var_os("RIGHTKIT_REQUIRE_ASSETS").is_some_and(|value| value == "1");
            let github_actions =
                std::env::var_os("GITHUB_ACTIONS").is_some_and(|value| value == "true");
            if assets_required || github_actions {
                panic!("Chrome for Testing close qualification: {error}");
            }
            println!("[browser] SKIP Chrome for Testing close qualification: {error}");
            None
        }
    }
}

/// macOS per-user `X` dirs (from `DARWIN_USER_TEMP_DIR` and `TMPDIR`) holding code-sign clones.
fn clone_roots() -> Vec<PathBuf> {
    if !cfg!(target_os = "macos") {
        return Vec::new();
    }
    let mut temps = Vec::new();
    if let Ok(out) = std::process::Command::new("getconf")
        .arg("DARWIN_USER_TEMP_DIR")
        .output()
    {
        let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !t.is_empty() {
            temps.push(PathBuf::from(t));
        }
    }
    temps.push(std::env::temp_dir());
    let mut roots: Vec<PathBuf> = temps
        .iter()
        .filter_map(|t| t.parent().map(|p| p.join("X")))
        .filter(|x| x.is_dir())
        .collect();
    roots.sort();
    roots.dedup();
    roots
}

/// Every entry under every `*code_sign_clone` directory (e.g. `com.google.Chrome.code_sign_clone`).
fn clone_entries() -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    for root in clone_roots() {
        let Ok(rd) = std::fs::read_dir(&root) else {
            continue;
        };
        for d in rd.flatten() {
            if !d.file_name().to_string_lossy().ends_with("code_sign_clone") {
                continue;
            }
            if let Ok(inner) = std::fs::read_dir(d.path()) {
                out.extend(inner.flatten().map(|e| e.path()));
            }
        }
    }
    out
}

#[cfg(unix)]
fn group_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", "--", &format!("-{pid}")])
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

#[tokio::test(flavor = "multi_thread")]
async fn chrome_for_testing_closes_gracefully_without_leftovers() {
    let Some(exe) = required_chrome() else {
        return;
    };
    let before = clone_entries();
    let session = BrowserSession::launch(LaunchOptions::default())
        .await
        .unwrap();
    let profile = session.profile_dir().to_path_buf();
    let pid = session.process_id().unwrap();
    assert!(profile.is_dir());
    let page = session.new_page("about:blank").await.unwrap();
    drop(page);

    session.shutdown().await;
    assert!(
        !profile.exists(),
        "temporary profile must be removed after shutdown: {}",
        profile.display()
    );
    #[cfg(unix)]
    assert!(!group_alive(pid), "Chrome process group {pid} must be gone");
    let _ = pid;
    drop(session);

    let created: Vec<_> = clone_entries().difference(&before).cloned().collect();
    assert!(
        created.is_empty(),
        "{} left code-sign clones: {created:?}",
        exe.display()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn dropped_session_terminates_gracefully_and_removes_profile() {
    if required_chrome().is_none() {
        return;
    }
    let before = clone_entries();
    let session = BrowserSession::launch(LaunchOptions::default())
        .await
        .unwrap();
    let profile = session.profile_dir().to_path_buf();
    let pid = session.process_id().unwrap();
    // No shutdown: Drop must SIGTERM the group, wait (bounded), and only then SIGKILL.
    let started = std::time::Instant::now();
    tokio::task::spawn_blocking(move || drop(session))
        .await
        .unwrap();
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
    assert!(
        !profile.exists(),
        "temporary profile must be removed on drop"
    );
    #[cfg(unix)]
    assert!(!group_alive(pid), "Chrome process group {pid} must be gone");
    let _ = pid;
    let created: Vec<_> = clone_entries().difference(&before).cloned().collect();
    assert!(created.is_empty(), "left code-sign clones: {created:?}");
}

#[test]
fn explicit_and_env_paths_win_and_missing_paths_fail_loudly() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("chrome-for-testing");
    std::fs::write(&exe, "").unwrap();
    assert_eq!(chrome_for_testing_path(Some(&exe)).unwrap(), exe);
    let err = chrome_for_testing_path(Some(&dir.path().join("missing")))
        .unwrap_err()
        .to_string();
    assert!(err.contains("does not exist"), "{err}");
    assert!(err.contains("ensure_chrome_for_testing"), "{err}");
    assert!(err.contains("RIGHTKIT_CHROME_FOR_TESTING"), "{err}");
    assert!(rightkit_browser::chrome_for_testing_candidates()
        .iter()
        .all(|p| !p.to_string_lossy().contains("Google Chrome.app")));
}
