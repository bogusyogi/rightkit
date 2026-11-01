//! Closing PTY journeys for the remaining TUI approval/model parity gaps.

use crate::support::tui_harness as h;
use h::*;

fn prefix_option_digit(view: &str) -> Option<u8> {
    view.lines().find_map(|line| {
        let prefix = line.split_once("Always allow:")?.0;
        prefix
            .chars()
            .rev()
            .find(|ch| ch.is_ascii_digit())
            .and_then(|ch| ch.to_digit(10))
            .map(|digit| digit as u8)
    })
}

#[test]
fn shell_prefix_allowance_persists_through_the_existing_policy_authority() {
    let provider = ScriptedProvider::start(vec![
        shell_call("prefix_1", "echo tui-prefix-grant-1842"),
        shell_call("prefix_2", "echo tui-prefix-grant-1842"),
        Step::text("prefix grant finished"),
    ]);
    let mut tui = Tui::launch(&provider, &[]);
    tui.ready();
    tui.submit("run same command twice");

    let card = tui.wait_viewport("prefix approval choice", |view| {
        view.contains("Always allow:")
    });
    assert!(card.contains("echo tui-prefix-grant-1842"), "{card}");
    let digit = prefix_option_digit(&card).expect("numbered prefix option");
    tui.send(digit.to_string().as_bytes());
    tui.wait_for_text("prefix grant finished");

    let requests = provider.requests();
    assert_eq!(
        requests.len(),
        3,
        "repeat must pass without a second approval"
    );
    let second_results = messages(&requests[2])
        .iter()
        .filter(|message| message["role"] == "tool")
        .map(content_text)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        second_results.contains("tui-prefix-grant-1842"),
        "{second_results}"
    );
    assert!(!tui.screen_text().contains("rejected shell"));
}
