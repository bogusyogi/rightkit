//! Open-path queue, argv filter and broker grants (Orthic-Labs/rightkit#386). Pure: no webview.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rightkit_tauri_shell::open_paths::{
    candidate_paths, file_paths_from_argv, grant_paths, is_path_arg, keep_regular_files,
    split_opened_urls, PendingOpenPaths, MAX_QUEUED,
};
use rightkit_tauri_shell::PathBroker;

fn touch(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, b"x").unwrap();
    path
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| (*s).to_owned()).collect()
}

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

#[test]
fn path_args_exclude_flags_urls_and_junk() {
    for ok in [
        "a.md",
        "a b.txt",
        "rel/c.txt",
        "/tmp/a.md",
        r"C:\docs\b.docx",
    ] {
        assert!(is_path_arg(ok), "{ok:?}");
    }
    for bad in [
        "-psn_0_12345",
        "--background",
        "-x",
        "",
        "   ",
        "bad\0path",
        "line\nbreak",
        "https://example.com/x",
        "myapp://open/1",
        "mailto:a@b.c",
    ] {
        assert!(!is_path_arg(bad), "{bad:?}");
    }
}

#[test]
fn candidates_join_relative_paths_to_the_base_and_never_to_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path();
    let abs = base.join("abs.md");
    let args = [
        "--flag".to_owned(),
        "doc.md".to_owned(),
        abs.to_string_lossy().into_owned(),
        "https://example.com/".to_owned(),
        "doc.md".to_owned(),
    ];
    // Relative joins the base; a repeat is dropped; flags and URLs are gone.
    assert_eq!(
        candidate_paths(args.iter().map(String::as_str), Some(base)),
        [base.join("doc.md"), abs.clone()]
    );
    // No usable base: a relative path is dropped rather than resolved against some other directory.
    assert_eq!(
        candidate_paths(args.iter().map(String::as_str), None),
        std::slice::from_ref(&abs)
    );
    assert_eq!(
        candidate_paths(
            args.iter().map(String::as_str),
            Some(Path::new("relative/base"))
        ),
        [abs]
    );
}

#[test]
fn argv_skips_the_program_and_keeps_only_existing_regular_files() {
    let dir = tempfile::tempdir().unwrap();
    let cwd = dir.path().to_str().unwrap().to_owned();
    let here = touch(dir.path(), "here.md");
    let absolute = touch(dir.path(), "absolute.md");
    std::fs::create_dir(dir.path().join("folder")).unwrap();
    let argv = argv(&[
        // argv[0] is the executable, even when it exists and looks like a file.
        absolute.to_str().unwrap(),
        "--background",
        "here.md",
        "missing.md",
        "folder",
        absolute.to_str().unwrap(),
        "mailto:a@b.c",
    ]);
    assert_eq!(
        file_paths_from_argv(&argv, &cwd),
        [dir.path().join("here.md"), absolute.clone()]
    );
    assert!(here.is_file());
    // The second launch's cwd decides relative paths; without one they are dropped.
    assert_eq!(
        file_paths_from_argv(&argv, ""),
        std::slice::from_ref(&absolute)
    );
    assert_eq!(file_paths_from_argv(&argv, "not/absolute"), [absolute]);
}

#[test]
fn nonexistent_files_are_dropped_by_the_existence_filter() {
    let dir = tempfile::tempdir().unwrap();
    let real = touch(dir.path(), "real.txt");
    let kept = keep_regular_files(vec![
        dir.path().join("gone.txt"),
        real.clone(),
        dir.path().to_path_buf(),
    ]);
    assert_eq!(kept, [real]);
}

#[test]
fn paths_wait_for_the_renderer_then_deliver_live() {
    let q = PendingOpenPaths::from_paths(vec![p("/launch/one.md")]);
    assert_eq!(q.pending(), 1);
    assert!(!q.is_ready());
    // Not ready: queued behind the launch path, nothing delivered now.
    assert!(q
        .queue_or_take(vec![p("/two.md"), p("/three.md")])
        .is_empty());
    assert_eq!(q.pending(), 3);
    // Ready: everything, in arrival order, exactly once.
    assert_eq!(
        q.mark_ready_and_drain(),
        [p("/launch/one.md"), p("/two.md"), p("/three.md")]
    );
    assert!(q.is_ready());
    assert_eq!(q.pending(), 0);
    assert!(q.mark_ready_and_drain().is_empty());
    // After ready: handed straight back, never queued.
    assert_eq!(q.queue_or_take(vec![p("/four.md")]), [p("/four.md")]);
    assert_eq!(q.pending(), 0);
}

#[test]
fn the_queue_is_bounded_and_keeps_the_oldest() {
    let q = PendingOpenPaths::new();
    let many: Vec<PathBuf> = (0..MAX_QUEUED + 50).map(|i| p(&format!("/f{i}"))).collect();
    assert!(q.queue_or_take(many).is_empty());
    assert_eq!(q.pending(), MAX_QUEUED);
    assert!(q.queue_or_take(vec![p("/extra")]).is_empty());
    assert_eq!(q.pending(), MAX_QUEUED);
    let first = q.mark_ready_and_drain();
    assert_eq!(first.len(), MAX_QUEUED);
    assert_eq!(first[0], p("/f0"));
    assert_eq!(first[MAX_QUEUED - 1], p(&format!("/f{}", MAX_QUEUED - 1)));
    let seeded =
        PendingOpenPaths::from_paths((0..MAX_QUEUED + 5).map(|i| p(&format!("/s{i}"))).collect());
    assert_eq!(seeded.pending(), MAX_QUEUED);
}

#[test]
fn every_path_is_delivered_exactly_once_whenever_ready_arrives() {
    for round in 0..20 {
        let q = Arc::new(PendingOpenPaths::new());
        let senders: Vec<_> = (0..4)
            .map(|t| {
                let q = Arc::clone(&q);
                std::thread::spawn(move || {
                    let mut live = Vec::new();
                    for i in 0..25 {
                        live.extend(q.queue_or_take(vec![p(&format!("/t{t}-{i}"))]));
                    }
                    live
                })
            })
            .collect();
        let drained = if round % 2 == 0 {
            std::thread::yield_now();
            q.mark_ready_and_drain()
        } else {
            q.mark_ready_and_drain()
        };
        let mut seen = drained;
        for sender in senders {
            seen.extend(sender.join().unwrap());
        }
        seen.sort();
        let mut expected: Vec<PathBuf> = (0..4)
            .flat_map(|t| (0..25).map(move |i| p(&format!("/t{t}-{i}"))))
            .collect();
        expected.sort();
        assert_eq!(
            seen, expected,
            "round {round}: a path was lost or delivered twice"
        );
    }
}

#[test]
fn the_renderer_only_receives_broker_granted_paths() {
    let dir = tempfile::tempdir().unwrap();
    let real = touch(dir.path(), "real.md");
    let other = touch(dir.path(), "other.md");
    let broker = PathBroker::new();
    let granted = grant_paths(
        &broker,
        vec![
            real.clone(),
            dir.path().join("vanished.md"),
            dir.path().to_path_buf(),
            real.clone(),
        ],
    );
    // The vanished file, the directory and the repeat are skipped.
    assert_eq!(granted.len(), 1);
    // The renderer-visible string is a real grant ...
    assert!(broker.require_file(&granted[0]).is_ok());
    // ... and a file nobody granted stays denied.
    assert!(broker.require_file(other.to_str().unwrap()).is_err());
}

#[test]
fn opened_urls_split_into_documents_and_links() {
    let dir = tempfile::tempdir().unwrap();
    let doc = touch(dir.path(), "doc.md");
    let file_url = tauri::Url::from_file_path(&doc).unwrap();
    let https = tauri::Url::parse("https://example.com/a").unwrap();
    let custom = tauri::Url::parse("myapp://open/1").unwrap();
    let opened = split_opened_urls([&file_url, &https, &file_url, &custom]);
    assert_eq!(opened.files, [doc]);
    assert_eq!(opened.links, ["https://example.com/a", "myapp://open/1"]);
}

// A `file:` URL with a remote host is a UNC path on Windows and nothing a Mac or Linux can open.
#[cfg(not(windows))]
#[test]
fn a_remote_file_url_is_neither_a_document_nor_a_link() {
    let remote = tauri::Url::parse("file://fileserver/share/doc.md").unwrap();
    let opened = split_opened_urls([&remote]);
    assert!(opened.files.is_empty());
    assert!(opened.links.is_empty());
}
