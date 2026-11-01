//! Black-box PTY qualification for streaming Markdown's stable-prefix contract.

use crate::support::tui_harness as h;
use h::*;

#[test]
fn incomplete_markdown_tail_stays_hidden_until_safe_boundary_arrives() {
    // ScriptedProvider splits at the midpoint. Equal-sized halves put the
    // closing emphasis delimiter in the second live chunk deterministically.
    let first = "render-stable-prefix\n\n**unfinished marker";
    let second = format!(
        "** completed{}",
        "x".repeat(first.len() - "** completed".len())
    );
    let answer = format!("{first}{second}");
    assert_eq!(first.chars().count(), second.chars().count());

    let provider = ScriptedProvider::start(vec![Step::text(&answer).held()]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("stream markup");
    tui.wait_requests(&provider, 1);

    let streaming = tui.wait_for_text("render-stable-prefix");
    assert!(
        !streaming.contains("unfinished marker"),
        "an incomplete Markdown tail must remain uncommitted while streaming:\n{streaming}"
    );

    provider.release(0);
    let settled = tui.wait_for_text("completed");
    assert!(
        settled.contains("unfinished marker") && settled.contains("completed"),
        "the completed Markdown tail appears after its safe boundary:\n{settled}"
    );
    assert!(
        !settled.contains("**unfinished") && !settled.contains("marker**"),
        "resolved emphasis delimiters are rendered as markup:\n{settled}"
    );
}
