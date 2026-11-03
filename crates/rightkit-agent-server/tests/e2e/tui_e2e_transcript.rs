//! TUI end-to-end: what the transcript shows — assistant markdown, user text,
//! tool ledger rows, long output, failures, empty turns, reasoning.

use crate::support::tui_harness as h;
use h::*;
use std::time::Duration;

fn pause(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

const MARKDOWN: &str = "# Release notes\n\nSome **bold** text, `inline code` and a [docs link](https://example.com/docs).\n\n- first bullet\n- second bullet\n\n```rust\nfn main() { println!(\"fenced-code-line\"); }\n```\n\n| col a | col b |\n|-------|-------|\n| cell1 | cell2 |\n\n> quoted words\n\nclosing sentence done";

// 1. Assistant markdown is rendered, not echoed raw.
#[test]
fn assistant_markdown_renders_structure_without_raw_syntax() {
    let provider = ScriptedProvider::start(vec![Step::text(MARKDOWN)]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("show me markdown");
    let screen = tui.wait_for_text("closing sentence done");
    println!("{screen}");
    assert!(screen.contains("Release notes"), "{screen}");
    assert!(
        screen.contains("Some bold text, inline code and a docs link"),
        "inline marks stripped:\n{screen}"
    );
    assert!(
        !screen.contains("**bold**") && !screen.contains("`inline code`"),
        "raw inline syntax leaked:\n{screen}"
    );
    assert!(
        screen.contains("https://example.com/docs"),
        "link target stays visible:\n{screen}"
    );
    assert!(
        screen.contains("• first bullet") && screen.contains("• second bullet"),
        "bullets:\n{screen}"
    );
    assert!(
        screen.contains(r#"fn main() { println!("fenced-code-line"); }"#),
        "code block body verbatim:\n{screen}"
    );
    assert!(!screen.contains("```"), "fences are not shown:\n{screen}");
    assert!(
        screen.contains("col a") && screen.contains("cell2"),
        "table cells:\n{screen}"
    );
    assert!(
        !screen.contains("|-------|"),
        "table separator row is drawn, not echoed:\n{screen}"
    );
    assert!(screen.contains("quoted words"), "{screen}");
}

// 2. The user's own prompt is literal text, never interpreted as markdown.
#[test]
fn user_prompt_is_echoed_literally() {
    let provider = ScriptedProvider::start(vec![Step::text("noted")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("keep **these stars** and `ticks` literal");
    let screen = tui.wait_for_text("noted");
    assert!(
        screen.contains("▌ keep **these stars** and `ticks` literal"),
        "{screen}"
    );
    assert!(last_user_text(&provider.requests()[0]).contains("**these stars**"));
}

// 3. Wide (CJK) and combining text wraps by display width and loses nothing.
#[test]
fn wide_unicode_wraps_by_display_width_without_loss() {
    let wide = "日本語のテキストを折り返して表示するテスト。".repeat(4);
    let provider = ScriptedProvider::start(vec![Step::text(&format!("{wide} end-marker"))]);
    let mut tui = Tui::launch_with(
        &provider,
        LaunchOptions {
            cols: 60,
            ..Default::default()
        },
    );
    tui.ready_loose();
    tui.submit("wide please");
    let screen = tui.wait_for_text("end-marker");
    let joined: String = screen
        .lines()
        .filter(|l| !l.contains('▌'))
        .collect::<Vec<_>>()
        .join("");
    let squeezed: String = joined.chars().filter(|c| !c.is_whitespace()).collect();
    let expected: String = wide.chars().filter(|c| !c.is_whitespace()).collect();
    assert!(
        squeezed.contains(&expected),
        "every wide char survives wrapping:\n{screen}"
    );
    let rows = screen.lines().filter(|l| l.contains("日本語")).count();
    assert!(
        rows >= 2,
        "a long wide line wraps onto several rows:\n{screen}"
    );
}

// 4. Resize: the composer + footer re-pin to the new geometry and new output wraps to the new width.
#[test]
fn resize_repins_composer_and_footer_and_wraps_new_output() {
    let long = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec romeo sierra tango";
    let provider = ScriptedProvider::start(vec![Step::text("before resize"), Step::text(long)]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("first");
    tui.wait_for_text("before resize");

    tui.resize(24, 50);
    let view = tui.wait_viewport("footer repinned after shrink", |v| {
        v.lines()
            .last()
            .is_some_and(|l| l.contains("Ask") && l.contains("e2e-branch"))
            && v.contains("▌")
    });
    for row in view.lines() {
        assert!(
            row.chars().count() <= 50,
            "row wider than the terminal: {row:?}
{view}"
        );
    }
    tui.submit("second");
    tui.wait_for_text("tango");
    let view = tui.viewport();
    let words: Vec<&str> = long.split_whitespace().collect();
    let shown: Vec<&str> = view.split_whitespace().collect();
    for w in &words {
        assert!(
            shown.contains(w),
            "word {w:?} lost when wrapping at 50 columns:
{view}"
        );
    }
    assert!(
        view.lines()
            .any(|l| l.contains("alpha") && !l.contains("tango")),
        "the answer is wrapped:
{view}"
    );
    for row in view.lines() {
        assert!(
            row.chars().count() <= 50,
            "row wider than the terminal: {row:?}"
        );
    }

    tui.resize(40, 140);
    let view = tui.wait_viewport("grown", |v| v.contains(MODEL) && v.contains("Ask anything"));
    assert!(
        view.lines().last().unwrap().contains("Ask"),
        "footer on the last row:
{view}"
    );
    tui.type_slow("after resize");
    tui.wait_viewport_text("after resize");
}

// 5. Tool ledger: a file edit shows its diff rows, a failing shell opens to its error, a success is one row.
#[test]
fn tool_calls_render_as_ledger_rows_and_failures_open_to_the_error() {
    let provider = ScriptedProvider::start(vec![
        Step::tool(
            "t1",
            "write_file",
            json!({"path": "ledger.txt", "content": "one\ntwo\n"}),
        ),
        shell_call("t2", "exit 3"),
        shell_call("t3", "echo ledger-ok-77"),
        Step::text("ledger finished"),
    ]);
    let mut tui = Tui::launch(&provider, &["--yolo"]);
    tui.ready();
    tui.submit("run the ledger scenario");
    let screen = tui.wait_for_text("ledger finished");
    println!("{screen}");
    assert!(
        screen.contains("✓ edit") && screen.contains("ledger.txt") && screen.contains("+2 −0"),
        "{screen}"
    );
    assert!(
        screen.contains("1 + one") && screen.contains("2 + two"),
        "numbered diff rows:\n{screen}"
    );
    assert!(
        screen.contains("✗ shell") && screen.contains("exit 3") && screen.contains("[exit 3]"),
        "{screen}"
    );
    assert!(
        screen.contains("╰ output") && screen.contains("ctrl+o full output"),
        "failure row offers the full output:
{screen}"
    );
    assert!(
        screen.contains("✓ shell") && screen.contains("echo ledger-ok-77"),
        "{screen}"
    );
    let requests = provider.requests();
    let results = messages(&requests[3])
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(content_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        results.contains("[exit 3]") && results.contains("ledger-ok-77"),
        "model sees the real results: {results}"
    );
}

// 5b. The failure footer names the command's real exit code.
#[test]
fn failed_shell_footer_names_the_real_exit_code() {
    let provider = ScriptedProvider::start(vec![
        shell_call("x3", "exit 3"),
        Step::text("exit code shown"),
    ]);
    let mut tui = Tui::launch(&provider, &["--yolo"]);
    tui.ready();
    tui.submit("fail with three");
    let screen = tui.wait_for_text("exit code shown");
    assert!(
        screen.contains("exit 3") && !screen.contains("· exit 1"),
        "{screen}"
    );
}

// 6. Long output collapses to a count; Ctrl+O reveals it.
#[test]
fn long_shell_output_collapses_and_ctrl_o_expands_it() {
    let command = if cfg!(windows) {
        "1..300 | % { \"outline $_\" }"
    } else {
        "seq 1 300 | sed 's/^/outline /'"
    };
    let provider = ScriptedProvider::start(vec![
        shell_call("lo", command),
        Step::text("long output done"),
    ]);
    let mut tui = Tui::launch(&provider, &["--yolo"]);
    tui.ready();
    tui.submit("print a lot");
    let screen = tui.wait_for_text("long output done");
    assert!(
        screen.contains("lines · ctrl+o"),
        "collapsed with a count:\n{screen}"
    );
    assert!(
        !screen.contains("outline 300"),
        "the body stays collapsed:
{screen}"
    );
    tui.key(b"\x0f");
    let expanded = tui.wait_for("expanded output", |s| s.contains("outline 300"));
    assert!(
        expanded.matches("outline ").count() > 30,
        "ctrl+o shows the output body:
{expanded}\n--- raw PTY ---\n{:?}",
        tui.raw_output()
    );
    let model = messages(&provider.requests()[1])
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(content_text)
        .collect::<String>();
    assert!(
        model.contains("outline 100"),
        "the model receives the real output"
    );
}

// 7. Ctrl+O after a newer turn never re-appends an older turn's tool row.
#[test]
fn ctrl_o_after_a_text_only_turn_has_nothing_to_expand() {
    let provider = ScriptedProvider::start(vec![
        shell_call("c1", "echo first-turn-row-3141"),
        Step::text("turn one done"),
        Step::text("turn two done"),
    ]);
    let mut tui = Tui::launch(&provider, &["--yolo"]);
    tui.ready();
    tui.submit("turn one");
    tui.wait_for_text("turn one done");
    tui.submit("turn two");
    tui.wait_for_text("turn two done");
    pause(300);
    let before = tui.screen_text().matches("first-turn-row-3141").count();
    tui.key(b"\x0f");
    let screen = tui.wait_for_text("Nothing to expand.");
    assert_eq!(
        screen.matches("first-turn-row-3141").count(),
        before,
        "an old turn's tool row must not be re-appended:\n{screen}"
    );
}

// 7b. Ctrl+O after a second turn that ran its own tool expands THAT turn only.
#[test]
fn ctrl_o_after_a_new_tool_turn_expands_only_the_new_row() {
    let long = if cfg!(windows) {
        "1..300 | % { \"newrow $_\" }"
    } else {
        "seq 1 300 | sed 's/^/newrow /'"
    };
    let provider = ScriptedProvider::start(vec![
        shell_call("o1", "echo old-turn-row-2718"),
        Step::text("old turn done"),
        shell_call("o2", long),
        Step::text("new turn done"),
    ]);
    let mut tui = Tui::launch(&provider, &["--yolo"]);
    tui.ready();
    tui.submit("old turn");
    tui.wait_for_text("old turn done");
    tui.submit("new turn");
    tui.wait_for_text("new turn done");
    pause(300);
    let before = tui.screen_text().matches("old-turn-row-2718").count();
    tui.key(b"");
    let screen = tui.wait_for("new row expanded", |s| s.contains("newrow 300"));
    assert!(
        screen.matches("old-turn-row-2718").count() <= before,
        "the old turn's row is not re-appended:
{screen}"
    );
    // Closing the expanded view returns to the transcript with the old row shown exactly as before.
    tui.key(ESC);
    tui.wait_viewport("expanded view closed", |v| v.contains("Ask anything"));
    pause(300);
    assert_eq!(
        tui.screen_text().matches("old-turn-row-2718").count(),
        before,
        "old row count unchanged after closing"
    );
}

// 8. A turn that yields neither text nor tools is never silent.
#[test]
fn a_turn_with_no_response_is_reported_not_silent() {
    let empty = Step {
        prompt_tokens: 100,
        completion_tokens: 0,
        ..Step::default()
    };
    let provider = ScriptedProvider::start(vec![empty.clone(), empty.clone(), empty]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("say nothing");
    let screen = tui.wait_for("empty turn reported", |s| {
        s.contains("Turn ended with no response") || s.contains("Turn failed")
    });
    println!("{screen}");
    assert!(
        screen.contains("empty response"),
        "retries are visible:\n{screen}"
    );
    assert!(
        provider.request_count() >= 2,
        "the engine retries an empty response"
    );
    // the UI is usable again
    tui.wait_viewport("idle composer", |v| v.contains("Ask anything"));
}

// 9. Reasoning shows as a Thought row BEFORE the answer, even when the summary
//    arrives after the answer text.
#[test]
fn reasoning_summary_renders_before_the_answer() {
    let provider = ScriptedProvider::start(vec![Step::text("The final answer body.")
        .with_late_reasoning("**Checking the premises** late summary")]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("think about it");
    let screen = tui.wait_for("late Thought row and answer", |s| {
        s.contains("Checking the premises") && s.contains("The final answer body.")
    });
    let thought = screen
        .lines()
        .position(|l| l.contains("Checking the premises"))
        .expect("a Thought row exists");
    let answer = screen
        .lines()
        .position(|l| l.contains("The final answer body."))
        .unwrap();
    assert!(
        thought < answer,
        "Thought row precedes the answer:\n{screen}"
    );
    assert_eq!(
        screen.matches("Checking the premises").count(),
        1,
        "late reasoning settles into one stable row:\n{screen}"
    );
}

// 10. Hostile terminal bytes in tool output are neutralised.
#[test]
fn escape_sequences_in_tool_output_do_not_reach_the_terminal() {
    let command = if cfg!(windows) {
        "Write-Output ([char]27 + \"]0;PWNED-TITLE\" + [char]7 + \"visible-tail\")"
    } else {
        "printf '\\033]0;PWNED-TITLE\\007visible-tail\\n'"
    };
    let provider = ScriptedProvider::start(vec![
        shell_call("hx", command),
        Step::text("hostile output handled"),
    ]);
    let mut tui = Tui::launch(&provider, &["--yolo"]);
    tui.ready();
    tui.submit("print hostile bytes");
    let screen = tui.wait_for_text("hostile output handled");
    assert!(
        screen.contains("visible-tail"),
        "the printable part is shown:\n{screen}"
    );
    assert!(
        !tui.raw_output().contains("\u{1b}]0;PWNED-TITLE"),
        "title-set escape leaked to the terminal"
    );
}
