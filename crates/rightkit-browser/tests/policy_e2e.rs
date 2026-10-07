//! Admission, SSRF policy, profile isolation, and process ownership against
//! real headless Chrome and local fixture servers (offline). Fixtures count
//! requests that actually reach them, so "blocked" means the server never saw it.
use rightkit_browser::{
    ActionKind, AddressClass, AdmissionHook, BlockReason, BrowserError, BrowserSession, Decision,
    LaunchOptions, LifecycleKind, NetworkPolicy, Target,
};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Each test drives real Chrome processes and asserts on process lifetimes with tight
/// bounds; run them one at a time so parallel browsers cannot starve each other.
static SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Clone)]
struct Fixture {
    port: u16,
    hits: Arc<Mutex<HashMap<String, usize>>>,
}

impl Fixture {
    fn start(redirect_to: Option<String>, secret_url: Option<String>) -> Fixture {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        let hits: Arc<Mutex<HashMap<String, usize>>> = Arc::default();
        let h = hits.clone();
        std::thread::spawn(move || {
            for s in l.incoming().flatten() {
                let (h, redirect_to, secret_url) =
                    (h.clone(), redirect_to.clone(), secret_url.clone());
                std::thread::spawn(move || {
                    let mut s = s;
                    let mut buf = [0u8; 4096];
                    let n = s.read(&mut buf).unwrap_or(0);
                    if n == 0 {
                        return; // speculative preconnect, no request
                    }
                    let req = String::from_utf8_lossy(&buf[..n]);
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    *h.lock().unwrap().entry(path.clone()).or_default() += 1;
                    let resp = if path == "/redir" {
                        format!(
                            "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                            redirect_to.unwrap_or_default()
                        )
                    } else {
                        let body = match path.as_str() {
                            "/sub" => format!(
                                "<title>Sub</title><img src='{0}/pixel.png'><iframe src='{0}/frame'></iframe><p id=ok>ok</p>",
                                secret_url.unwrap_or_default()
                            ),
                            "/btn" => "<title>Btn</title><button id=b onclick=\"document.title='clicked'\">Go</button>".to_string(),
                            _ => "<title>Home</title><p>home</p>".to_string(),
                        };
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                    };
                    let _ = s.write_all(resp.as_bytes());
                });
            }
        });
        Fixture { port, hits }
    }
    fn base(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }
    fn hits(&self, path: &str) -> usize {
        *self.hits.lock().unwrap().get(path).unwrap_or(&0)
    }
}

fn pids_for(marker: &str) -> Vec<u32> {
    // One "<pid> <command line>" per process on both families.
    #[cfg(not(windows))]
    let out = Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
        .unwrap();
    #[cfg(windows)]
    let out = Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            "Get-CimInstance Win32_Process | ForEach-Object { [string]$_.ProcessId + ' ' + $_.CommandLine }",
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter(|l| l.contains(marker))
        .filter_map(|l| l.split_whitespace().next()?.parse().ok())
        .collect()
}

async fn wait_gone(marker: &str) -> Vec<u32> {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let p = pids_for(marker);
        if p.is_empty() || Instant::now() > end {
            return p;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn default_policy_blocks_before_any_request_and_callback_allows() {
    let _serial = SERIAL.lock().await;
    let fx = Fixture::start(None, None);
    let denied: Arc<Mutex<Vec<String>>> = Arc::default();
    let d = denied.clone();
    let opts = LaunchOptions::default().on_event(move |e| {
        if e.kind == LifecycleKind::Denied {
            d.lock().unwrap().push(e.reason.clone());
        }
    });
    let s = BrowserSession::launch(opts).await.unwrap();
    // Loopback, localhost, link-local metadata endpoint, private range, file://.
    for url in [
        format!("{}/", fx.base()),
        format!("http://localhost:{}/", fx.port),
        "http://169.254.169.254/latest/meta-data".to_string(),
        "http://10.0.0.5/".to_string(),
        "http://[::1]/".to_string(),
        "http://2130706433/".to_string(), // decimal form of 127.0.0.1
        "file:///etc/hosts".to_string(),
    ] {
        let err = s
            .new_page(&url)
            .await
            .err()
            .unwrap_or_else(|| panic!("{url} was not denied"));
        assert!(matches!(err, BrowserError::Blocked(_)), "{url}: {err:?}");
    }
    assert_eq!(
        fx.hits("/"),
        0,
        "denied navigation must not reach the server"
    );
    assert!(denied.lock().unwrap().len() >= 7);
    s.shutdown().await;

    // Per-request allow callback opens exactly that target.
    let port = fx.port;
    let opts = LaunchOptions::default().network(
        NetworkPolicy::default().allow_with(move |r| r.host == "127.0.0.1" && r.port == Some(port)),
    );
    let s = BrowserSession::launch(opts).await.unwrap();
    let p = s.new_page(&format!("{}/", fx.base())).await.unwrap();
    assert_eq!(p.title().await, "Home");
    assert_eq!(fx.hits("/"), 1);
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn redirects_and_subresources_are_blocked_in_the_browser() {
    let _serial = SERIAL.lock().await;
    // `secret` is a loopback service the policy never allows.
    let secret = Fixture::start(None, None);
    let allowed = Fixture::start(
        Some(format!("{}/redir-target", secret.base())),
        Some(secret.base()),
    );
    let ap = allowed.port;
    let opts = LaunchOptions::default().network(
        NetworkPolicy::default().allow_with(move |r| r.host == "127.0.0.1" && r.port == Some(ap)),
    );
    let s = BrowserSession::launch(opts).await.unwrap();

    // Redirect hop to the blocked service: the second hop is stopped in the browser.
    let _ = s.new_page(&format!("{}/redir", allowed.base())).await;
    assert_eq!(allowed.hits("/redir"), 1);
    assert_eq!(
        secret.hits("/redir-target"),
        0,
        "redirect target reached the blocked service"
    );

    // Image and cross-site iframe subresources pointing at the blocked service.
    let p = s
        .new_page(&format!("{}/sub", allowed.base()))
        .await
        .unwrap();
    assert_eq!(p.title().await, "Sub");
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(secret.hits("/pixel.png"), 0);
    assert_eq!(secret.hits("/frame"), 0);
    let blocked = p.blocked_requests();
    assert!(
        blocked.iter().any(|b| b.url.ends_with("/pixel.png")),
        "{blocked:?}"
    );
    assert!(
        blocked.iter().any(|b| b.url.ends_with("/frame")),
        "{blocked:?}"
    );
    // Script cannot fetch it either.
    let r = p
        .eval(&format!(
            "fetch('{}/xhr').then(()=>'reached').catch(()=>'blocked')",
            secret.base()
        ))
        .await
        .unwrap();
    let _ = r;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(secret.hits("/xhr"), 0);
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn admission_denies_before_side_effect_and_sees_typed_requests() {
    let _serial = SERIAL.lock().await;
    let fx = Fixture::start(None, None);
    // (action, url, target) for every admission request.
    type Seen = Arc<Mutex<Vec<(ActionKind, Option<String>, Option<String>)>>>;
    let seen: Seen = Arc::default();
    let log = seen.clone();
    let hook: AdmissionHook = Arc::new(move |req| {
        let log = log.clone();
        Box::pin(async move {
            log.lock()
                .unwrap()
                .push((req.action, req.url.clone(), req.target.clone()));
            if req.action == ActionKind::Click {
                Decision::Deny("clicks need approval".into())
            } else {
                Decision::Allow
            }
        })
    });
    let opts = LaunchOptions::default()
        .network(NetworkPolicy::default().allow_host("127.0.0.1"))
        .admission(hook);
    let s = BrowserSession::launch(opts).await.unwrap();
    let p = s.new_page(&format!("{}/btn", fx.base())).await.unwrap();
    let err = p.click(&Target::css("#b")).await.unwrap_err();
    assert!(
        matches!(&err, BrowserError::Denied(m) if m.contains("approval")),
        "{err:?}"
    );
    assert_eq!(p.title().await, "Btn", "denied click must not have clicked");
    // Allowed action still works and is reported with its typed fields.
    p.hover(&Target::css("#b")).await.unwrap();
    {
        let seen = seen.lock().unwrap();
        assert!(seen.iter().any(|(a, u, _)| *a == ActionKind::Navigate
            && u.as_deref().is_some_and(|u| u.ends_with("/btn"))));
        assert!(seen
            .iter()
            .any(|(a, _, t)| *a == ActionKind::Click && t.as_deref() == Some("css:#b")));
    }
    // Denied navigation never reaches the server.
    let hook: AdmissionHook =
        Arc::new(|_| Box::pin(async { Decision::Deny("no navigation".into()) }));
    let s2 = BrowserSession::launch(
        LaunchOptions::default()
            .network(NetworkPolicy::default().allow_host("127.0.0.1"))
            .admission(hook),
    )
    .await
    .unwrap();
    assert!(matches!(
        s2.new_page(&format!("{}/never", fx.base())).await,
        Err(BrowserError::Denied(_))
    ));
    assert_eq!(fx.hits("/never"), 0);
    s.shutdown().await;
    s2.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn profiles_are_isolated_and_never_the_real_browser_profile() {
    let _serial = SERIAL.lock().await;
    let fx = Fixture::start(None, None);
    let opts =
        || LaunchOptions::default().network(NetworkPolicy::default().allow_host("127.0.0.1"));
    // Ephemeral by default: state does not survive and the directory is removed.
    let s1 = BrowserSession::launch(opts()).await.unwrap();
    let dir = s1.profile_dir().to_path_buf();
    let p = s1.new_page(&format!("{}/", fx.base())).await.unwrap();
    p.eval("localStorage.setItem('k','v'); 1").await.unwrap();
    s1.shutdown().await;
    drop(s1);
    assert!(!dir.exists(), "temporary profile must be deleted");
    let s2 = BrowserSession::launch(opts()).await.unwrap();
    let p = s2.new_page(&format!("{}/", fx.base())).await.unwrap();
    assert_eq!(
        p.eval("localStorage.getItem('k')").await.unwrap(),
        serde_json::Value::Null
    );
    s2.shutdown().await;

    // Caller cannot smuggle in a different profile or debug endpoint.
    for flag in [
        "--user-data-dir=/tmp/elsewhere",
        "remote-debugging-port=9222",
        "--profile-directory=Default",
    ] {
        let mut o = opts();
        o.extra_args = vec![flag.into()];
        assert!(
            matches!(
                BrowserSession::launch(o).await,
                Err(BrowserError::Invalid(_))
            ),
            "{flag}"
        );
    }
    // Named profile root inside the real Chrome data dir is refused.
    if let Some(home) = std::env::var_os("HOME") {
        let real = std::path::Path::new(&home).join("Library/Application Support/Google/Chrome");
        let real = if real.exists() {
            real
        } else {
            std::path::Path::new(&home).join(".config/google-chrome")
        };
        if real.exists() {
            let o = opts().named_profile(&real, "rightkit-test");
            assert!(matches!(
                BrowserSession::launch(o).await,
                Err(BrowserError::Profile(_))
            ));
            assert!(!real.join("rightkit-test").exists());
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn chromium_child_is_owned_and_dies_on_shutdown_cancel_and_drop() {
    let _serial = SERIAL.lock().await;
    let opts = || LaunchOptions::default();
    // Graceful shutdown.
    let s = BrowserSession::launch(opts()).await.unwrap();
    let marker = s.profile_dir().to_string_lossy().to_string();
    let pid = s.process_id().expect("owned child pid");
    assert!(pids_for(&marker).contains(&pid));
    s.shutdown().await;
    assert!(
        wait_gone(&marker).await.is_empty(),
        "chrome processes survived shutdown"
    );
    drop(s);

    // Cancel kills immediately, helpers included.
    let s = BrowserSession::launch(opts()).await.unwrap();
    let marker = s.profile_dir().to_string_lossy().to_string();
    let _p = s.new_page("about:blank").await.unwrap();
    assert!(!pids_for(&marker).is_empty());
    s.cancel().await;
    assert!(
        wait_gone(&marker).await.is_empty(),
        "chrome processes survived cancel"
    );
    drop(s);

    // Abandoned session (dropped without shutdown) leaves nothing behind.
    let s = BrowserSession::launch(opts()).await.unwrap();
    let marker = s.profile_dir().to_string_lossy().to_string();
    let _p = s.new_page("about:blank").await.unwrap();
    assert!(!pids_for(&marker).is_empty());
    drop(_p);
    drop(s);
    assert!(
        wait_gone(&marker).await.is_empty(),
        "chrome processes survived drop"
    );
}

type Events = Arc<Mutex<Vec<rightkit_browser::BrowserEvent>>>;

fn recording() -> (Events, LaunchOptions) {
    let ev: Events = Arc::default();
    let e2 = ev.clone();
    let opts = LaunchOptions::default()
        .on_event(move |e| e2.lock().unwrap_or_else(|p| p.into_inner()).push(e.clone()));
    (ev, opts)
}

async fn wait_kind(ev: &Events, kind: LifecycleKind) -> bool {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        if ev
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .iter()
            .any(|e| e.kind == kind)
        {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    false
}

#[tokio::test(flavor = "multi_thread")]
async fn lifecycle_start_stop_and_crash_events_carry_session_reason_and_time() {
    let _serial = SERIAL.lock().await;
    // Start, then a graceful stop: exactly one of each, no crash.
    let (ev, opts) = recording();
    let s = BrowserSession::launch(opts).await.unwrap();
    let sid = s.id().to_string();
    s.shutdown().await;
    s.shutdown().await;
    drop(s);
    {
        let ev = ev.lock().unwrap();
        let kinds: Vec<_> = ev.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![LifecycleKind::Start, LifecycleKind::Stop],
            "{ev:?}"
        );
        for e in ev.iter() {
            assert_eq!(e.session_id, sid);
            assert!(!e.reason.is_empty());
            assert!(e.timestamp_ms() > 0);
            assert!(e.pid.is_some());
        }
        assert_eq!(ev[1].reason, "shutdown");
    }

    // Chrome killed from outside: a crash event, then stop on cleanup.
    let (ev, opts) = recording();
    let s = BrowserSession::launch(opts).await.unwrap();
    let pid = s.process_id().unwrap().to_string();
    #[cfg(unix)]
    let _ = Command::new("kill").args(["-9", &pid]).status();
    #[cfg(windows)]
    let _ = Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid])
        .status();
    let got = wait_kind(&ev, LifecycleKind::Crash).await;
    let seen = format!("{:?}", ev.lock().unwrap());
    assert!(got, "no crash event: {seen}");
    s.shutdown().await;
    {
        let ev = ev.lock().unwrap();
        let crash = ev.iter().find(|e| e.kind == LifecycleKind::Crash).unwrap();
        assert_eq!(crash.session_id, s.id());
        assert!(
            crash.reason.contains("exited") || crash.reason.contains("lost"),
            "{crash:?}"
        );
        assert_eq!(ev.last().unwrap().kind, LifecycleKind::Stop);
        assert_eq!(
            ev.iter().filter(|e| e.kind == LifecycleKind::Crash).count(),
            1
        );
    }

    // Renderer crash on one page: a page-scoped crash event.
    let (ev, opts) = recording();
    let s = BrowserSession::launch(opts.network(NetworkPolicy::unrestricted()))
        .await
        .unwrap();
    let p = s.new_page("about:blank").await.unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(5), p.goto("chrome://crash")).await;
    assert!(
        wait_kind(&ev, LifecycleKind::Crash).await,
        "no renderer crash event"
    );
    {
        let ev = ev.lock().unwrap();
        let crash = ev.iter().find(|e| e.kind == LifecycleKind::Crash).unwrap();
        assert_eq!(crash.page_id.as_deref(), Some(p.id()));
        assert_eq!(crash.reason, "renderer crashed");
    }
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn new_tab_and_denied_events_are_typed() {
    let _serial = SERIAL.lock().await;
    let (ev, opts) = recording();
    let s = BrowserSession::launch(opts).await.unwrap();
    let err = s
        .new_page("http://169.254.169.254/latest/meta-data/")
        .await
        .err()
        .unwrap();
    match err {
        BrowserError::Blocked(d) => {
            assert_eq!(d.resource_type, "Navigation");
            assert!(matches!(
                d.reason,
                BlockReason::Address {
                    class: AddressClass::LinkLocal,
                    ..
                }
            ));
        }
        other => panic!("{other:?}"),
    }
    let err = s.new_page("file:///etc/hosts").await.err().unwrap();
    assert!(
        matches!(err, BrowserError::Blocked(ref d) if matches!(d.reason, BlockReason::Scheme(_)))
    );
    {
        let ev = ev.lock().unwrap();
        let denied: Vec<_> = ev
            .iter()
            .filter(|e| e.kind == LifecycleKind::Denied)
            .collect();
        assert_eq!(denied.len(), 2, "{ev:?}");
        for e in denied {
            assert_eq!(e.session_id, s.id());
            assert!(e.blocked.is_some());
            assert_eq!(e.what.as_deref(), Some("Navigation"));
        }
    }
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn cross_site_iframe_and_subresources_are_intercepted() {
    let _serial = SERIAL.lock().await;
    // `localhost` and `127.0.0.1` are different sites, so the iframe is genuinely cross-site.
    let other = Fixture::start(None, None);
    let other_origin = format!("http://localhost:{}", other.port);
    let page = Fixture::start(None, Some(other_origin.clone()));
    let (pp, op) = (page.port, other.port);

    // Denied: only the top-level fixture is allowed.
    let opts = LaunchOptions::default().network(
        NetworkPolicy::default().allow_with(move |r| r.host == "127.0.0.1" && r.port == Some(pp)),
    );
    let s = BrowserSession::launch(opts).await.unwrap();
    let p = s.new_page(&format!("{}/sub", page.base())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        other.hits("/frame"),
        0,
        "cross-site iframe request reached the blocked origin"
    );
    assert_eq!(other.hits("/pixel.png"), 0);
    let blocked = p.blocked_requests();
    assert!(
        blocked
            .iter()
            .any(|b| b.url == format!("{other_origin}/frame")),
        "{blocked:?}"
    );
    s.shutdown().await;

    // Allowed: the same iframe loads, proving it is a real cross-site frame that interception saw.
    let opts = LaunchOptions::default().network(NetworkPolicy::default().allow_with(move |r| {
        (r.host == "127.0.0.1" && r.port == Some(pp))
            || (r.host == "localhost" && r.port == Some(op))
    }));
    let s = BrowserSession::launch(opts).await.unwrap();
    let p = s.new_page(&format!("{}/sub", page.base())).await.unwrap();
    p.wait_eval("window.frames.length===1", Duration::from_secs(5))
        .await
        .unwrap();
    for _ in 0..50 {
        if other.hits("/frame") > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(other.hits("/frame"), 1);
    let kind = p
        .eval(
            "(()=>{try{frames[0].document;return 'same-origin'}catch(e){return 'cross-origin'}})()",
        )
        .await
        .unwrap();
    assert_eq!(kind, "cross-origin");
    assert!(s
        .pinned_connections()
        .iter()
        .any(|c| c.host == "localhost" && c.port == op));
    s.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dns_answer_is_resolved_once_and_pinned() {
    let _serial = SERIAL.lock().await;
    let fx = Fixture::start(None, None);
    let port = fx.port;

    // Rebinding attempt: first answer is public, every later answer is loopback.
    // The vetted (first) answer must be the one connected to; loopback is never touched.
    let calls = Arc::new(Mutex::new(0usize));
    let c = calls.clone();
    // 203.0.113/24 is a documentation range: allowed here so the test stays offline.
    let opts = LaunchOptions::default().network(
        NetworkPolicy::default()
            .allow_class(AddressClass::Reserved)
            .with_resolver(move |host| {
                assert_eq!(host, "rebind.example");
                let mut n = c.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    vec!["203.0.113.9".parse().unwrap()]
                } else {
                    vec!["127.0.0.1".parse().unwrap()]
                }
            }),
    );
    let s = BrowserSession::launch(opts).await.unwrap();
    let _ = s
        .new_page(&format!("http://rebind.example:{port}/rebind"))
        .await;
    assert_eq!(
        fx.hits("/rebind"),
        0,
        "rebound loopback address was reached"
    );
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "name must be resolved once per request"
    );
    let conns = s.pinned_connections();
    assert!(
        !conns.is_empty() && conns.iter().all(|c| c.ip.to_string() == "203.0.113.9"),
        "{conns:?}"
    );
    s.shutdown().await;

    // A name that resolves private is refused outright, with no connection attempt.
    let opts = LaunchOptions::default()
        .network(NetworkPolicy::default().with_resolver(|_| vec!["10.1.2.3".parse().unwrap()]));
    let s = BrowserSession::launch(opts).await.unwrap();
    assert!(matches!(
        s.new_page("http://intranet.example/").await,
        Err(BrowserError::Blocked(ref d))
            if matches!(d.reason, BlockReason::Address { class: AddressClass::Private, .. })
    ));
    assert!(s.pinned_connections().is_empty());
    s.shutdown().await;

    // An explicitly allowed name that maps to loopback is served through the pinned address.
    let opts = LaunchOptions::default().network(
        NetworkPolicy::default()
            .with_resolver(|_| vec!["127.0.0.1".parse().unwrap()])
            .allow_with(move |r| r.host == "app.example" && r.port == Some(port)),
    );
    let s = BrowserSession::launch(opts).await.unwrap();
    let p = s
        .new_page(&format!("http://app.example:{port}/"))
        .await
        .unwrap();
    assert_eq!(p.title().await, "Home");
    assert_eq!(fx.hits("/"), 1);
    assert!(s
        .pinned_connections()
        .iter()
        .any(|c| c.host == "app.example" && c.ip.to_string() == "127.0.0.1"));
    s.shutdown().await;
}

/// Child half of `host_death_kills_chrome`: only acts when re-executed by it.
#[tokio::test(flavor = "multi_thread")]
async fn host_for_sigkill() {
    let _serial = SERIAL.lock().await;
    if std::env::var_os("RK_SIGKILL_HOST").is_none() {
        return;
    }
    let s = BrowserSession::launch(LaunchOptions::default())
        .await
        .unwrap();
    let _p = s.new_page("about:blank").await.unwrap();
    println!(
        "READY {} {}",
        s.profile_dir().display(),
        s.process_id().unwrap()
    );
    std::io::stdout().flush().unwrap();
    std::thread::sleep(Duration::from_secs(120));
}

#[tokio::test(flavor = "multi_thread")]
async fn host_death_kills_chrome_without_destructors() {
    let _serial = SERIAL.lock().await;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    let mut host = Command::new(std::env::current_exe().unwrap())
        .args([
            "host_for_sigkill",
            "--exact",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("RK_SIGKILL_HOST", "1")
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(host.stdout.take().unwrap()).lines();
    let ready = loop {
        let l = lines.next().expect("host exited before READY").unwrap();
        if let Some(i) = l.find("READY ") {
            break l[i + 6..].to_string();
        }
    };
    let (marker, chrome_pid) = ready.rsplit_once(' ').unwrap();
    let chrome_pid: u32 = chrome_pid.parse().unwrap();
    assert!(
        pids_for(marker).contains(&chrome_pid),
        "chrome should be running under the host"
    );
    assert!(pids_for(marker).len() >= 2 || !pids_for(marker).is_empty());

    host.kill().unwrap(); // SIGKILL: no destructors, no atexit.
    host.wait().unwrap();
    let killed = Instant::now();
    loop {
        if pids_for(marker).is_empty() {
            break;
        }
        assert!(
            killed.elapsed() < Duration::from_secs(3),
            "chrome survived host SIGKILL: {:?}",
            pids_for(marker)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// CodeRight's InlineHost owns a job object; Chromium and every helper must live in it and
/// die when the host terminates or closes it, with no breakaway.
#[cfg(windows)]
#[tokio::test(flavor = "multi_thread")]
async fn chromium_joins_the_callers_job_and_dies_when_the_caller_closes_it() {
    let _serial = SERIAL.lock().await;
    use std::ffi::c_void;
    use std::os::windows::io::{AsHandle, AsRawHandle, FromRawHandle, OwnedHandle};
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> *mut c_void;
        fn SetInformationJobObject(
            job: *mut c_void,
            class: u32,
            info: *const c_void,
            len: u32,
        ) -> i32;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn IsProcessInJob(process: *mut c_void, job: *mut c_void, result: *mut i32) -> i32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    // JOBOBJECT_EXTENDED_LIMIT_INFORMATION (144 bytes on x64); LimitFlags sits at byte 16.
    let job = unsafe {
        let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        assert!(!raw.is_null());
        let mut info = [0u64; 18];
        info[2] = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        assert_ne!(
            SetInformationJobObject(raw, 9, info.as_ptr().cast(), 144),
            0
        );
        OwnedHandle::from_raw_handle(raw)
    };
    let in_job = |pid: u32| unsafe {
        let process = OpenProcess(0x1000, 0, pid);
        if process.is_null() {
            return None; // already gone
        }
        let mut inside = 0;
        IsProcessInJob(process, job.as_raw_handle(), &mut inside);
        CloseHandle(process);
        Some(inside != 0)
    };

    let s = BrowserSession::launch(
        LaunchOptions::default()
            .windows_job(job.as_handle())
            .unwrap(),
    )
    .await
    .unwrap();
    let marker = s.profile_dir().to_string_lossy().to_string();
    let _p = s.new_page("about:blank").await.unwrap();
    let pids = pids_for(&marker);
    assert!(pids.len() >= 2, "browser plus helpers expected: {pids:?}");
    for pid in &pids {
        assert_ne!(
            in_job(*pid),
            Some(false),
            "chromium process {pid} is outside the caller's job"
        );
    }

    // The caller closes its job while the session is still alive: everything dies.
    drop(job);
    assert!(
        wait_gone(&marker).await.is_empty(),
        "chromium survived the caller closing its job"
    );
    drop(_p);
    drop(s);
}
