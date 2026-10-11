//! The site-root scheme: denied names, symlink escape, percent-decoding, the MIME allowlist, root
//! inference and document grants. Driven through `serve_site_request` with a granted token, the same
//! path the `<app>-site://` protocol takes.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use rightkit_shell::broker::PathBroker;
use rightkit_shell::site::{
    grant_document_site, infer_site_root, percent_decode, serve_site_request, site_base_url,
    site_mime, split_site_request, SitePolicy,
};
use tauri::http::{header, Request, Response, StatusCode};

const SCHEME: &str = "demo-site";

fn write(root: &Path, rel: &str, body: &[u8]) {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, body).unwrap();
}

fn serve(broker: &PathBroker, policy: &SitePolicy, token: &str, rel: &str) -> Response<Vec<u8>> {
    let request = Request::builder()
        .uri(format!("{SCHEME}://localhost/{token}/{rel}"))
        .body(Vec::new())
        .unwrap();
    let (tx, rx) = mpsc::channel();
    serve_site_request(broker, policy, &request, move |response| {
        let _ = tx.send(response);
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("serve_site_request responds exactly once")
}

/// A granted root holding the given files, plus its token.
fn granted_root(files: &[(&str, &[u8])]) -> (tempfile::TempDir, PathBroker, String) {
    let dir = tempfile::tempdir().unwrap();
    for (rel, body) in files {
        write(dir.path(), rel, body);
    }
    let broker = PathBroker::default();
    let token = broker.grant_site_root(dir.path()).unwrap();
    (dir, broker, token)
}

#[test]
fn denied_names_are_refused_by_default_whatever_their_case() {
    let (_dir, broker, token) = granted_root(&[
        (".env", b"SECRET=1"),
        (".env.local", b"SECRET=2"),
        (".npmrc", b"//registry:_authToken=x"),
        (".netrc", b"machine x"),
        (".ENV.Production", b"SECRET=3"),
        ("ok.css", b"body{}"),
    ]);
    let policy = SitePolicy::default();
    for denied in [".env", ".env.local", ".npmrc", ".netrc", ".ENV.Production"] {
        assert_eq!(
            serve(&broker, &policy, &token, denied).status(),
            StatusCode::FORBIDDEN,
            "{denied} must be refused"
        );
    }
    assert_eq!(
        serve(&broker, &policy, &token, "ok.css").status(),
        StatusCode::OK
    );
}

#[test]
fn denied_names_are_configurable() {
    let (_dir, broker, token) = granted_root(&[
        ("secret.css", b"a{}"),
        ("secrets-notes.css", b"b{}"),
        ("other.css", b"c{}"),
    ]);
    let policy = SitePolicy::default().deny("secret*");
    assert_eq!(
        serve(&broker, &policy, &token, "secret.css").status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        serve(&broker, &policy, &token, "secrets-notes.css").status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        serve(&broker, &policy, &token, "other.css").status(),
        StatusCode::OK
    );
}

#[cfg(unix)]
#[test]
fn symlink_escape_and_symlink_to_a_denied_file_are_refused() {
    let outside = tempfile::tempdir().unwrap();
    let outside_file = outside.path().join("outside.css");
    std::fs::write(&outside_file, b"x{}").unwrap();
    let (dir, broker, token) = granted_root(&[(".env", b"SECRET=1"), ("ok.css", b"a{}")]);
    std::os::unix::fs::symlink(&outside_file, dir.path().join("escape.css")).unwrap();
    std::os::unix::fs::symlink(dir.path().join(".env"), dir.path().join("alias.css")).unwrap();
    let policy = SitePolicy::default();

    assert_eq!(
        serve(&broker, &policy, &token, "escape.css").status(),
        StatusCode::FORBIDDEN
    );
    // The requested name looks harmless; the resolved target is a denied file.
    assert_eq!(
        serve(&broker, &policy, &token, "alias.css").status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        serve(&broker, &policy, &token, "ok.css").status(),
        StatusCode::OK
    );
}

#[test]
fn traversal_is_refused_even_when_percent_encoded() {
    let (_dir, broker, token) = granted_root(&[("ok.css", b"a{}")]);
    let policy = SitePolicy::default();
    assert_eq!(
        serve(&broker, &policy, &token, "../ok.css").status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        serve(&broker, &policy, &token, "..%2Fok.css").status(),
        StatusCode::FORBIDDEN
    );
}

#[test]
fn percent_decoding_resolves_spaces_and_unicode_and_passes_bad_escapes_through() {
    let (_dir, broker, token) = granted_root(&[
        ("my photo.png", b"png"),
        ("100%zz.css", b"a{}"),
        ("caf\u{e9}.css", b"b{}"),
    ]);
    let policy = SitePolicy::default();
    assert_eq!(
        serve(&broker, &policy, &token, "my%20photo.png").status(),
        StatusCode::OK
    );
    assert_eq!(
        serve(&broker, &policy, &token, "100%zz.css").status(),
        StatusCode::OK
    );
    assert_eq!(
        serve(&broker, &policy, &token, "caf%C3%A9.css").status(),
        StatusCode::OK
    );

    assert_eq!(percent_decode("a%20b"), "a b");
    assert_eq!(percent_decode("%E2%9C%93"), "\u{2713}");
    assert_eq!(percent_decode("100%"), "100%");
    assert_eq!(percent_decode("%zz"), "%zz");
    assert_eq!(
        split_site_request("/tok/a%2Fb.css"),
        Some(("tok".into(), "a/b.css".into()))
    );
    assert_eq!(split_site_request("/tok/"), None);
    assert_eq!(split_site_request("/tok"), None);
}

#[test]
fn only_allowlisted_mime_types_are_served_with_nosniff() {
    let (_dir, broker, token) = granted_root(&[
        ("page.html", b"<p>x</p>"),
        ("tool.exe", b"MZ"),
        ("icon.svg", b"<svg/>"),
        ("app.js", b"void 0;"),
        ("favicon.ico", b"\0\0"),
    ]);
    let policy = SitePolicy::default();

    // Documents and unknown types are never served as sub-resources.
    assert_eq!(
        serve(&broker, &policy, &token, "page.html").status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    assert_eq!(
        serve(&broker, &policy, &token, "tool.exe").status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );

    let svg = serve(&broker, &policy, &token, "icon.svg");
    assert_eq!(svg.status(), StatusCode::OK);
    assert_eq!(svg.headers()[header::CONTENT_TYPE], "image/svg+xml");
    assert_eq!(svg.headers()[header::X_CONTENT_TYPE_OPTIONS], "nosniff");

    let js = serve(&broker, &policy, &token, "app.js");
    assert_eq!(js.headers()[header::CONTENT_TYPE], "text/javascript");

    let ico = serve(&broker, &policy, &token, "favicon.ico");
    assert_eq!(ico.headers()[header::CONTENT_TYPE], "image/x-icon");

    assert_eq!(site_mime("HTML"), None);
    assert_eq!(site_mime("woff2"), Some("font/woff2"));
}

#[test]
fn ranged_reads_return_partial_content() {
    let (_dir, broker, token) = granted_root(&[("clip.mp4", b"0123456789")]);
    let request = Request::builder()
        .uri(format!("{SCHEME}://localhost/{token}/clip.mp4"))
        .header(header::RANGE, "bytes=2-4")
        .body(Vec::new())
        .unwrap();
    let (tx, rx) = mpsc::channel();
    serve_site_request(&broker, &SitePolicy::default(), &request, move |r| {
        let _ = tx.send(r);
    });
    let response = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.body(), b"234");
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-4/10");
}

#[test]
fn root_is_inferred_from_a_project_marker_above_the_document() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    write(&project, "package.json", b"{}");
    write(&project, "docs/deep/page.html", b"<p/>");
    let page = project.join("docs/deep/page.html");

    let root = infer_site_root(&page, &SitePolicy::default()).unwrap();
    assert_eq!(root, std::fs::canonicalize(&project).unwrap());

    // A git directory is a marker too.
    let repo = dir.path().join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    write(&repo, "index.html", b"<p/>");
    assert_eq!(
        infer_site_root(&repo.join("index.html"), &SitePolicy::default()).unwrap(),
        std::fs::canonicalize(&repo).unwrap()
    );

    // An app-declared marker counts when the policy names it.
    let custom = dir.path().join("custom");
    write(&custom, ".site-root", b"");
    write(&custom, "p.html", b"<p/>");
    let policy = SitePolicy::default().marker(".site-root");
    assert_eq!(
        infer_site_root(&custom.join("p.html"), &policy).unwrap(),
        std::fs::canonicalize(&custom).unwrap()
    );
}

#[test]
fn a_document_without_markers_falls_back_to_its_own_folder() {
    let dir = tempfile::tempdir().unwrap();
    let loose = dir.path().join("loose");
    write(&loose, "page.html", b"<p/>");
    assert_eq!(
        infer_site_root(&loose.join("page.html"), &SitePolicy::default()).unwrap(),
        std::fs::canonicalize(&loose).unwrap()
    );
}

#[test]
fn a_document_grant_needs_a_granted_document_and_covers_its_site() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("proj");
    write(&project, "package.json", b"{}");
    write(&project, "style.css", b"a{}");
    write(&project, "index.html", b"<p/>");
    let broker = PathBroker::default();
    let policy = SitePolicy::default();

    let ungranted = project.join("index.html");
    assert!(grant_document_site(&broker, ungranted.to_str().unwrap(), &policy).is_err());

    let granted = broker.grant_file(&ungranted).unwrap();
    let grant = grant_document_site(&broker, &granted, &policy).unwrap();
    assert_eq!(
        broker.resolve_site_root(&grant.token).unwrap(),
        std::fs::canonicalize(&project).unwrap()
    );
    assert_eq!(
        serve(&broker, &policy, &grant.token, "style.css").status(),
        StatusCode::OK
    );
    assert_eq!(
        site_base_url(SCHEME, &grant.token),
        if cfg!(windows) {
            format!("http://{SCHEME}.localhost/{}/", grant.token)
        } else {
            format!("{SCHEME}://localhost/{}/", grant.token)
        }
    );
}
