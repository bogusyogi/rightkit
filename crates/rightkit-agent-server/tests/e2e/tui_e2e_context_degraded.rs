//! D18 TUI e2e: with Membrane provably absent (no `RIGHTKIT_CONTEXT=off`
//! shortcut, installed-root env redirected into an empty directory) the TUI
//! does not refuse the turn and keeps no shadow store: the footer shows
//! "⚠ context off · <reason>", a single notice is posted, and the turn runs.

use crate::support::tui_harness as h;
use h::*;

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn unavailable_context_runs_visibly_degraded_and_the_turn_still_answers() {
    let provider = ScriptedProvider::start(vec![
        Step::text("degraded-answer-1"),
        Step::text("degraded-answer-2"),
    ]);
    let redirect = tempfile::tempdir().expect("redirect root");
    let redirect_str = redirect.path().to_string_lossy().to_string();
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            env: &[
                // Anything other than "off" selects the normal Membrane path.
                ("RIGHTKIT_CONTEXT", "on"),
                ("LOCALAPPDATA", redirect_str.as_str()),
                ("APPDATA", redirect_str.as_str()),
            ],
            ..Default::default()
        },
    );
    tui.wait_viewport("context-off footer", |v| v.contains("context off"));
    let footer = tui.viewport();
    assert!(
        footer.contains("⚠ context off ·"),
        "footer names the degradation and a reason:\n{footer}"
    );

    tui.submit("first question");
    tui.wait_for_text("degraded-answer-1");
    tui.submit("second question");
    tui.wait_for_text("degraded-answer-2");

    let screen = tui.screen_text();
    assert_eq!(
        count(&screen, "Context is off ("),
        1,
        "exactly one degraded notice, not one per turn:\n{screen}"
    );
    assert!(
        !screen.contains("Turn not started"),
        "the turn is never refused:\n{screen}"
    );
    assert_eq!(provider.requests().len(), 2, "both turns reached the model");

    // No shadow store: nothing context/memory related was created under the home.
    let shadow = walk(tui.home.path())
        .into_iter()
        .filter(|p| {
            let n = p
                .file_name()
                .map(|n| n.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            n.contains("shadow") || n.contains("cortex") || n.contains("memory.db")
        })
        .collect::<Vec<_>>();
    assert!(
        shadow.is_empty(),
        "no shadow memory store may exist: {shadow:?}"
    );
    assert!(
        walk(redirect.path()).is_empty(),
        "no installed-Membrane effect landed in the redirected app-data root"
    );
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p.clone());
                }
                out.push(p);
            }
        }
    }
    out
}
