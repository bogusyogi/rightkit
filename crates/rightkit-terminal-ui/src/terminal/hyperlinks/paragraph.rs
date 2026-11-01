//! Paragraph rendering that keeps visible text and hyperlink annotations aligned.

#[cfg(test)]
use super::HyperlinkLine;
#[cfg(test)]
use super::mark_buffer_hyperlinks;
#[cfg(test)]
use super::visible_lines_ref;
#[cfg(test)]
use ratatui::buffer::Buffer;
#[cfg(test)]
use ratatui::layout::Rect;
#[cfg(test)]
use ratatui::style::Style;
#[cfg(test)]
use ratatui::text::Text;
#[cfg(test)]
use ratatui::widgets::Paragraph;
#[cfg(test)]
use ratatui::widgets::Widget;
#[cfg(test)]
use ratatui::widgets::Wrap;

/// Word-wraps without trimming and applies the same vertical scroll to text and links.
#[cfg(test)]
pub(crate) struct HyperlinkParagraph<'a> {
    lines: &'a [HyperlinkLine],
    paragraph: Paragraph<'a>,
    scroll_rows: u16,
}

#[cfg(test)]
impl<'a> HyperlinkParagraph<'a> {
    #[cfg(test)]
    pub(crate) fn new(lines: &'a [HyperlinkLine], style: Style) -> Self {
        Self {
            lines,
            paragraph: Paragraph::new(Text::from(visible_lines_ref(lines)))
                .style(style)
                .wrap(Wrap { trim: false }),
            scroll_rows: 0,
        }
    }

    #[cfg(test)]
    pub(crate) fn scroll(mut self, rows: u16) -> Self {
        self.scroll_rows = rows;
        self
    }
}

#[cfg(test)]
impl Widget for HyperlinkParagraph<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        self.paragraph
            .scroll((self.scroll_rows, 0))
            .render(area, buf);
        mark_buffer_hyperlinks(buf, area, self.lines, usize::from(self.scroll_rows));
    }
}
