//! Outcome tests: real headless Chrome against a local static page. Every
//! assertion reads page state produced by trusted browser input.
use rightkit_browser::{BrowserError, BrowserSession, LaunchOptions, NetworkPolicy, Target};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Duration;

/// Every test here drives a real Chrome for Testing. Without one the test prints a SKIP line
/// and returns; `RIGHTKIT_REQUIRE_ASSETS=1` turns the gap into a failure.
macro_rules! need_chrome {
    ($test:literal) => {
        if let Err(e) = rightkit_browser::chrome_for_testing_path(None) {
            if std::env::var_os("RIGHTKIT_REQUIRE_ASSETS").is_some_and(|v| v == "1") {
                panic!("{}: {e}", $test);
            }
            println!("[browser] SKIP {}: {e}", $test);
            return;
        }
    };
}

const INDEX: &str = r##"<!doctype html><html><head><title>RK Test</title><style>
#hov{width:120px;height:40px;background:#ccc} #hov:hover{background:#0a0}
#box{width:60px;height:60px;background:#36f;position:absolute;left:300px;top:200px;user-select:none}
#zone{width:200px;height:120px;position:absolute;left:500px;top:150px;border:1px solid #000}
</style></head><body>
<button id="btn" onclick="document.getElementById('out').textContent='clicked trusted='+event.isTrusted">Go</button>
<div id="hov" onmouseenter="window.__hover=event.isTrusted"></div>
<input id="txt" onkeydown="window.__keys=(window.__keys||[]).concat([event.key+':'+event.isTrusted])">
<div id="box"></div><div id="zone"></div>
<a id="dl" href="/file.txt" download>Download</a>
<input id="up" type="file" onchange="document.getElementById('upname').textContent=this.files[0].name">
<span id="upname"></span><p id="out"></p>
<script>
const box=document.getElementById('box');let drag=false;window.__drag={};
box.addEventListener('mousedown',e=>{drag=true;window.__drag.trusted=e.isTrusted});
addEventListener('mousemove',e=>{if(drag){box.style.left=(e.clientX-30)+'px';box.style.top=(e.clientY-30)+'px'}});
addEventListener('mouseup',e=>{if(drag){drag=false;const z=document.getElementById('zone').getBoundingClientRect();
 window.__drag.dropped=e.clientX>z.left&&e.clientX<z.right&&e.clientY>z.top&&e.clientY<z.bottom}});
console.log('hello-console');console.error('boom-console');
</script></body></html>"##;

/// Loopback fixtures are blocked by default; this suite opts in explicitly.
fn fixture_opts() -> LaunchOptions {
    LaunchOptions::default().network(NetworkPolicy::default().allow_host("127.0.0.1"))
}

fn serve() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let mut s = s;
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (ct, extra, body) = if path.starts_with("/file.txt") {
                (
                    "text/plain",
                    "Content-Disposition: attachment; filename=\"got.txt\"\r\n",
                    "download-body",
                )
            } else if path == "/other" {
                (
                    "text/html",
                    "",
                    "<title>Other</title><p id=o>second page</p>",
                )
            } else {
                ("text/html", "", INDEX)
            };
            let _ = write!(s, "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    });
    format!("http://127.0.0.1:{port}")
}

#[tokio::test(flavor = "multi_thread")]
async fn browser_runtime_outcomes() {
    need_chrome!("browser_runtime_outcomes");
    let base = serve();
    let session = BrowserSession::launch(fixture_opts())
        .await
        .expect("launch");
    let p = session.new_page(&format!("{base}/")).await.unwrap();
    assert_eq!(p.title().await, "RK Test");

    // Console capture (listener attached before navigation).
    p.reload().await.unwrap();
    let console = p.console().await;
    assert!(
        console
            .iter()
            .any(|l| l.level == "log" && l.text.contains("hello-console")),
        "{console:?}"
    );
    assert!(console
        .iter()
        .any(|l| l.level == "error" && l.text.contains("boom-console")));

    // Real click.
    p.click(&Target::css("#btn")).await.unwrap();
    assert!(p.text().await.unwrap().contains("clicked trusted=true"));

    // Real hover: :hover matches only with a genuine pointer position.
    p.hover(&Target::css("#hov")).await.unwrap();
    assert_eq!(
        p.eval("document.getElementById('hov').matches(':hover')")
            .await
            .unwrap(),
        true
    );
    assert_eq!(p.eval("window.__hover").await.unwrap(), true);

    // Real keyboard.
    p.fill(&Target::css("#txt"), "hi", true).await.unwrap();
    p.press("Enter").await.unwrap();
    assert_eq!(
        p.eval("document.getElementById('txt').value")
            .await
            .unwrap(),
        "hi"
    );
    let keys = p.eval("window.__keys.join(',')").await.unwrap();
    assert!(
        keys.as_str().unwrap().contains("h:true") && keys.as_str().unwrap().contains("Enter:true"),
        "{keys}"
    );

    // Real drag into the drop zone.
    p.drag(&Target::css("#box"), &Target::Point(600.0, 210.0), 8)
        .await
        .unwrap();
    assert_eq!(p.eval("window.__drag.trusted").await.unwrap(), true);
    assert_eq!(p.eval("window.__drag.dropped").await.unwrap(), true);

    // Observation refs and stale checks.
    let obs = p.observe().await.unwrap();
    let btn = obs
        .nodes
        .iter()
        .find(|n| n.tag == "button")
        .unwrap()
        .node_ref
        .clone();
    p.click(&Target::node_ref(btn.clone())).await.unwrap();
    p.reload().await.unwrap();
    assert!(matches!(
        p.click(&Target::node_ref(btn)).await,
        Err(BrowserError::StaleRef(_))
    ));

    // Screenshot is a PNG.
    let png = p.screenshot().await.unwrap();
    assert_eq!(&png[..4], &[0x89, b'P', b'N', b'G']);

    // Upload.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("up.txt");
    std::fs::write(&f, "x").unwrap();
    p.upload(&Target::css("#up"), &f).await.unwrap();
    p.wait_eval(
        "document.getElementById('upname').textContent==='up.txt'",
        Duration::from_secs(5),
    )
    .await
    .unwrap();

    // Download.
    let before = session.downloads();
    p.click(&Target::css("#dl")).await.unwrap();
    let got = session
        .wait_download(&before, Duration::from_secs(15))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&got).unwrap(), "download-body");

    // Multi-page.
    let p2 = session.new_page(&format!("{base}/other")).await.unwrap();
    assert_eq!(session.pages().len(), 2);
    assert_eq!(session.current().unwrap().id(), p2.id());
    assert_eq!(p2.title().await, "Other");
    assert_eq!(p.title().await, "RK Test");
    session.select_page(p.id()).await.unwrap();
    session.close_page(p2.id()).await.unwrap();
    assert_eq!(session.pages().len(), 1);

    assert!(session.is_headless());
    session.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn named_profile_persists_and_is_exclusive() {
    need_chrome!("named_profile_persists_and_is_exclusive");
    let base = serve();
    let root = tempfile::tempdir().unwrap();
    let opts = || fixture_opts().named_profile(root.path(), "qa");
    let s1 = BrowserSession::launch(opts()).await.unwrap();
    assert!(matches!(
        BrowserSession::launch(opts()).await,
        Err(BrowserError::ProfileInUse(_))
    ));
    let p = s1.new_page(&format!("{base}/")).await.unwrap();
    p.eval("localStorage.setItem('k','persisted'); document.cookie='c=1; max-age=3600'; 1")
        .await
        .unwrap();
    s1.shutdown().await;
    drop(s1);
    let s2 = BrowserSession::launch(opts()).await.unwrap();
    let p = s2.new_page(&format!("{base}/")).await.unwrap();
    assert_eq!(
        p.eval("localStorage.getItem('k')").await.unwrap(),
        "persisted"
    );
    s2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "opens a visible window; run with --ignored"]
async fn headed_smoke() {
    need_chrome!("headed_smoke");
    let base = serve();
    let s = BrowserSession::launch(fixture_opts().headed())
        .await
        .unwrap();
    assert!(!s.is_headless());
    let p = s.new_page(&format!("{base}/")).await.unwrap();
    p.click(&Target::css("#btn")).await.unwrap();
    assert!(p.text().await.unwrap().contains("clicked trusted=true"));
    s.shutdown().await;
}
