use super::ScrollbackStrategy;
use crate::terminal::capability::Multiplexer;
use crate::terminal::capability::TerminalInfo;
use crate::terminal::capability::TerminalName;
use crate::terminal::history::HistoryLineWrapPolicy;
use crate::terminal::history::InsertMode;
use crate::terminal::history::Viewport;
use crate::terminal::history::insert;
use crate::terminal::hyperlinks::plain_hyperlink_lines;
use crate::terminal::test_backend::VT100Backend;
use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::Print;
use pretty_assertions::assert_eq;
use ratatui::layout::Size;
use ratatui::text::Line;

#[test]
fn windows_terminal_uses_full_screen_unless_zellij_is_active() {
    let mut terminal = TerminalInfo {
        name: TerminalName::WindowsTerminal,
        term_program: None,
        version: None,
        term: None,
        multiplexer: None,
    };
    let mut strategy = ScrollbackStrategy::detect(&terminal);

    assert_eq!(
        [
            strategy.history_insertion_mode(HistoryLineWrapPolicy::PreWrap),
            strategy.history_insertion_mode(HistoryLineWrapPolicy::Terminal),
        ],
        [InsertMode::FullScreen, InsertMode::FullScreen]
    );

    terminal.multiplexer = Some(Multiplexer::Zellij);
    strategy = ScrollbackStrategy::detect(&terminal);
    assert_eq!(strategy, ScrollbackStrategy::Zellij);
}

#[test]
fn zellij_only_uses_full_screen_insertion_for_terminal_wrapped_history() {
    assert_eq!(
        [
            ScrollbackStrategy::Zellij.history_insertion_mode(HistoryLineWrapPolicy::PreWrap),
            ScrollbackStrategy::Zellij.history_insertion_mode(HistoryLineWrapPolicy::Terminal),
        ],
        [InsertMode::ScrollRegion, InsertMode::FullScreen]
    );
}

#[test]
fn full_screen_history_insertion_preserves_terminal_scrollback() {
    let width = 24;
    let height = 6;
    let mut backend = VT100Backend::with_scrollback(width, height, /*scrollback_len*/ 32);
    let viewport = Viewport {
        top: height - 2,
        height: 2,
    };

    for (row, line) in [
        "oldest-history-row",
        "history-row-2",
        "history-row-3",
        "history-row-4",
        "stale-composer-1",
        "stale-composer-2",
    ]
    .into_iter()
    .enumerate()
    {
        queue!(&mut backend, MoveTo(/*x*/ 0, row as u16), Print(line)).expect("seed terminal row");
    }

    let lines = plain_hyperlink_lines(vec![Line::from("new-history-row")]);
    insert(
        &mut backend,
        ScrollbackStrategy::FullScreen.history_insertion_mode(HistoryLineWrapPolicy::PreWrap),
        viewport,
        width,
        height,
        &lines,
        HistoryLineWrapPolicy::PreWrap,
        /*restore_cursor*/ (0, height - 1),
    )
    .expect("insert history through the full screen");

    // vt100 0.16 `contents()` trims trailing blank rows; enumerate every row
    // so the cleared viewport rows are checked, not dropped.
    let visible = backend.vt100().screen().rows(0, width).collect::<Vec<_>>();
    let mut scrollback_screen = backend.vt100().screen().clone();
    scrollback_screen.set_scrollback(/*rows*/ usize::MAX);
    // A fully scrolled-up window shows the scrollback row plus the screen's
    // first `height - scrollback` rows.
    let scrollback = scrollback_screen.rows(0, width).collect::<Vec<_>>();

    assert_eq!(
        scrollback,
        [
            "oldest-history-row",
            "history-row-2",
            "history-row-3",
            "history-row-4",
            "new-history-row",
            "",
        ],
    );
    assert_eq!(
        visible,
        [
            "history-row-2",
            "history-row-3",
            "history-row-4",
            "new-history-row",
            "",
            "",
        ],
    );
}

#[test]
fn full_screen_viewport_growth_preserves_terminal_scrollback() {
    let width = 24;
    let height = 6;
    let mut backend = VT100Backend::with_scrollback(width, height, /*scrollback_len*/ 32);
    let viewport_top = height - 2;

    for (row, line) in [
        "oldest-history-row",
        "history-row-2",
        "history-row-3",
        "history-row-4",
        "stale-composer-1",
        "stale-composer-2",
    ]
    .into_iter()
    .enumerate()
    {
        queue!(&mut backend, MoveTo(/*x*/ 0, row as u16), Print(line)).expect("seed terminal row");
    }

    ScrollbackStrategy::FullScreen
        .grow_viewport(
            &mut backend,
            viewport_top,
            height,
            /*scroll_by*/ 2,
            /*restore_cursor*/ (0, height - 1),
        )
        .expect("grow viewport through full-screen scrolling");

    // vt100 0.16 `contents()` trims trailing blank rows; enumerate every row
    // so the cleared viewport rows are checked, not dropped.
    let visible = backend.vt100().screen().rows(0, width).collect::<Vec<_>>();
    let mut scrollback_screen = backend.vt100().screen().clone();
    scrollback_screen.set_scrollback(/*rows*/ usize::MAX);
    // A fully scrolled-up window shows the two scrollback rows plus the
    // screen's first `height - 2` rows.
    let scrollback = scrollback_screen.rows(0, width).collect::<Vec<_>>();

    assert_eq!(
        scrollback,
        [
            "oldest-history-row",
            "history-row-2",
            "history-row-3",
            "history-row-4",
            "",
            "",
        ],
    );
    assert_eq!(visible, ["history-row-3", "history-row-4", "", "", "", ""],);
}

#[test]
fn standard_viewport_growth_uses_scrollback_preserving_newlines() {
    for scroll_by in [1, 3, 4] {
        let mut out = Vec::<u8>::new();
        ScrollbackStrategy::Standard
            .grow_viewport(
                &mut out,
                /*viewport_top*/ 4,
                /*screen_height*/ 6,
                scroll_by,
                /*restore_cursor*/ (7, 5),
            )
            .expect("grow standard viewport");
        assert_eq!(
            String::from_utf8(out).unwrap(),
            format!(
                "\x1b[1;4r\x1b[4;1H{}\x1b[r\x1b[6;8H",
                "\r\n".repeat(usize::from(scroll_by))
            )
        );
    }
}

#[test]
fn standard_viewport_growth_with_one_history_row_preserves_scrollback() {
    let width = 24;
    let height = 3;
    let mut backend = VT100Backend::with_scrollback(width, height, /*scrollback_len*/ 32);
    queue!(
        &mut backend,
        Print("history\r\nstale-composer\r\nstale-footer")
    )
    .expect("seed terminal");
    ScrollbackStrategy::Standard
        .grow_viewport(
            &mut backend,
            /*viewport_top*/ 1,
            height,
            /*scroll_by*/ 1,
            /*restore_cursor*/ (0, height - 1),
        )
        .expect("grow past the single history row");
    let screen = backend.vt100().screen();
    let mut history = screen.clone();
    history.set_scrollback(/*rows*/ usize::MAX);
    assert_eq!(
        format!(
            "SCROLLBACK:\n{}\nVISIBLE:\n{}",
            history.contents(),
            screen.contents()
        ),
        "SCROLLBACK:\nhistory\nVISIBLE:\n"
    );
}

#[test]
fn standard_viewport_growth_without_rows_is_cursor_neutral() {
    for (viewport_top, scroll_by) in [(4, 0), (0, 1)] {
        let mut backend = VT100Backend::new(/*width*/ 24, /*height*/ 6);
        let cursor = (7u16, 5u16);
        let before = backend.vt100().screen().state_formatted();
        ScrollbackStrategy::Standard
            .grow_viewport(
                &mut backend,
                viewport_top,
                /*screen_height*/ 6,
                scroll_by,
                cursor,
            )
            .expect("no-op viewport growth");
        assert_eq!(backend.vt100().screen().state_formatted(), before);
    }
    let _ = Size::new(0, 0);
}
