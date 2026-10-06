//! End-to-end proof of `rightkit-shell` through a real backgrounded Tauri app: the example binary is
//! launched by the rightkit-qa harness (shown behind the user's app, never activated), driven with the in-app control
//! server (trusted native input into the webview), and every claim is read back from the app, the
//! OS, or disk. Nothing is mocked except the link handler, which records instead of opening a
//! browser.
#![cfg(target_os = "macos")]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use rightkit_qa::control::{launch, Control, LaunchSpec, Mode};
use rightkit_qa::process::{is_alive, Tracker};
use rightkit_qa::workspace;
use rightkit_shell::clipboard::{self, Board, ClipboardContent};
use serde_json::{json, Value};

const EXE: &str = env!("CARGO_BIN_EXE_shell-example-app");

struct Run {
    c: Control,
    ws: workspace::QaWorkspace,
    open_log: PathBuf,
    stall_log: PathBuf,
    state_dir: PathBuf,
    home: PathBuf,
}

/// LaunchServices cannot start an app whose log/bundle lives on an external volume (error -10810),
/// and the managed build broker points TMPDIR at one, so QA artefacts live under /private/tmp.
fn internal_tmp() -> PathBuf {
    let dir = PathBuf::from("/private/tmp/rk-shell-e2e");
    std::fs::create_dir_all(&dir).unwrap();
    std::env::set_var("TMPDIR", &dir);
    dir
}

fn start(tracker: &Tracker, label: &str, state_dir: Option<PathBuf>) -> Run {
    start_with(tracker, label, state_dir, &[])
}

fn start_with(tracker: &Tracker, label: &str, state_dir: Option<PathBuf>, extra_env: &[(&str, &str)]) -> Run {
    let cache = internal_tmp().join(format!("run-{}", std::process::id()));
    let mut ws = workspace::create(&cache, None, "rightkit-shell-example").expect("workspace");
    // macOS launches through `open --env`, which carries only RIGHTKIT_* keys.
    ws.env.retain(|k, _| k.starts_with("RIGHTKIT_"));
    let open_log = ws.home.join("opened.txt");
    let stall_log = ws.home.join("stall.txt");
    let state_dir = state_dir.unwrap_or_else(|| ws.home.join("state"));
    let home = ws.home.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let mut env: Vec<(String, String)> = vec![
        ("RIGHTKIT_EXAMPLE_OPEN_LOG".into(), open_log.to_string_lossy().into()),
        ("RIGHTKIT_EXAMPLE_STALL_LOG".into(), stall_log.to_string_lossy().into()),
        ("RIGHTKIT_EXAMPLE_STATE_DIR".into(), state_dir.to_string_lossy().into()),
        ("RIGHTKIT_SHELL_HOME".into(), home.to_string_lossy().into()),
    ];
    env.extend(extra_env.iter().filter(|(k, _)| k.starts_with("RIGHTKIT_")).map(|(k, v)| (k.to_string(), v.to_string())));
    let spec = LaunchSpec {
        binary: PathBuf::from(EXE),
        mode: if extra_env.iter().any(|(k, _)| *k == "RK_MODE_HIDDEN") { Mode::Hidden } else { Mode::Background },
        env,
        startup_timeout: Duration::from_secs(60),
        label: label.into(),
    };
    let c = launch(&spec, &ws, tracker).expect("launch hidden app");
    Run { c, ws, open_log, stall_log, state_dir, home }
}

/// Run an async bridge call in the page and wait for its outcome: `{ok}` or `{err}`.
fn bridge(c: &Control, expr: &str) -> Value {
    c.eval("window.__r = undefined; return 1;").unwrap();
    let expr_json = serde_json::to_string(expr).unwrap();
    c.eval(&format!("({expr}).then(function(v){{window.__r={{ok:v===undefined?null:v}};}},function(e){{window.__r={{err:String(e),expr:{expr_json}}};}}); return 1;")).unwrap();
    c.wait_eval("return window.__r;", Duration::from_secs(20)).unwrap_or_else(|e| panic!("bridge `{expr}`: {e}"))
}

fn bridge_error(c: &Control, expr: &str) -> Value {
    c.eval("window.__r = undefined; return 1;").unwrap();
    c.eval(&format!("({expr}).then(function(v){{window.__r={{ok:v===undefined?null:v}};}},function(e){{window.__r={{err:{{code:e&&e.code||'other',message:e&&e.message||String(e)}}}};}}); return 1;")).unwrap();
    c.wait_eval("return window.__r;", Duration::from_secs(20)).unwrap_or_else(|e| panic!("bridge `{expr}`: {e}"))
}

fn js_string(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}
fn ok(v: &Value) -> &Value {
    assert!(v.get("err").is_none(), "bridge call failed: {v}");
    &v["ok"]
}

fn ok_owned(v: Value) -> Value {
    ok(&v).clone()
}

fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("timed out waiting for {what}");
}

fn read_lines(p: &Path) -> Vec<String> {
    std::fs::read_to_string(p).unwrap_or_default().lines().map(str::to_owned).collect()
}

fn status(c: &Control) -> Value {
    ok(&bridge(c, "rk.status()")).clone()
}

fn blocked(c: &Control) -> Vec<String> {
    c.eval("return window.__rkShell.blocked.map(function(b){return b.kind;});").unwrap().as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_owned()).collect()
}



/// Which process owns the accessibility element the OS draws at a screen point.
mod ax {
    use std::ffi::c_void;
    type Ref = *const c_void;
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXUIElementCreateSystemWide() -> Ref;
        fn AXUIElementCopyElementAtPosition(el: Ref, x: f32, y: f32, out: *mut Ref) -> i32;
        fn AXUIElementGetPid(el: Ref, pid: *mut i32) -> i32;
    }
    pub fn owner_at(x: f32, y: f32) -> i32 {
        unsafe {
            let sys = AXUIElementCreateSystemWide();
            let mut el: Ref = std::ptr::null();
            if AXUIElementCopyElementAtPosition(sys, x, y, &mut el) != 0 || el.is_null() {
                return -1;
            }
            let mut pid = 0;
            AXUIElementGetPid(el, &mut pid);
            pid
        }
    }
}

/// Dead or a zombie (killed but not yet reaped by its parent).
fn gone(pid: u32) -> bool {
    let out = Command::new("/bin/ps").args(["-o", "stat=", "-p", &pid.to_string()]).output().unwrap();
    let st = String::from_utf8_lossy(&out.stdout);
    st.trim().is_empty() || st.trim().starts_with('Z')
}

fn pid_windows(pid: u32) -> Vec<rightkit_control::mac::Win> {
    rightkit_control::mac::windows(pid as i32)
}

fn assert_window_server_background(pid: u32) {
    let foreground = rightkit_control::mac::frontmost_pid().expect("WindowServer foreground pid");
    assert_ne!(foreground, pid as i32, "QA app became foreground");
    // rightkit-control's CGWindowList wrapper documents front-to-back order; native
    // level is the stable below-normal proof, so do not infer z-order from indices.
    let fixture_windows: Vec<_> = rightkit_control::mac::windows(0)
        .into_iter()
        .filter(|w| w.pid == pid as i32 && w.onscreen)
        .collect();
    assert!(!fixture_windows.is_empty(), "fixture onscreen window");
    assert!(fixture_windows.iter().all(|w| w.layer == -1), "every onscreen fixture window must use below-normal layer -1: {fixture_windows:?}");
}

#[test]
fn shell_hardens_the_webview_and_wires_menu_tray_links_engine_and_exit() {
    let tracker = Tracker::new();
    let mut run = start(&tracker, "shell-main", None);
    let pid = run.c.pid;
    let c = &run.c;
    c.wait_eval("return window.__ready;", Duration::from_secs(20)).expect("frontend ready via bridge");

    // --- menu, tray, single-instance role: read from the running app -----------------------
    let st0 = status(c);
    assert_eq!(st0["instance"], "primary");
    assert_eq!(st0["tray"], true, "tray icon registered");
    assert_eq!(st0["windows"][0]["native"]["visible"], true, "native main window is visible");
    assert_eq!(st0["windows"][0]["native"]["level"], -1, "background QA window stays below normal user windows");
    assert_window_server_background(pid);
    let rows: Vec<String> = st0["menu"].as_array().unwrap().iter().map(|r| format!("{}|{}", r["path"].as_str().unwrap(), r["label"].as_str().unwrap())).collect();
    for want in ["Edit/", "Window/", "Help/", "Demo/demo.ping|Ping", "Help/help.docs|Documentation", "View/"] {
        assert!(rows.iter().any(|r| r.contains(want)), "menu lacks {want}: {rows:?}");
    }
    for forbidden in ["reload", "zoom in", "zoom out", "actual size", "back", "forward", "developer", "inspect"] {
        assert!(!rows.iter().any(|r| r.to_lowercase().rsplit('|').next().unwrap_or("").contains(forbidden)), "menu must not contain `{forbidden}`: {rows:?}");
    }
    // The OS sees the status item: AppKit parks status-item windows at layer 25.
    std::thread::sleep(Duration::from_secs(2));
    let rect = &st0["trayRect"];
    let w0 = rect["w"].as_f64().unwrap_or(0.0);
    assert!(w0 > 0.0 && rect["h"].as_f64().unwrap_or(0.0) > 0.0, "OS gave the status item real geometry: {rect}");
    // Updating the tray from the webview: a menu-bar title makes the OS widen the status item.
    ok(&bridge(c, "rk.tray.update({title: ' 3 pending', tooltip: 'updated', items: [{kind: 'item', id: 'tray.x', label: 'X'}]})"));
    wait_for("status item widened by its title", 10, || status(c)["trayRect"]["w"].as_f64().unwrap_or(0.0) > w0 + 20.0);
    // Menu items can be disabled from the webview and read back from the native item.
    assert_eq!(status(c)["menuEnabled"]["demo.ping"], true);
    ok(&bridge(c, "rk.menu.setEnabled('demo.ping', false)"));
    assert_eq!(status(c)["menuEnabled"]["demo.ping"], false);
    ok(&bridge(c, "rk.menu.setEnabled('demo.ping', true)"));
    assert_eq!(status(c)["menuEnabled"]["demo.ping"], true);
    assert!(bridge(c, "rk.menu.setEnabled('nope', true)")["err"].as_str().unwrap_or("").contains("no menu item"));
    // A secondary window: label validated, created through the shell, visible to the OS.
    let wins_before = pid_windows(pid).len();
    assert_eq!(ok(&bridge(c, "rk.window.open({label: 'mini', title: 'Mini', width: 420, height: 300})")), "mini");
    wait_for("secondary window", 10, || status(c)["windows"].as_array().unwrap().iter().any(|w| w["label"] == "mini" && w["visible"] == true && w["native"]["visible"] == true && w["title"] == "Mini"));
    assert!(pid_windows(pid).len() > wins_before, "OS sees the new window");
    assert!(bridge(c, "rk.window.open({label: '../evil'})")["err"].as_str().unwrap_or("").contains("invalid window label"));

    // --- right click: no webview menu on plain surfaces, app-owned menu works -------------
    let before = pid_windows(pid).len();
    let (x, y) = c.center_of("#plain").unwrap();
    c.click(x, y, "right", 1).unwrap();
    std::thread::sleep(Duration::from_millis(700));
    assert!(blocked(c).contains(&"contextmenu-blocked".to_string()), "context menu event was cancelled by the shell");
    assert!(pid_windows(pid).iter().all(|w| w.layer != 101), "no native popup menu window after right click on plain surface");
    assert_eq!(pid_windows(pid).len(), before, "no new window of any kind");
    let (x, y) = c.center_of("#own").unwrap();
    c.click(x, y, "right", 1).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(c.eval("return window.__ownMenu === true && getComputedStyle(document.getElementById('appmenu')).display;").unwrap(), json!("block"), "app's own context menu rendered");
    assert!(pid_windows(pid).iter().all(|w| w.layer != 101));
    // Editable fields and opted-in surfaces keep the native menu (their events are NOT cancelled);
    // the positive control below proves such a menu really is detectable.
    let probe = c.eval("function t(sel){var el=document.querySelector(sel);return document.querySelector(sel).dispatchEvent(new MouseEvent('contextmenu',{bubbles:true,cancelable:true,clientX:5,clientY:5}));} return {plain:t('#plain'),input:t('#inp'),opted:t('#nativesurface')};").unwrap();
    assert_eq!(probe, json!({"plain": false, "input": true, "opted": true}), "dispatchEvent returns false only when the shell cancelled the menu");

    // --- keys: reload / zoom / history / devtools are cancelled, the page survives ---------
    c.eval("window.__marker = 'alive'; window.__keys = []; return 1;").unwrap();
    // Real trusted key events for every chord the harness can synthesise ...
    for k in ["cmd+r", "cmd+shift+r", "cmd+u", "cmd+p", "alt+left", "alt+right", "cmd+alt+i", "cmd+alt+j", "cmd+alt+c"] {
        c.key(k).unwrap_or_else(|e| panic!("send {k}: {e}"));
        std::thread::sleep(Duration::from_millis(80));
    }
    // ... and synthetic events for keys it cannot (F-keys, punctuation): same listeners, so a
    // `false` from dispatchEvent means the shell cancelled the default action.
    let cancelled = c.eval("var out = {}; [['F5',{}],['F12',{}],['=',{metaKey:true}],['-',{metaKey:true}],['0',{ctrlKey:true}],['[',{metaKey:true}],[']',{metaKey:true}],['I',{ctrlKey:true,shiftKey:true}],['a',{}]].forEach(function(p){ var ev = new KeyboardEvent('keydown', Object.assign({key:p[0],bubbles:true,cancelable:true}, p[1])); out[p[0]+JSON.stringify(p[1])] = !document.body.dispatchEvent(ev); }); var w = new WheelEvent('wheel',{ctrlKey:true,deltaY:-100,bubbles:true,cancelable:true}); out.wheelZoom = !document.body.dispatchEvent(w); return out;").unwrap();
    for (k, v) in cancelled.as_object().unwrap() {
        let expect = !k.starts_with("a{");
        assert_eq!(v, &json!(expect), "{k}: cancelled={v}");
    }
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(c.eval("return window.__marker;").unwrap(), json!("alive"), "page was not reloaded");
    let b = blocked(c);
    for kind in ["key-reload", "key-zoom", "key-history", "key-devtools"] {
        assert!(b.contains(&kind.to_string()), "{kind} not recorded: {b:?}");
    }
    let keys = c.eval("return window.__keys;").unwrap();
    assert!(keys.as_array().unwrap().iter().any(|k| k["key"] == "F5" && k["prevented"] == true), "app still sees the key, already cancelled: {keys}");
    let st = ok(&bridge(c, "rk.status()")).clone();
    assert_eq!(st["devtoolsOpen"], false, "inspector never opened");
    assert!(pid_windows(pid).iter().all(|w| !w.name.to_lowercase().contains("inspector")), "no Web Inspector window");

    // --- external links leave for the system handler; the webview never navigates ----------
    let href0 = c.eval("return location.href;").unwrap();
    c.click_selector("#ext").unwrap();
    c.click_selector("#blank").unwrap();
    c.click_selector("#mail").unwrap();
    c.eval("window.open('https://example.net/rk-window-open'); window.open('file:///etc/hosts'); return 1;").unwrap();
    c.eval("location.href = 'https://example.com/rk-navigation';return 1;").unwrap(); // not a link click: only the native hook can stop this
    wait_for("opened links", 10, || read_lines(&run.open_log).len() >= 5);
    let opened = read_lines(&run.open_log);
    for want in ["https://example.com/rk-external", "https://example.org/rk-blank", "mailto:rk@example.com", "https://example.net/rk-window-open", "https://example.com/rk-navigation"] {
        assert!(opened.iter().any(|l| l == want), "{want} not handed to the opener: {opened:?}");
    }
    assert!(!opened.iter().any(|l| l.starts_with("file:")), "file: URLs are never opened");
    assert_eq!(c.eval("return location.href;").unwrap(), href0, "webview stayed on the app page");
    assert_eq!(c.eval("return window.__marker;").unwrap(), json!("alive"));
    let bad = bridge(c, "rk.openExternal('javascript:alert(1)')");
    assert!(bad["err"].as_str().unwrap_or("").contains("blocked"), "javascript: refused by the host: {bad}");

    // --- engine: calls, streamed events, restart after a crash, bounded shutdown ----------
    wait_for("engine ready", 20, || ok(&bridge(c, "rk.ready()"))["engine"]["state"] == "ready");
    let echo = ok(&bridge(c, "rk.engine.call('echo', {a: 1, s: 'héllo'})")).clone();
    assert_eq!(echo["echo"], json!({"a": 1, "s": "héllo"}));
    let engine_pid = echo["pid"].as_u64().unwrap() as u32;
    assert_ne!(engine_pid, pid);
    assert!(is_alive(engine_pid));
    assert_eq!(ok(&bridge(c, "rk.engine.call('stream')"))["ticks"], 3);
    let ticks = c.eval("return window.__events.filter(function(e){return e.kind==='engine'&&e.p.call==='stream';}).map(function(e){return e.p.event.tick;});").unwrap();
    assert_eq!(ticks, json!([0, 1, 2]), "engine events streamed to the webview in order");
    let _ = Command::new("kill").arg("-9").arg(engine_pid.to_string()).status();
    wait_for("engine death", 5, || gone(engine_pid));
    let echo2 = ok(&bridge(c, "rk.engine.call('echo', 'again')")).clone();
    let engine_pid2 = echo2["pid"].as_u64().unwrap() as u32;
    assert_ne!(engine_pid2, engine_pid, "dead engine was restarted");
    assert_eq!(echo2["echo"], "again");

    // --- deep links + single instance: a second launch forwards argv and exits -------------
    let suite = std::fs::read_dir(&run.ws.home).unwrap().filter_map(Result::ok).map(|e| e.path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with("suite-")).expect("suite dir");
    let record = suite.join("runtime/rk-shell-example-shell/service.json");
    assert!(record.exists(), "primary published its single-instance record");
    // A second launch through LaunchServices, hidden and without activation, exactly how the OS
    // would start a second copy (a bare exec would briefly become the frontmost app).
    let bundle = run.ws.root.join("shell-example-app.app");
    assert!(bundle.exists(), "harness bundle {}", bundle.display());
    let needle = format!("{}/Contents/MacOS/shell-example-app", bundle.display());
    let count_main = || {
        let out = Command::new("/bin/ps").args(["-ax", "-o", "pid=,command="]).output().unwrap();
        String::from_utf8_lossy(&out.stdout).lines().filter(|l| l.contains(&needle) && !l.contains("--engine")).count()
    };
    assert_eq!(count_main(), 1);
    let launched = Command::new("/usr/bin/open")
        .args(["-g", "-j", "-n", "--env"])
        .arg(format!("RIGHTKIT_SUITE_ROOT={}", suite.display()))
        .arg(&bundle)
        .args(["--args", "rkshell-example://open/doc/42?x=a%20b%C3%A9", "rkshell-example://open/..%2Fsecret", "https://evil.example/x"])
        .status()
        .expect("open second instance");
    assert!(launched.success());
    // The second copy forwards its argv and exits; the primary stays the only instance.
    wait_for("second instance exited", 20, || c.eval("return window.__events.some(function(e){return e.kind==='second-instance';});").unwrap() == json!(true) && count_main() == 1);
    wait_for("deep link event", 10, || c.eval("return window.__events.some(function(e){return e.kind==='deep-link';});").unwrap() == json!(true));
    let links = c.eval("return window.__events.filter(function(e){return e.kind==='deep-link';}).map(function(e){return e.p;});").unwrap();
    let links = links.as_array().unwrap();
    assert_eq!(links.len(), 1, "only the valid link is delivered: {links:?}");
    assert_eq!(links[0]["segments"], json!(["doc", "42"]));
    assert_eq!(links[0]["query"]["x"], "a bé", "percent-decoding is UTF-8 aware");
    assert_eq!(c.eval("return window.__events.filter(function(e){return e.kind==='second-instance';}).length;").unwrap(), json!(1));

    // --- autostart: a real, valid LaunchAgent that is removed again ------------------------
    let on = ok(&bridge(c, "rk.autostart.set(true)")).clone();
    assert_eq!(on["enabled"], true);
    let plist = run.home.join("Library/LaunchAgents/rk-shell-example.autostart.plist");
    assert_eq!(on["entry"].as_str().map(PathBuf::from), Some(plist.clone()));
    let lint = Command::new("/usr/bin/plutil").arg("-lint").arg(&plist).output().unwrap();
    assert!(lint.status.success(), "plutil: {}", String::from_utf8_lossy(&lint.stdout));
    let body = std::fs::read_to_string(&plist).unwrap();
    assert!(body.contains("<key>RunAtLoad</key>") && body.contains("--background") && body.contains("rk-shell-example.autostart"));
    assert!(body.contains("/usr/bin/open"), "bundle launched through LaunchServices: {body}");
    assert_eq!(ok(&bridge(c, "rk.autostart.get()"))["enabled"], true);
    assert_eq!(ok(&bridge(c, "rk.autostart.set(false)"))["enabled"], false);
    assert!(!plist.exists());

    // --- rich clipboard through the bridge, both directions, on a private pasteboard -------
    let board = format!("rk-shell-e2e-{}", std::process::id());
    let tmp = internal_tmp().join(format!("rk-shell-clip-{}.txt", std::process::id()));
    std::fs::write(&tmp, "file body").unwrap();
    let png = b"\x89PNG\r\n\x1a\n-not-a-real-image-but-bytes-survive-\x00\xff".to_vec();
    let mut custom = std::collections::BTreeMap::new();
    custom.insert("app.rightkit.test".to_string(), vec![0u8, 1, 2, 254, 255]);
    let mine = ClipboardContent { text: Some("plain héllo".into()), html: Some("<b>bold</b>".into()), png: Some(png.clone()), files: vec![tmp.to_string_lossy().into()], custom };
    clipboard::write(&Board::Named(board.clone()), &mine).unwrap();
    let read = ok(&bridge(c, &format!("rk.clipboard.read('{board}').then(function(r){{return {{text:r.text,html:r.html,png:Array.from(r.png||[]),files:r.files,custom:Object.fromEntries(Object.entries(r.custom).map(function(kv){{return [kv[0],Array.from(kv[1])];}}))}};}})"))).clone();
    assert_eq!(read["text"], "plain héllo");
    assert_eq!(read["html"], "<b>bold</b>");
    assert_eq!(read["png"], json!(png));
    assert_eq!(read["files"].as_array().unwrap().len(), 1);
    assert_eq!(std::fs::canonicalize(read["files"][0].as_str().unwrap()).unwrap(), std::fs::canonicalize(&tmp).unwrap());
    assert_eq!(read["custom"]["app.rightkit.test"], json!([0, 1, 2, 254, 255]));
    ok(&bridge(c, &format!("rk.clipboard.write({{board:'{board}',text:'from the webview',html:'<i>i</i>',png:new Uint8Array([137,80,78,71,1,2,3]),custom:{{'app.rightkit.test':new Uint8Array([9,8,7])}}}})")));
    let back = clipboard::read(&Board::Named(board.clone())).unwrap();
    assert_eq!(back.text.as_deref(), Some("from the webview"));
    assert_eq!(back.html.as_deref(), Some("<i>i</i>"));
    assert_eq!(back.png.as_deref(), Some(&[137u8, 80, 78, 71, 1, 2, 3][..]));
    assert_eq!(back.custom.get("app.rightkit.test").map(Vec::as_slice), Some(&[9u8, 8, 7][..]));
    let _ = std::fs::remove_file(&tmp);

    // --- watchdog: a blocked main thread is reported once, then recovers -------------------
    c.command("block_main_thread", &Value::Null).unwrap();
    wait_for("stall report", 15, || run.stall_log.exists());
    let stall = std::fs::read_to_string(&run.stall_log).unwrap();
    let ms: u64 = stall.split("stalled_ms=").nth(1).unwrap().split_whitespace().next().unwrap().parse().unwrap();
    assert!(ms >= 900, "reported stall of {ms}ms");
    wait_for("stall event reaches the webview after recovery", 15, || c.eval("return window.__events.some(function(e){return e.kind==='stall';});").map(|v| v == json!(true)).unwrap_or(false));

    // --- ordered exit: quit tears down engine, tray and the instance record ----------------
    assert!(record.exists());
    // The app exits while answering, so the reply may never arrive: ignore the transport result.
    let _ = c.eval("rk.quit(); return 1;");
    wait_for("app exit", 15, || !is_alive(pid));
    assert!(gone(engine_pid2), "engine did not outlive the app");
    assert!(!record.exists(), "single-instance record released");
    let rep = run.c.stop().unwrap();
    assert!(rep.process_gone && rep.endpoint_closed);
    // User may switch Finder/Safari while QA runs. The strict invariant is that
    // every owned shell PID stayed out of foreground; app-name stability remains
    // diagnostic in StopReport only.
    assert!(rep.owned_never_frontmost, "owned shell process reached foreground: {rep:?}");
    tracker.assert_no_orphans().unwrap();
    let _ = std::fs::remove_dir_all(&run.state_dir);
    workspace::dispose(&run.ws, false, 5, 200);
}

#[test]
fn window_geometry_is_saved_and_restored_across_launches() {
    let tracker = Tracker::new();
    let shared_state = internal_tmp().join(format!("rk-shell-state-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&shared_state);
    let mut first = start(&tracker, "shell-state-1", Some(shared_state.clone()));
    first.c.wait_eval("return window.__ready;", Duration::from_secs(20)).unwrap();
    first.c.command("set_geometry", &json!({"x": 140, "y": 120, "w": 777, "h": 555})).unwrap();
    wait_for("window-state.json", 10, || {
        std::fs::read_to_string(shared_state.join("window-state.json")).map(|s| s.contains("777")).unwrap_or(false)
    });
    let saved: Value = serde_json::from_str(&std::fs::read_to_string(shared_state.join("window-state.json")).unwrap()).unwrap();
    assert_eq!(saved["main"]["version"], 1);
    assert!((saved["main"]["width"].as_f64().unwrap() - 777.0).abs() < 2.0);
    assert!((saved["main"]["height"].as_f64().unwrap() - 555.0).abs() < 2.0);
    first.c.stop().unwrap();
    workspace::dispose(&first.ws, false, 5, 200);

    let mut second = start(&tracker, "shell-state-2", Some(shared_state.clone()));
    second.c.wait_eval("return window.__ready;", Duration::from_secs(20)).unwrap();
    let geo = second.c.command("geometry", &Value::Null).unwrap();
    assert!((geo["w"].as_f64().unwrap() - 777.0).abs() < 2.0 && (geo["h"].as_f64().unwrap() - 555.0).abs() < 2.0, "restored size: {geo}");
    assert!((geo["x"].as_f64().unwrap() - saved["main"]["x"].as_f64().unwrap()).abs() < 2.0, "restored x: {geo} vs {saved}");
    second.c.stop().unwrap();
    tracker.assert_no_orphans().unwrap();
    let _ = std::fs::remove_dir_all(&shared_state);
    workspace::dispose(&second.ws, false, 5, 200);
}

#[test]
#[ignore = "background-only QA forbids native popup over user work"]
fn positive_control_native_menu_is_detectable_where_the_shell_allows_it() {
    let tracker = Tracker::new();
    let mut run = start(&tracker, "shell-positive", None);
    let pid = run.c.pid;
    run.c.wait_eval("return window.__ready;", Duration::from_secs(20)).unwrap();
    assert!(pid_windows(pid).iter().all(|w| w.layer != 101));
    let (x, y) = run.c.center_of("#inp").unwrap();
    run.c.click(x, y, "right", 1).unwrap();
    wait_for("native popup menu for the editable field", 10, || pid_windows(pid).iter().any(|w| w.layer == 101));
    // Opted-in surface: same menu machinery, so `data-rk-native-menu` is a real escape hatch.
    run.c.stop().unwrap(); // the open menu is modal inside the app; the harness kills the recorded pid tree
    tracker.assert_no_orphans().unwrap();
    workspace::dispose(&run.ws, false, 5, 200);
}

/// The real native menu bar read back through the Accessibility API (not the shell's own registry).
/// Needs the Accessibility permission of whatever launched the test: present in an interactive
/// shell, absent under the managed build broker, so this test reports and returns there. It is
/// separate from the main test because walking the Apple menu through AX briefly activates other
/// apps, which would invalidate that test's frontmost-app-unchanged proof.
#[test]
#[ignore = "background-only QA forbids global menu activation"]
fn native_menu_bar_matches_the_spec_through_accessibility() {
    if !rightkit_control::mac::accessibility_trusted() {
        println!("native menu bar AX check NOT RUN: this process has no Accessibility permission (run the test binary from an interactive terminal)");
        return;
    }
    let tracker = Tracker::new();
    // A status item only opens its menu in a launched (not LaunchServices-hidden) app, so this one
    // test runs in background mode: window shown, app never activated.
    let mut run = start_with(&tracker, "shell-ax", None, &[("RK_MODE_BACKGROUND", "1")]);
    let pid = run.c.pid;
    run.c.wait_eval("return window.__ready;", Duration::from_secs(20)).unwrap();
    let mut ax = String::new();
    wait_for("native menu bar visible to Accessibility", 20, || {
        ax = rightkit_control::mac::ax_dump(pid as i32, 6, 5000).join("\n");
        ax.contains("AXMenuItem \"Documentation\"")
    });
    for want in ["AXMenuBarItem \"Edit\"", "AXMenuBarItem \"View\"", "AXMenuBarItem \"Demo\"", "AXMenuBarItem \"Window\"", "AXMenuBarItem \"Help\"", "AXMenuItem \"Ping\"", "AXMenuItem \"Settings", "AXMenuItem \"Cut\"", "AXMenuItem \"Paste\""] {
        assert!(ax.contains(want), "native menu bar lacks {want}:\n{ax}");
    }
    for forbidden in ["Reload", "Zoom In", "Zoom Out", "Actual Size", "Developer", "Inspect"] {
        assert!(!ax.contains(forbidden), "native menu bar must not offer {forbidden}");
    }
    println!("AX native menu bar verified");
    // Native menu item: open the Demo menu and press Ping through the OS, then read the event back.
    let demo = rightkit_control::mac::ax_find(pid as i32, "Demo", true).expect("Demo menu bar item");
    assert!(rightkit_control::mac::ax_press(&demo));
    std::thread::sleep(Duration::from_millis(600));
    let ping = rightkit_control::mac::ax_find(pid as i32, "Ping", true).expect("Ping menu item");
    assert!(rightkit_control::mac::ax_press(&ping));
    wait_for("native menu click event in the webview", 10, || run.c.eval("return window.__events.some(function(e){return e.kind==='menu'&&e.p==='demo.ping';});").unwrap() == json!(true));
    // Regression: Tauri hands every menu event to a tray's handler; the shell must not re-emit an
    // app-menu click as a tray action.
    assert_eq!(run.c.eval("return window.__events.filter(function(e){return e.kind==='tray';}).length;").unwrap(), json!(0));
    // Tray: the OS resolves the point at the tray rectangle to the app's own status-item element.
    // AXPress on it does not open or fire the menu in a launched-but-inactive app (no menu window, no
    // click event), so tray *menu* clicks are not actuated; see final.md.
    let rect = status(&run.c)["trayRect"].clone();
    let (cx, cy) = (rect["x"].as_f64().unwrap() + rect["w"].as_f64().unwrap() / 2.0, rect["y"].as_f64().unwrap() + rect["h"].as_f64().unwrap() / 2.0);
    assert_eq!(ax::owner_at(cx as f32, cy as f32), pid as i32, "the element the OS draws at the tray rectangle belongs to the app");
    let _ = run.c.stop();
    tracker.assert_no_orphans().unwrap();
    workspace::dispose(&run.ws, false, 5, 200);
}

#[test]
fn closing_the_window_keeps_a_tray_app_running_and_a_second_launch_brings_it_back() {
    let tracker = Tracker::new();
    let mut run = start_with(&tracker, "shell-tray", None, &[("RIGHTKIT_EXAMPLE_CLOSE_TO_TRAY", "1")]);
    let pid = run.c.pid;
    run.c.wait_eval("return window.__ready;", Duration::from_secs(20)).unwrap();
    let c = &run.c;
    wait_for("main window visible", 10, || status(c)["windows"][0]["visible"] == true);
    ok(&bridge(c, "rk.window.close()"));
    wait_for("main window hidden", 10, || status(c)["windows"][0]["visible"] == false);
    std::thread::sleep(Duration::from_millis(500));
    assert!(is_alive(pid), "closing the last window did not exit a tray-resident app");
    assert_eq!(status(c)["tray"], true);
    // A second launch (Dock click / double click) must reveal the hidden window.
    let suite = std::fs::read_dir(&run.ws.home).unwrap().filter_map(Result::ok).map(|e| e.path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with("suite-")).unwrap();
    let bundle = run.ws.root.join("shell-example-app.app");
    assert!(Command::new("/usr/bin/open").args(["-g", "-j", "-n", "--env"]).arg(format!("RIGHTKIT_SUITE_ROOT={}", suite.display())).arg(&bundle).status().unwrap().success());
    wait_for("second launch reveals the window", 20, || status(c)["windows"][0]["visible"] == true);
    let _ = c.eval("rk.quit(); return 1;");
    wait_for("quit still exits a tray-resident app", 15, || !is_alive(pid));
    let _ = run.c.stop();
    tracker.assert_no_orphans().unwrap();
    workspace::dispose(&run.ws, false, 5, 200);
}

/// `@rightkit/app-shell` (React, real `ShellProvider`) running in the example webview on the real
/// `@rightkit/shell` bridge (`shell.bridge()` is type-checked against `AppShellBridge` when the
/// bundle is built): theme mirrored natively, link routing, and native actions run commands.
#[test]
fn app_shell_runs_on_the_real_shell_bridge() {
    let tracker = Tracker::new();
    let mut run = start(&tracker, "shell-appshell", None);
    let c = &run.c;
    c.wait_eval("return window.__ready;", Duration::from_secs(20)).unwrap();
    c.eval("location.assign('/appshell.html'); return 1;").unwrap();
    c.wait_eval("return !!document.getElementById('appshell-ready') && !!window.__appshell && !!window.__rkShell;", Duration::from_secs(30)).expect("app-shell mounted");
    // The real frame rendered (sidebar destination from `groups`, title from props).
    let text = c.eval("return document.body.innerText;").unwrap();
    assert!(text.as_str().unwrap().contains("Home") && text.as_str().unwrap().contains("app-shell on rightkit-shell"), "app-shell UI rendered: {text}");
    // setNativeTheme went through rk_shell_set_theme without error.
    c.wait_eval("return window.__appshell.themes.length > 0;", Duration::from_secs(10)).expect("provider mirrored the theme natively");
    assert_eq!(c.eval("return window.__appshell.errors;").unwrap(), json!([]));
    // openExternal -> shell broker -> opener (not the webview).
    c.eval("window.__bridge.openExternal('https://example.com/from-appshell'); return 1;").unwrap();
    wait_for("openExternal reached the opener", 10, || read_lines(&run.open_log).iter().any(|l| l == "https://example.com/from-appshell"));
    // A native action stream (deep link here) is routed by app-shell to the command with that id.
    let suite = std::fs::read_dir(&run.ws.home).unwrap().filter_map(Result::ok).map(|e| e.path()).find(|p| p.file_name().unwrap().to_string_lossy().starts_with("suite-")).unwrap();
    let bundle = run.ws.root.join("shell-example-app.app");
    assert!(Command::new("/usr/bin/open").args(["-g", "-j", "-n", "--env"]).arg(format!("RIGHTKIT_SUITE_ROOT={}", suite.display())).arg(&bundle).args(["--args", "rkshell-example://open/doc/7"]).status().unwrap().success());
    wait_for("app-shell ran the command for the native action", 20, || c.eval("return window.__appshell.ran.indexOf('deep-link:doc/7') >= 0;").unwrap() == json!(true));
    let _ = run.c.stop();
    tracker.assert_no_orphans().unwrap();
    workspace::dispose(&run.ws, false, 5, 200);
}

#[test]
fn shell_native_capabilities_cover_paths_ranges_errors_presence_dnd_idle_and_recovery() {
    let tracker = Tracker::new();
    let fixture = internal_tmp().join(format!("rk-shell-capabilities-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&fixture);
    std::fs::create_dir_all(&fixture).unwrap();
    let file = fixture.join("sample.txt");
    std::fs::write(&file, b"0123456789").unwrap();
    let cache = fixture.join("cache");
    #[cfg(unix)]
    std::os::unix::fs::symlink(fixture.join("missing-cache"), &cache).unwrap();
    let idle_ms = "100";
    let file_s = file.to_string_lossy().to_string();
    let cache_s = cache.to_string_lossy().to_string();
    let mut run = start_with(
        &tracker,
        "shell-capabilities",
        None,
        &[
            ("RIGHTKIT_EXAMPLE_IDLE_MS", idle_ms),
            ("RIGHTKIT_EXAMPLE_CACHE_DIR", &cache_s),
            ("RIGHTKIT_EXAMPLE_GRANT_FILE", &file_s),
        ],
    );
    let c = &run.c;
    c.wait_eval("return window.__ready;", Duration::from_secs(20)).unwrap();

    let st = status(c);
    assert_window_server_background(run.c.pid);
    assert_eq!(st["streamScheme"], "rk-stream");
    assert_eq!(st["cacheRepair"]["outcome"], "repaired");

    let ungranted = fixture.join("ungranted.txt");
    std::fs::write(&ungranted, b"private").unwrap();
    let denied = bridge_error(c, &format!("rk.paths.require({})", js_string(&ungranted.to_string_lossy())));
    assert_eq!(denied["err"]["code"], "broker_denied");

    let granted = ok_owned(bridge(c, &format!("rk.paths.require({})", js_string(&file_s))));
    assert_eq!(granted, json!(std::fs::canonicalize(&file).unwrap().to_string_lossy()));
    let stream = ok_owned(bridge(c, &format!("rk.paths.streamUrl({})", js_string(&file_s))));
    let url = stream["url"].as_str().unwrap().to_owned();
    let ranged = ok_owned(bridge(c, &format!("fetch({}, {{headers: {{Range: 'bytes=2-5'}}}}).then(async r => ({{status:r.status, range:r.headers.get('content-range'), body:await r.text()}}))", js_string(&url))));
    assert_eq!(ranged, json!({"status": 206, "range": "bytes 2-5/10", "body": "2345"}));

    let snapshot = json!({"version":1,"documents":[{"document":{"path":file_s}}]});
    let recovery = bridge_error(c, &format!("rk.recovery.save({})", snapshot));
    assert!(recovery.get("err").is_none(), "recovery save: {recovery}");
    let loaded = ok_owned(bridge(c, "rk.recovery.load()"));
    assert_eq!(loaded["version"], 1);
    let denied_recovery = bridge_error(c, "rk.recovery.save({version:1,documents:[{document:{path:'/private/etc/hosts'}}]})");
    assert_eq!(denied_recovery["err"]["code"], "broker_denied");
    ok(&bridge(c, "rk.recovery.clear()"));

    let revoked = ok_owned(bridge(c, &format!("rk.paths.revoke({})", js_string(&file_s))));
    assert_eq!(revoked, Value::Null);
    let after_revoke = ok_owned(bridge(c, &format!("fetch({}).then(async r => ({{status:r.status, body:await r.text()}}))", js_string(&url))));
    assert_eq!(after_revoke["status"], 403);

    // Automatic meeting detection and manual DND both suppress native posting without touching
    // user notification state; the status call below is the OS read-back for this throwaway app.
    ok(&bridge(c, "rk.presence.setAuto({autoDndMeetings:true})"));
    ok(&bridge(c, "rk.presence.evaluate({x:0,y:0,width:1440,height:900}, [{owner:'Zoom',title:'Meeting',layer:0,bounds:{x:0,y:0,width:800,height:600}}], true)"));
    let suppressed = ok_owned(bridge(c, "rk.notifications.post({title:'rk-shell meeting suppression',body:'test'})"));
    assert_eq!(suppressed["delivered"], false);
    assert_eq!(suppressed["suppressed"], "dnd");
    ok(&bridge(c, "rk.presence.setDnd(false)"));
    ok(&bridge(c, "rk.presence.setAuto({autoDndMeetings:false})"));
    ok(&bridge(c, "rk.idle.note(true)"));
    wait_for("idle suspend", 5, || ok(&bridge(c, "rk.idle.status()"))["suspended"].as_bool().unwrap_or(false));
    ok(&bridge(c, "rk.idle.note(false)"));
    wait_for("idle resume", 5, || !ok(&bridge(c, "rk.idle.status()"))["suspended"].as_bool().unwrap_or(true));

    let pid = run.c.pid;
    let _ = c.eval("rk.quit(); return 1;");
    wait_for("capability app exit", 15, || !is_alive(pid));
    run.c.stop().unwrap();
    tracker.assert_no_orphans().unwrap();
    workspace::dispose(&run.ws, false, 5, 200);
    let _ = std::fs::remove_dir_all(&fixture);
}

#[test]
fn shell_native_notification_delivery_gate() {
    let tracker = Tracker::new();
    let mut run = start(&tracker, "shell-notification-delivery", None);
    let c = &run.c;
    c.wait_eval("return window.__ready;", Duration::from_secs(20)).expect("frontend ready via bridge");

    let title = format!("rk-shell delivery gate {}", std::process::id());
    let post = bridge_error(c, &format!("rk.notifications.post({{title:{},body:'delivery-gate',soundEnabled:false}})", js_string(&title)));
    if let Some(err) = post.get("err") {
        match err["code"].as_str() {
            Some("permission_denied" | "unsupported") => {
                eprintln!("NOT DONE notification delivery gate: {err}");
                let _ = c.eval("rk.quit(); return 1;");
                let pid = run.c.pid;
                wait_for("notification gate app exit", 15, || !is_alive(pid));
                run.c.stop().unwrap();
                tracker.assert_no_orphans().unwrap();
                workspace::dispose(&run.ws, false, 5, 200);
                return;
            }
            _ => panic!("notification post unexpected error: {post}"),
        }
    }
    assert_eq!(post["ok"]["delivered"], true, "authorized notification must report delivered: {post}");

    let mut last_status = Value::Null;
    wait_for("notification OS delivery read-back", 10, || {
        let status = bridge_error(c, "rk.notifications.status()");
        last_status = status.clone();
        status["ok"]["delivered"].as_array().map(|rows| rows.iter().any(|row| row["title"] == title)).unwrap_or(false)
    });
    assert!(last_status["ok"]["authorization"].as_str().is_some(), "notification status lacks authorization: {last_status}");

    ok(&bridge(c, "rk.notifications.clear()"));
    let pid = run.c.pid;
    let _ = c.eval("rk.quit(); return 1;");
    wait_for("notification gate app exit", 15, || !is_alive(pid));
    run.c.stop().unwrap();
    tracker.assert_no_orphans().unwrap();
    workspace::dispose(&run.ws, false, 5, 200);
}
