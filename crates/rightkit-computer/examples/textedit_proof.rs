//! Proof: drive TextEdit in the background; frontmost app must be unchanged.
//! Run: rightkit cargo run -p rightkit-computer --example textedit_proof
#[cfg(target_os = "macos")]
use rightkit_computer::{model::Node, safety::Policy, *};
#[cfg(target_os = "macos")]
use std::{process::Command, thread::sleep, time::Duration};

#[cfg(target_os = "macos")]
fn main() {
    let st = status();
    println!("status: {st:?}");
    if !st.accessibility {
        eprintln!("Accessibility not granted to the host of this process");
        std::process::exit(2);
    }
    let front0 = frontmost_pid().expect("frontmost");
    println!("frontmost before: pid {front0}");

    let policy = Policy::new(["TextEdit"]);
    assert!(policy.check("Finder").is_err(), "allowlist must deny others");
    policy.check("TextEdit").unwrap();

    // Launch without activating (-g).
    Command::new("open").args(["-g", "-a", "TextEdit"]).status().unwrap();
    sleep(Duration::from_millis(1500));
    let te = list_windows(false).into_iter().find(|w| w.owner == "TextEdit").map(|w| w.pid)
        .or_else(|| String::from_utf8(Command::new("pgrep").args(["-x", "TextEdit"]).output().unwrap().stdout).ok()
            .and_then(|s| s.trim().lines().next().and_then(|l| l.parse().ok()))).expect("TextEdit pid");
    let app = El::app(te).unwrap();
    println!("TextEdit pid {te}; frontmost now {:?}", frontmost_pid());

    // Tier 1: AX menu command (File > New) without opening the menu.
    let has_window = |app: &El| app.find(1, &|e| e.role() == "AXTextArea").is_some();
    if !has_window(&app) {
        app.press_menu(&["File", "New"]).expect("File > New");
        sleep(Duration::from_millis(800));
    }
    let area = app.find(6, &|e| e.role() == "AXTextArea").expect("text area");

    // Tier 1: AX set value (no input events).
    area.set_value("hello from rightkit-computer (AX set)").unwrap();
    sleep(Duration::from_millis(200));
    println!("AX value readback: {:?}", area.str_attr("AXValue"));

    // Tier 2: pid-addressed key events appended at the caret.
    let before = area.str_attr("AXValue").unwrap_or_default();
    type_text_to_pid(te, " + keys").unwrap();
    sleep(Duration::from_millis(400));
    let after = area.str_attr("AXValue").unwrap_or_default();
    println!("after CGEventPostToPid keys: {after:?}  (delivered={})", after != before);

    // Observe: tree + per-window screenshot.
    let tree: Node = observe_tree(te, 5, 80).unwrap();
    let mut s = String::new();
    tree.render(&mut s, 0);
    println!("AX tree ({} nodes):\n{}", tree.count(), s.lines().take(14).collect::<Vec<_>>().join("\n"));
    if let Some(w) = list_windows(true).into_iter().find(|w| w.pid == te && w.layer == 0) {
        let p = std::env::temp_dir().join("rkc-textedit.png");
        println!("capture window {} -> {:?}: {:?}", w.window_id, p, capture_window(w.window_id, &p).map(|_| std::fs::metadata(&p).map(|m| m.len())));
    }

    // Kill switch must fail closed.
    safety::kill();
    assert!(policy.check("TextEdit").is_err());
    safety::reset_kill();

    // Cleanup: close doc, discard via sheet button if it appears.
    let _ = app.press_menu(&["File", "Close"]);
    sleep(Duration::from_millis(700));
    if let Some(btn) = app.find(6, &|e| e.role() == "AXButton" && matches!(e.str_attr("AXTitle").as_deref(), Some("Delete") | Some("Don’t Save") | Some("Don't Save"))) {
        let _ = btn.press();
    }

    let front1 = frontmost_pid().expect("frontmost");
    println!("frontmost after: pid {front1}");
    if front0 == front1 { println!("PROOF OK: frontmost app unchanged"); } else { println!("PROOF FAIL: focus moved"); std::process::exit(1); }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("textedit_proof requires macOS & TextEdit");
    std::process::exit(1);
}
