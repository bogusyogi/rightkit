//! A194 diff review pane model: per-file hunks of the session's changes,
//! navigable, with accept/revert per hunk and line comments that are
//! collected to send back to the agent. Presentation only; reverting
//! returns a reverse patch for the host to apply.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Undecided,
    Accepted,
    Reverted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    pub header: String,
    pub lines: Vec<String>,
    pub decision: Decision,
    /// Comments as (index into `lines`, text).
    pub comments: Vec<(usize, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewFile {
    pub path: String,
    pub hunks: Vec<Hunk>,
}

/// Parse a multi-file unified diff (`diff --git` or `--- a/ +++ b/` form).
pub fn parse_unified(diff: &str) -> Vec<ReviewFile> {
    let mut files: Vec<ReviewFile> = Vec::new();
    let mut in_hunk = false;
    for raw in diff.lines() {
        if !in_hunk || raw.starts_with("diff ") {
            // Header region: file markers only.
            if let Some(rest) = raw.strip_prefix("+++ ") {
                let path = rest.strip_prefix("b/").unwrap_or(rest).to_string();
                files.push(ReviewFile {
                    path,
                    hunks: Vec::new(),
                });
                in_hunk = false;
                continue;
            }
            if raw.starts_with("diff ") {
                in_hunk = false;
                continue;
            }
        }
        if raw.starts_with("@@") {
            if let Some(f) = files.last_mut() {
                f.hunks.push(Hunk {
                    header: raw.to_string(),
                    lines: Vec::new(),
                    decision: Decision::Undecided,
                    comments: Vec::new(),
                });
                in_hunk = true;
            }
        } else if in_hunk {
            // A new file's `--- a/..` marker ends the previous hunk.
            if raw.starts_with("--- a/") || raw.starts_with("--- /dev/null") {
                in_hunk = false;
                continue;
            }
            if let Some(h) = files.last_mut().and_then(|f| f.hunks.last_mut()) {
                h.lines.push(raw.to_string());
            }
        }
    }
    files.retain(|f| !f.hunks.is_empty());
    files
}

fn swap_header(header: &str) -> String {
    // "@@ -a,b +c,d @@ tail" -> "@@ -c,d +a,b @@ tail"
    let mut parts = header.splitn(4, ' ');
    let at = parts.next().unwrap_or("@@");
    let old = parts.next().unwrap_or("");
    let new = parts.next().unwrap_or("");
    let tail = parts.next().unwrap_or("");
    let (Some(o), Some(n)) = (old.strip_prefix('-'), new.strip_prefix('+')) else {
        return header.to_string();
    };
    format!("{at} -{n} +{o} {tail}").trim_end().to_string()
}

#[derive(Debug, Clone, Default)]
pub struct DiffReview {
    files: Vec<ReviewFile>,
    file: usize,
    hunk: usize,
}

impl DiffReview {
    pub fn from_diff(diff: &str) -> Self {
        Self {
            files: parse_unified(diff),
            file: 0,
            hunk: 0,
        }
    }

    pub fn files(&self) -> &[ReviewFile] {
        &self.files
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    pub fn current(&self) -> Option<(&ReviewFile, &Hunk)> {
        let f = self.files.get(self.file)?;
        Some((f, f.hunks.get(self.hunk)?))
    }

    /// Next hunk, crossing into the next file; stays put at the end.
    pub fn next_hunk(&mut self) {
        let Some(f) = self.files.get(self.file) else {
            return;
        };
        if self.hunk + 1 < f.hunks.len() {
            self.hunk += 1;
        } else if self.file + 1 < self.files.len() {
            self.file += 1;
            self.hunk = 0;
        }
    }

    /// Previous hunk, crossing into the previous file's last hunk.
    pub fn prev_hunk(&mut self) {
        if self.hunk > 0 {
            self.hunk -= 1;
        } else if self.file > 0 {
            self.file -= 1;
            self.hunk = self.files[self.file].hunks.len().saturating_sub(1);
        }
    }

    pub fn next_file(&mut self) {
        if self.file + 1 < self.files.len() {
            self.file += 1;
            self.hunk = 0;
        }
    }

    pub fn prev_file(&mut self) {
        if self.file > 0 {
            self.file -= 1;
            self.hunk = 0;
        } else {
            // First file: jump back to its first hunk.
            self.hunk = 0;
        }
    }

    fn current_hunk_mut(&mut self) -> Option<&mut Hunk> {
        self.files.get_mut(self.file)?.hunks.get_mut(self.hunk)
    }

    pub fn accept(&mut self) {
        if let Some(h) = self.current_hunk_mut() {
            h.decision = Decision::Accepted;
        }
    }

    /// Mark the hunk reverted and return a patch that undoes it.
    pub fn revert(&mut self) -> Option<String> {
        let path = self.files.get(self.file)?.path.clone();
        let h = self.current_hunk_mut()?;
        h.decision = Decision::Reverted;
        let mut patch = format!("--- a/{path}\n+++ b/{path}\n{}\n", swap_header(&h.header));
        for l in &h.lines {
            let flipped = match l.chars().next() {
                Some('+') => format!("-{}", &l[1..]),
                Some('-') => format!("+{}", &l[1..]),
                _ => l.clone(),
            };
            patch.push_str(&flipped);
            patch.push('\n');
        }
        Some(patch)
    }

    /// Attach a comment to a line of the current hunk. False if out of range.
    pub fn comment(&mut self, line_idx: usize, text: &str) -> bool {
        let text = text.trim();
        let Some(h) = self.current_hunk_mut() else {
            return false;
        };
        if text.is_empty() || line_idx >= h.lines.len() {
            return false;
        }
        h.comments.push((line_idx, text.to_string()));
        true
    }

    /// All comments as agent feedback text; None when there are none.
    pub fn feedback(&self) -> Option<String> {
        let mut out = String::new();
        for f in &self.files {
            for h in &f.hunks {
                for (i, c) in &h.comments {
                    out.push_str(&format!("{} {} line {}: {}\n", f.path, h.header, i + 1, c));
                }
            }
        }
        (!out.is_empty()).then_some(out)
    }

    pub fn lines(&self) -> Vec<Line<'static>> {
        let mut out = Vec::new();
        for (fi, f) in self.files.iter().enumerate() {
            out.push(Line::from(Span::styled(
                f.path.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            for (hi, h) in f.hunks.iter().enumerate() {
                let cursor = if fi == self.file && hi == self.hunk {
                    ">"
                } else {
                    " "
                };
                let tag = match h.decision {
                    Decision::Undecided => "",
                    Decision::Accepted => " [accepted]",
                    Decision::Reverted => " [reverted]",
                };
                out.push(Line::from(format!("{cursor} {}{tag}", h.header)));
                for l in &h.lines {
                    let color = match l.chars().next() {
                        Some('+') => Some(Color::Green),
                        Some('-') => Some(Color::Red),
                        _ => None,
                    };
                    let style = color.map_or_else(Style::default, |c| Style::default().fg(c));
                    out.push(Line::from(Span::styled(format!("  {l}"), style)));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &str = "\
diff --git a/a.rs b/a.rs\nindex 1..2 100644\n--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@ fn\n keep\n-old\n+new\n@@ -10,1 +10,2 @@\n ctx\n+added\ndiff --git a/b.rs b/b.rs\n--- a/b.rs\n+++ b/b.rs\n@@ -1 +1 @@\n-x\n+y\n";

    #[test]
    fn parses_files_and_hunks() {
        let r = DiffReview::from_diff(DIFF);
        assert_eq!(r.files().len(), 2);
        assert_eq!(r.files()[0].path, "a.rs");
        assert_eq!(r.files()[0].hunks.len(), 2);
        assert_eq!(r.files()[0].hunks[0].lines, vec![" keep", "-old", "+new"]);
        assert_eq!(r.files()[1].hunks[0].lines, vec!["-x", "+y"]);
        assert!(DiffReview::from_diff("").is_empty());
    }

    #[test]
    fn navigation_crosses_files_and_clamps() {
        let mut r = DiffReview::from_diff(DIFF);
        r.next_hunk();
        assert_eq!(r.current().unwrap().1.header, "@@ -10,1 +10,2 @@");
        r.next_hunk();
        assert_eq!(r.current().unwrap().0.path, "b.rs");
        r.next_hunk();
        assert_eq!(r.current().unwrap().0.path, "b.rs");
        r.prev_hunk();
        assert_eq!(r.current().unwrap().1.header, "@@ -10,1 +10,2 @@");
        r.prev_file();
        assert_eq!(r.current().unwrap().1.header, "@@ -1,2 +1,2 @@ fn");
        r.next_file();
        assert_eq!(r.current().unwrap().0.path, "b.rs");
    }

    #[test]
    fn accept_marks_hunk() {
        let mut r = DiffReview::from_diff(DIFF);
        r.accept();
        assert_eq!(r.current().unwrap().1.decision, Decision::Accepted);
    }

    #[test]
    fn revert_returns_reverse_patch() {
        let mut r = DiffReview::from_diff(DIFF);
        let p = r.revert().unwrap();
        assert_eq!(
            p,
            "--- a/a.rs\n+++ b/a.rs\n@@ -1,2 +1,2 @@ fn\n keep\n+old\n-new\n"
        );
        assert_eq!(r.current().unwrap().1.decision, Decision::Reverted);
    }

    #[test]
    fn revert_swaps_header_counts() {
        assert_eq!(swap_header("@@ -10,1 +10,2 @@"), "@@ -10,2 +10,1 @@");
    }

    #[test]
    fn comments_collect_into_feedback() {
        let mut r = DiffReview::from_diff(DIFF);
        assert!(r.feedback().is_none());
        assert!(!r.comment(99, "x"));
        assert!(!r.comment(0, "  "));
        assert!(r.comment(1, "why remove?"));
        assert_eq!(
            r.feedback().unwrap(),
            "a.rs @@ -1,2 +1,2 @@ fn line 2: why remove?\n"
        );
    }

    #[test]
    fn lines_mark_cursor_and_decision() {
        let mut r = DiffReview::from_diff(DIFF);
        r.accept();
        let t: Vec<String> = r
            .lines()
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect();
        assert_eq!(t[0], "a.rs");
        assert_eq!(t[1], "> @@ -1,2 +1,2 @@ fn [accepted]");
        assert_eq!(t[3], "  -old");
    }
}
