//! Styling and destination visibility for web links in semantic Markdown output.
//!
//! Terminal identity is a conservative heuristic, not end-to-end capability negotiation. Unknown
//! terminals and multiplexers retain the destination so a label never hides the only usable URL.

use crate::terminal::capability::TerminalInfo;
use crate::terminal::capability::TerminalName;
use crate::terminal::capability::terminal_info;
use std::io::IsTerminal;
use std::sync::LazyLock;

use ratatui::style::Style;
use ratatui::text::Line;
use ratatui::text::Span;
use unicode_segmentation::UnicodeSegmentation;

use crate::terminal::hyperlinks::HyperlinkLine;
use crate::terminal::hyperlinks::annotate_web_urls_in_line;
use crate::terminal::hyperlinks::web_destination;
use crate::transcript::width::display_width;

/// Give detected URLs the same affordance as explicit Markdown links, preserving surrounding text.
pub(super) fn style_bare_web_urls(span: Span<'static>, link_style: Style) -> HyperlinkLine {
    let mut line = annotate_web_urls_in_line(Line::from(span));
    if line.hyperlinks.is_empty() {
        return line;
    }
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut links = line.hyperlinks.iter().peekable();
    let mut column = 0;
    let span = &line.line.spans[0];
    for grapheme in span.content.graphemes(/*is_extended*/ true) {
        while links.peek().is_some_and(|link| link.columns.end <= column) {
            links.next();
        }
        let style = if links
            .peek()
            .is_some_and(|link| link.columns.contains(&column))
        {
            span.style.patch(link_style)
        } else {
            span.style
        };
        if let Some(last) = spans.last_mut()
            && last.style == style
        {
            last.content.to_mut().push_str(grapheme);
        } else {
            spans.push(Span::styled(grapheme.to_owned(), style));
        }
        column += display_width(grapheme);
    }
    line.line.spans = spans;
    line
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WebLinkDisplay {
    LabelOnly,
    WithDestination,
}

impl WebLinkDisplay {
    fn for_terminal(terminal: &TerminalInfo, term: Option<&str>) -> Self {
        if terminal.multiplexer.is_some()
            || term.is_some_and(|term| {
                term == "dumb" || term.starts_with("screen") || term.starts_with("tmux")
            })
        {
            return Self::WithDestination;
        }
        match terminal.name {
            TerminalName::Ghostty
            | TerminalName::Iterm2
            | TerminalName::WezTerm
            | TerminalName::Kitty
            | TerminalName::VsCode
            | TerminalName::Alacritty
            | TerminalName::WindowsTerminal
            | TerminalName::Konsole
            | TerminalName::GnomeTerminal
            | TerminalName::Vte => Self::LabelOnly,
            TerminalName::AppleTerminal
            | TerminalName::WarpTerminal
            | TerminalName::Dumb
            | TerminalName::Unknown => Self::WithDestination,
        }
    }

    fn hide_destination(self, destination: &str) -> bool {
        self == Self::LabelOnly && web_destination(destination).is_some()
    }
}

pub(crate) fn hide_web_link_destination(destination: &str) -> bool {
    static DISPLAY: LazyLock<WebLinkDisplay> = LazyLock::new(|| {
        if !std::io::stdout().is_terminal() || std::env::var_os("STY").is_some() {
            return WebLinkDisplay::WithDestination;
        }
        WebLinkDisplay::for_terminal(&terminal_info(), std::env::var("TERM").ok().as_deref())
    });
    DISPLAY.hide_destination(destination)
}
