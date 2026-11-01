//! A070 / P-16: attach the TUI to an interactive PTY process.
//!
//! The engine already owns the PTY lifecycle (spawn, lease-fenced stdin,
//! resize, output tail, attach/detach, kill; ConPTY on Windows, openpty on
//! Unix). This module is the presentation half: it turns TUI events into
//! [`PtyIntent`]s the host executes against the engine, and turns the engine's
//! output snapshots into transcript-ready lines. It constructs no engine type
//! and performs no I/O, so every rule here is unit-testable.
//!
//! Usage from the host:
//! - parse `/pty attach <id>` with [`parse_command`];
//! - on an accepted attach, create [`PtyAttach::new`] with the lease the
//!   engine returned and the current terminal size;
//! - while attached, route every [`Event`] through [`PtyAttach::handle`] and
//!   execute [`PtyAttach::drain_intents`];
//! - on a poll tick, feed the engine's output snapshot to
//!   [`PtyAttach::ingest_snapshot`] and print the returned lines.

use crate::component::{Event, Outcome};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::time::{Duration, Instant};

/// Detach chord: Ctrl+] (byte 0x1d), the telnet/ssh-style escape. It is never
/// forwarded to the child.
pub const DETACH_BYTE: u8 = 0x1d;
/// Raw output tail kept for overlap detection between snapshots.
const OVERLAP_PROBE: usize = 128;
/// Largest paste forwarded as a single write.
const MAX_WRITE_BYTES: usize = 8 * 1024;
/// Rows reserved below the PTY viewport for the host's own footer.
const CHROME_ROWS: u16 = 1;

/// #291: the human always wins the shared PTY input. After any human key or
/// paste the agent may not type until the human has been idle this long.
pub const HUMAN_IDLE: Duration = Duration::from_millis(1500);
/// Agent bytes held back while the human is typing; beyond this they are refused.
const MAX_AGENT_QUEUE_BYTES: usize = 64 * 1024;

/// What happened to input the agent tried to type into the shared PTY.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentInput {
    /// The human is idle: the write was queued as a fenced intent.
    Sent,
    /// The human is typing: held until they go idle, then sent in order.
    Queued,
    /// The human is typing and the hold-back queue is full: dropped, not sent.
    Refused,
}

/// A parsed `/pty` command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyCommand {
    Attach(String),
    Detach,
    Kill,
}

/// Parse `/pty attach <id>`, `/pty detach`, `/pty kill`. `None` for anything
/// else (including a bare `/pty`, which the host can answer with [`USAGE`]).
pub fn parse_command(text: &str) -> Option<PtyCommand> {
    let mut words = text.split_whitespace();
    if words.next()? != "/pty" {
        return None;
    }
    match (words.next()?, words.next(), words.next()) {
        ("attach", Some(id), None) => Some(PtyCommand::Attach(id.to_string())),
        ("detach", None, None) => Some(PtyCommand::Detach),
        ("kill", None, None) => Some(PtyCommand::Kill),
        _ => None,
    }
}

pub const USAGE: &str =
    "Usage: /pty attach <id> | /pty detach | /pty kill  (Ctrl+] detaches while attached)";

/// Lease the engine granted for this attach (`epoch` fences stale writers;
/// `next_sequence` is the input sequence the next write must carry).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PtyLease {
    pub epoch: u64,
    pub next_sequence: u64,
}

/// What the host must do on the attached PTY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PtyIntent {
    /// Write bytes with the carried lease fence.
    Write {
        bytes: Vec<u8>,
        epoch: u64,
        sequence: u64,
    },
    Resize {
        rows: u16,
        cols: u16,
    },
    Detach,
}

/// One attached interactive PTY session.
pub struct PtyAttach {
    id: String,
    lease: PtyLease,
    rows: u16,
    cols: u16,
    intents: Vec<PtyIntent>,
    /// Last raw snapshot the engine returned (tail used for overlap).
    last_snapshot: String,
    /// Sanitized text of the current, not-yet-newline-terminated line.
    partial: String,
    ended: bool,
    /// #126: VT100/xterm emulator fed the PTY's raw bytes; its screen grid is
    /// what the host paints while attached, so full-screen programs render.
    term: vt100::Parser,
    /// #291: when the human last pressed a key or pasted.
    last_human_input: Option<Instant>,
    /// #291: true while the human holds the input; the agent lease is released.
    agent_released: bool,
    /// #291: set on the held->released transition so the host can tell the engine once.
    release_notice: bool,
    /// #291: agent writes held back while the human is typing.
    agent_queue: Vec<Vec<u8>>,
}

impl std::fmt::Debug for PtyAttach {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // vt100::Parser has no Debug impl; the screen grid is omitted.
        f.debug_struct("PtyAttach")
            .field("id", &self.id)
            .field("lease", &self.lease)
            .field("rows", &self.rows)
            .field("cols", &self.cols)
            .field("intents", &self.intents)
            .field("last_snapshot", &self.last_snapshot)
            .field("partial", &self.partial)
            .field("ended", &self.ended)
            .finish_non_exhaustive()
    }
}

impl PtyAttach {
    /// `width`/`height` are the terminal size; the PTY gets the viewport minus
    /// the host chrome row. The initial size is queued as a resize intent so
    /// the child agrees with the terminal the user is looking at.
    pub fn new(id: impl Into<String>, lease: PtyLease, width: u16, height: u16) -> Self {
        let (rows, cols) = viewport(width, height);
        Self {
            id: id.into(),
            lease,
            rows,
            cols,
            intents: vec![PtyIntent::Resize { rows, cols }],
            last_snapshot: String::new(),
            partial: String::new(),
            ended: false,
            term: vt100::Parser::new(rows, cols, 0),
            last_human_input: None,
            agent_released: false,
            release_notice: false,
            agent_queue: Vec::new(),
        }
    }

    fn human_idle(&self, now: Instant) -> bool {
        self.last_human_input
            .is_none_or(|at| now.saturating_duration_since(at) >= HUMAN_IDLE)
    }

    fn note_human_input(&mut self, now: Instant) {
        self.last_human_input = Some(now);
        if !self.agent_released {
            // Any human keypress releases the agent's input immediately.
            self.agent_released = true;
            self.release_notice = true;
        }
    }

    /// True while the agent may type (human idle, lease not released).
    pub fn agent_lease_held(&self) -> bool {
        !self.agent_released
    }

    /// One-shot: true if a human keypress released the agent lease since the last call.
    pub fn take_agent_release_notice(&mut self) -> bool {
        std::mem::take(&mut self.release_notice)
    }

    /// Bytes of agent input waiting for the human to go idle.
    pub fn queued_agent_bytes(&self) -> usize {
        self.agent_queue.iter().map(Vec::len).sum()
    }

    /// The agent tries to type. Only allowed while the human is idle; otherwise
    /// the bytes are held (bounded) and sent after the human goes idle.
    pub fn agent_write_at(&mut self, bytes: &[u8], now: Instant) -> AgentInput {
        self.poll_agent_at(now);
        if !self.agent_released {
            self.queue_write(bytes);
            return AgentInput::Sent;
        }
        if self.queued_agent_bytes() + bytes.len() > MAX_AGENT_QUEUE_BYTES {
            return AgentInput::Refused;
        }
        self.agent_queue.push(bytes.to_vec());
        AgentInput::Queued
    }

    pub fn agent_write(&mut self, bytes: &[u8]) -> AgentInput {
        self.agent_write_at(bytes, Instant::now())
    }

    /// Once the human has been idle, hand the lease back to the agent and send
    /// whatever it queued, in order.
    pub fn poll_agent_at(&mut self, now: Instant) {
        if self.agent_released && self.human_idle(now) {
            self.agent_released = false;
            for chunk in std::mem::take(&mut self.agent_queue) {
                self.queue_write(&chunk);
            }
        }
    }

    pub fn poll_agent(&mut self) {
        self.poll_agent_at(Instant::now());
    }

    /// The emulated screen the host paints while attached.
    pub fn screen(&self) -> &vt100::Screen {
        self.term.screen()
    }

    /// Feed raw PTY output straight to the emulator (no line splitting).
    pub fn feed_raw(&mut self, bytes: &[u8]) {
        self.term.process(bytes);
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn size(&self) -> (u16, u16) {
        (self.rows, self.cols)
    }

    pub fn lease(&self) -> PtyLease {
        self.lease
    }

    /// The unfinished line (e.g. a prompt); show it as the composer placeholder.
    pub fn partial_line(&self) -> &str {
        &self.partial
    }

    /// True once the child's output ended or the host marked it exited.
    pub fn ended(&self) -> bool {
        self.ended
    }

    pub fn mark_ended(&mut self) {
        self.ended = true;
    }

    /// The engine accepted a write; the next write uses the next sequence.
    pub fn write_accepted(&mut self) {
        self.lease.next_sequence = self.lease.next_sequence.saturating_add(1);
    }

    /// The engine rejected a write (stale epoch / sequence): adopt its lease.
    pub fn adopt_lease(&mut self, lease: PtyLease) {
        self.lease = lease;
    }

    pub fn drain_intents(&mut self) -> Vec<PtyIntent> {
        std::mem::take(&mut self.intents)
    }

    /// Route one TUI event. Everything is consumed while attached except
    /// events with no PTY meaning (focus, mouse, resume).
    pub fn handle(&mut self, event: &Event) -> Outcome {
        self.handle_at(event, Instant::now())
    }

    /// [`PtyAttach::handle`] with an explicit clock (deterministic tests).
    pub fn handle_at(&mut self, event: &Event, now: Instant) -> Outcome {
        match event {
            Event::Key(key) if key.kind != KeyEventKind::Release => self.note_human_input(now),
            Event::Paste(_) => self.note_human_input(now),
            _ => {}
        }
        match event {
            Event::Key(key) => self.handle_key(key),
            Event::Paste(text) => {
                self.queue_write(text.as_bytes());
                Outcome::Consumed
            }
            Event::Resize { width, height } => {
                let (rows, cols) = viewport(*width, *height);
                if (rows, cols) != (self.rows, self.cols) {
                    self.rows = rows;
                    self.cols = cols;
                    self.term.screen_mut().set_size(rows, cols);
                    self.intents.push(PtyIntent::Resize { rows, cols });
                }
                Outcome::Redraw
            }
            _ => Outcome::Ignored,
        }
    }

    fn handle_key(&mut self, key: &KeyEvent) -> Outcome {
        if key.kind == KeyEventKind::Release {
            return Outcome::Consumed;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char(']') {
            self.intents.push(PtyIntent::Detach);
            return Outcome::Consumed;
        }
        if let Some(bytes) = encode_key(key) {
            self.queue_write(&bytes);
        }
        Outcome::Consumed
    }

    fn queue_write(&mut self, bytes: &[u8]) {
        for chunk in utf8_chunks(bytes, MAX_WRITE_BYTES) {
            self.intents.push(PtyIntent::Write {
                bytes: chunk.to_vec(),
                epoch: self.lease.epoch,
                // Sequence is consumed when the host reports acceptance; the
                // host executes intents in order, so queue position offsets it.
                sequence: self.lease.next_sequence + self.pending_writes() as u64,
            });
        }
    }

    fn pending_writes(&self) -> usize {
        self.intents
            .iter()
            .filter(|intent| matches!(intent, PtyIntent::Write { .. }))
            .count()
    }

    /// Fold an engine output snapshot (the whole retained tail, possibly
    /// window-shifted) and return the NEW completed, sanitized lines.
    pub fn ingest_snapshot(&mut self, snapshot: &str) -> Vec<String> {
        let delta = snapshot_delta(&self.last_snapshot, snapshot).to_string();
        self.term.process(delta.as_bytes());
        self.last_snapshot = tail_bytes(snapshot, OVERLAP_PROBE * 32).to_string();
        let mut text = std::mem::take(&mut self.partial);
        text.push_str(&delta);
        let (lines, partial) = split_terminal_text(&text);
        self.partial = partial;
        lines
    }

    /// Flush the unfinished line (child exited or user detached).
    pub fn flush_partial(&mut self) -> Option<String> {
        let line = std::mem::take(&mut self.partial);
        (!line.trim().is_empty()).then_some(line)
    }
}

/// Map a vt100 color onto a ratatui color.
fn map_color(color: vt100::Color) -> ratatui::style::Color {
    match color {
        vt100::Color::Default => ratatui::style::Color::Reset,
        vt100::Color::Idx(n) => ratatui::style::Color::Indexed(n),
        vt100::Color::Rgb(r, g, b) => ratatui::style::Color::Rgb(r, g, b),
    }
}

/// Paint `screen` into `buf` over `area` (clipped to the grid), with colors
/// and attributes. Returns the absolute cursor position when it is visible.
pub fn render_grid(
    screen: &vt100::Screen,
    buf: &mut ratatui::buffer::Buffer,
    area: ratatui::layout::Rect,
) -> Option<(u16, u16)> {
    use ratatui::style::{Modifier, Style};
    let (rows, cols) = screen.size();
    for row in 0..rows.min(area.height) {
        for col in 0..cols.min(area.width) {
            let Some(cell) = screen.cell(row, col) else {
                continue;
            };
            if cell.is_wide_continuation() {
                continue;
            }
            let mut style = Style::default()
                .fg(map_color(cell.fgcolor()))
                .bg(map_color(cell.bgcolor()));
            let mut modifier = Modifier::empty();
            if cell.bold() {
                modifier |= Modifier::BOLD;
            }
            if cell.italic() {
                modifier |= Modifier::ITALIC;
            }
            if cell.underline() {
                modifier |= Modifier::UNDERLINED;
            }
            if cell.inverse() {
                modifier |= Modifier::REVERSED;
            }
            style = style.add_modifier(modifier);
            let contents = cell.contents();
            let text: &str = if contents.is_empty() { " " } else { contents };
            buf.set_string(area.x + col, area.y + row, text, style);
        }
    }
    if screen.hide_cursor() {
        return None;
    }
    let (row, col) = screen.cursor_position();
    (row < area.height && col < area.width).then(|| (area.x + col, area.y + row))
}

/// Split on UTF-8 boundaries (when the input is valid UTF-8) so a multibyte
/// character is never torn across two writes; the engine requires UTF-8.
fn utf8_chunks(bytes: &[u8], max: usize) -> Vec<&[u8]> {
    let mut out = Vec::new();
    let mut rest = bytes;
    while !rest.is_empty() {
        let mut end = rest.len().min(max);
        while end < rest.len() && end > 0 && (rest[end] & 0xC0) == 0x80 {
            end -= 1;
        }
        if end == 0 {
            end = rest.len().min(max);
        }
        out.push(&rest[..end]);
        rest = &rest[end..];
    }
    out
}

fn viewport(width: u16, height: u16) -> (u16, u16) {
    (height.saturating_sub(CHROME_ROWS).max(1), width.max(1))
}

/// The portion of `current` that is new relative to `previous`.
fn snapshot_delta<'a>(previous: &str, current: &'a str) -> &'a str {
    if previous.is_empty() {
        return current;
    }
    if let Some(rest) = current.strip_prefix(previous) {
        return rest;
    }
    // The engine's tail window moved: find where our remembered tail ends.
    let probe = tail_bytes(previous, OVERLAP_PROBE);
    match current.rfind(probe) {
        Some(at) => &current[at + probe.len()..],
        // Wholly replaced (or overlap lost): treat everything as new.
        None => current,
    }
}

fn tail_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    &text[start..]
}

/// Split terminal output into completed lines and an unfinished tail,
/// interpreting `\r`, `\b` and dropping escape sequences and control bytes.
pub fn split_terminal_text(text: &str) -> (Vec<String>, String) {
    let mut lines = Vec::new();
    let mut line: Vec<char> = Vec::new();
    let mut cursor = 0usize;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => skip_escape(&mut chars),
            '\n' => {
                lines.push(line.drain(..).collect::<String>());
                cursor = 0;
            }
            '\r' => cursor = 0,
            '\x08' => cursor = cursor.saturating_sub(1),
            '\t' => {
                for _ in 0..4 {
                    put(&mut line, &mut cursor, ' ');
                }
            }
            c if c.is_control() => {}
            c => put(&mut line, &mut cursor, c),
        }
    }
    (lines, line.into_iter().collect())
}

fn put(line: &mut Vec<char>, cursor: &mut usize, c: char) {
    if *cursor < line.len() {
        line[*cursor] = c;
    } else {
        line.push(c);
    }
    *cursor += 1;
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        // CSI: parameters/intermediates then a final byte 0x40..=0x7e.
        Some('[') => {
            for c in chars.by_ref() {
                if ('\x40'..='\x7e').contains(&c) {
                    break;
                }
            }
        }
        // OSC / DCS / PM / APC: until BEL or ST (ESC \).
        Some(']' | 'P' | '^' | '_') => {
            while let Some(c) = chars.next() {
                if c == '\x07' {
                    break;
                }
                if c == '\x1b' {
                    chars.next();
                    break;
                }
            }
        }
        // Two-byte sequences (ESC 7, ESC =, charset selects consume one more).
        Some('(' | ')' | '*' | '+' | '#' | '%') => {
            chars.next();
        }
        _ => {}
    }
}

/// xterm-style encoding of a key press. `None` for keys with no byte form.
pub fn encode_key(key: &KeyEvent) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let modifier = 1 + u8::from(shift) + 2 * u8::from(alt) + 4 * u8::from(ctrl);
    let csi = |final_byte: char| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[1;{modifier}{final_byte}").into_bytes()
        } else {
            format!("\x1b[{final_byte}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if modifier > 1 {
            format!("\x1b[{n};{modifier}~").into_bytes()
        } else {
            format!("\x1b[{n}~").into_bytes()
        }
    };
    let bytes = match key.code {
        KeyCode::Char(c) => {
            let mut out = Vec::new();
            if alt {
                out.push(0x1b);
            }
            if ctrl {
                match c {
                    'a'..='z' | 'A'..='Z' => out.push(c.to_ascii_lowercase() as u8 & 0x1f),
                    '@' | ' ' => out.push(0),
                    '[' => out.push(0x1b),
                    '\\' => out.push(0x1c),
                    '^' => out.push(0x1e),
                    '_' | '/' => out.push(0x1f),
                    _ => return None,
                }
            } else {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
            out
        }
        KeyCode::Enter => b"\r".to_vec(),
        KeyCode::Tab => b"\t".to_vec(),
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => {
            if alt {
                vec![0x1b, 0x7f]
            } else {
                vec![0x7f]
            }
        }
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => csi('A'),
        KeyCode::Down => csi('B'),
        KeyCode::Right => csi('C'),
        KeyCode::Left => csi('D'),
        KeyCode::Home => csi('H'),
        KeyCode::End => csi('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let letter = (b'P' + (n - 1)) as char;
            if modifier > 1 {
                format!("\x1b[1;{modifier}{letter}").into_bytes()
            } else {
                format!("\x1bO{letter}").into_bytes()
            }
        }
        KeyCode::F(n @ 5..=12) => tilde(match n {
            5 => 15,
            6 => 17,
            7 => 18,
            8 => 19,
            9 => 20,
            10 => 21,
            11 => 23,
            _ => 24,
        }),
        _ => return None,
    };
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> Event {
        Event::Key(KeyEvent::new(code, modifiers))
    }

    fn attach() -> PtyAttach {
        let mut pty = PtyAttach::new(
            "sh-1",
            PtyLease {
                epoch: 3,
                next_sequence: 7,
            },
            100,
            30,
        );
        pty.drain_intents();
        pty
    }

    #[test]
    fn parses_pty_commands() {
        assert_eq!(
            parse_command("/pty attach sh-1"),
            Some(PtyCommand::Attach("sh-1".into()))
        );
        assert_eq!(parse_command("/pty detach"), Some(PtyCommand::Detach));
        assert_eq!(parse_command("/pty kill"), Some(PtyCommand::Kill));
        assert_eq!(parse_command("/pty"), None);
        assert_eq!(parse_command("/pty attach"), None);
        assert_eq!(parse_command("/pty attach a b"), None);
        assert_eq!(parse_command("/ptyx attach a"), None);
        assert_eq!(parse_command("hello /pty attach a"), None);
    }

    #[test]
    fn new_attach_queues_initial_resize_minus_chrome_row() {
        let mut pty = PtyAttach::new(
            "x",
            PtyLease {
                epoch: 1,
                next_sequence: 0,
            },
            120,
            40,
        );
        assert_eq!(pty.size(), (39, 120));
        assert_eq!(
            pty.drain_intents(),
            vec![PtyIntent::Resize {
                rows: 39,
                cols: 120
            }]
        );
    }

    #[test]
    fn keystrokes_become_fenced_writes_with_increasing_sequence() {
        let mut pty = attach();
        pty.handle(&key(KeyCode::Char('l'), KeyModifiers::NONE));
        pty.handle(&key(KeyCode::Char('s'), KeyModifiers::NONE));
        pty.handle(&key(KeyCode::Enter, KeyModifiers::NONE));
        let intents = pty.drain_intents();
        assert_eq!(
            intents,
            vec![
                PtyIntent::Write {
                    bytes: b"l".to_vec(),
                    epoch: 3,
                    sequence: 7
                },
                PtyIntent::Write {
                    bytes: b"s".to_vec(),
                    epoch: 3,
                    sequence: 8
                },
                PtyIntent::Write {
                    bytes: b"\r".to_vec(),
                    epoch: 3,
                    sequence: 9
                },
            ]
        );
    }

    #[test]
    fn accepted_write_advances_sequence_for_the_next_batch() {
        let mut pty = attach();
        pty.handle(&key(KeyCode::Char('a'), KeyModifiers::NONE));
        pty.drain_intents();
        pty.write_accepted();
        pty.handle(&key(KeyCode::Char('b'), KeyModifiers::NONE));
        assert_eq!(
            pty.drain_intents(),
            vec![PtyIntent::Write {
                bytes: b"b".to_vec(),
                epoch: 3,
                sequence: 8
            }]
        );
    }

    #[test]
    fn ctrl_bracket_detaches_and_is_not_forwarded() {
        let mut pty = attach();
        assert_eq!(
            pty.handle(&key(KeyCode::Char(']'), KeyModifiers::CONTROL)),
            Outcome::Consumed
        );
        assert_eq!(pty.drain_intents(), vec![PtyIntent::Detach]);
    }

    #[test]
    fn ctrl_c_is_forwarded_to_the_child_not_the_host() {
        let mut pty = attach();
        pty.handle(&key(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert_eq!(
            pty.drain_intents(),
            vec![PtyIntent::Write {
                bytes: vec![0x03],
                epoch: 3,
                sequence: 7
            }]
        );
    }

    #[test]
    fn key_release_is_swallowed() {
        let mut pty = attach();
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        assert_eq!(pty.handle(&Event::Key(release)), Outcome::Consumed);
        assert!(pty.drain_intents().is_empty());
    }

    #[test]
    fn resize_event_queues_one_resize_and_dedupes_same_size() {
        let mut pty = attach();
        assert_eq!(
            pty.handle(&Event::Resize {
                width: 80,
                height: 24
            }),
            Outcome::Redraw
        );
        assert_eq!(
            pty.drain_intents(),
            vec![PtyIntent::Resize { rows: 23, cols: 80 }]
        );
        pty.handle(&Event::Resize {
            width: 80,
            height: 24,
        });
        assert!(pty.drain_intents().is_empty());
        assert_eq!(pty.size(), (23, 80));
    }

    #[test]
    fn tiny_terminal_never_produces_zero_dimensions() {
        let mut pty = attach();
        pty.handle(&Event::Resize {
            width: 0,
            height: 0,
        });
        assert_eq!(
            pty.drain_intents(),
            vec![PtyIntent::Resize { rows: 1, cols: 1 }]
        );
    }

    #[test]
    fn paste_is_chunked_to_the_write_cap() {
        let mut pty = attach();
        pty.handle(&Event::Paste("x".repeat(MAX_WRITE_BYTES + 5)));
        let intents = pty.drain_intents();
        assert_eq!(intents.len(), 2);
        match (&intents[0], &intents[1]) {
            (
                PtyIntent::Write {
                    bytes: a,
                    sequence: 7,
                    ..
                },
                PtyIntent::Write {
                    bytes: b,
                    sequence: 8,
                    ..
                },
            ) => {
                assert_eq!(a.len(), MAX_WRITE_BYTES);
                assert_eq!(b.len(), 5);
            }
            other => panic!("unexpected intents {other:?}"),
        }
    }

    #[test]
    fn chunking_never_tears_a_multibyte_character() {
        let text = format!("{}{}", "a".repeat(MAX_WRITE_BYTES - 1), "\u{e9}\u{e9}");
        for chunk in utf8_chunks(text.as_bytes(), MAX_WRITE_BYTES) {
            assert!(std::str::from_utf8(chunk).is_ok());
        }
        assert_eq!(
            utf8_chunks(text.as_bytes(), MAX_WRITE_BYTES).concat(),
            text.as_bytes()
        );
    }

    #[test]
    fn key_encoding_matches_xterm() {
        let enc = |code, mods| encode_key(&KeyEvent::new(code, mods));
        assert_eq!(
            enc(KeyCode::Up, KeyModifiers::NONE),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            enc(KeyCode::Left, KeyModifiers::CONTROL),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            enc(KeyCode::Backspace, KeyModifiers::NONE),
            Some(vec![0x7f])
        );
        assert_eq!(
            enc(KeyCode::Char('x'), KeyModifiers::ALT),
            Some(vec![0x1b, b'x'])
        );
        assert_eq!(
            enc(KeyCode::Char('é'), KeyModifiers::NONE),
            Some("é".as_bytes().to_vec())
        );
        assert_eq!(
            enc(KeyCode::Delete, KeyModifiers::NONE),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            enc(KeyCode::F(1), KeyModifiers::NONE),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            enc(KeyCode::F(5), KeyModifiers::NONE),
            Some(b"\x1b[15~".to_vec())
        );
        assert_eq!(
            enc(KeyCode::BackTab, KeyModifiers::SHIFT),
            Some(b"\x1b[Z".to_vec())
        );
        assert_eq!(enc(KeyCode::Char('1'), KeyModifiers::CONTROL), None);
        assert_eq!(enc(KeyCode::Null, KeyModifiers::NONE), None);
    }

    #[test]
    fn sanitizer_strips_escapes_and_applies_cr_and_backspace() {
        let (lines, partial) = split_terminal_text(
            "\x1b[32mok\x1b[0m\r\nprogress 10%\rprogress 99%\r\nab\x08c\x1b]0;title\x07\r\nprompt$ ",
        );
        assert_eq!(lines, vec!["ok", "progress 99%", "ac"]);
        assert_eq!(partial, "prompt$ ");
    }

    #[test]
    fn snapshots_yield_only_new_lines_and_keep_the_partial_prompt() {
        let mut pty = attach();
        assert_eq!(pty.ingest_snapshot("one\ntwo\npro"), vec!["one", "two"]);
        assert_eq!(pty.partial_line(), "pro");
        assert_eq!(
            pty.ingest_snapshot("one\ntwo\nprompt> "),
            Vec::<String>::new()
        );
        assert_eq!(pty.partial_line(), "prompt> ");
        assert_eq!(
            pty.ingest_snapshot("one\ntwo\nprompt> ls\nfile\n"),
            vec!["prompt> ls", "file"]
        );
        assert_eq!(pty.partial_line(), "");
    }

    #[test]
    fn shifted_tail_window_does_not_replay_old_lines() {
        let mut pty = attach();
        let first: String = (0..40).map(|i| format!("line-{i:03}\n")).collect();
        assert_eq!(pty.ingest_snapshot(&first).len(), 40);
        // Engine window dropped the oldest lines and appended two new ones.
        let shifted: String = (10..42).map(|i| format!("line-{i:03}\n")).collect();
        assert_eq!(pty.ingest_snapshot(&shifted), vec!["line-040", "line-041"]);
    }

    #[test]
    fn flush_partial_returns_unfinished_line_once() {
        let mut pty = attach();
        pty.ingest_snapshot("done? ");
        assert_eq!(pty.flush_partial(), Some("done? ".to_string()));
        assert_eq!(pty.flush_partial(), None);
    }

    fn writes(pty: &mut PtyAttach) -> Vec<Vec<u8>> {
        pty.drain_intents()
            .into_iter()
            .filter_map(|i| match i {
                PtyIntent::Write { bytes, .. } => Some(bytes),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn agent_types_while_the_human_is_idle() {
        let mut pty = attach();
        let t0 = Instant::now();
        assert!(pty.agent_lease_held());
        assert_eq!(pty.agent_write_at(b"ls\r", t0), AgentInput::Sent);
        assert_eq!(writes(&mut pty), vec![b"ls\r".to_vec()]);
    }

    #[test]
    fn human_keypress_releases_the_agent_lease_immediately() {
        let mut pty = attach();
        let t0 = Instant::now();
        pty.handle_at(&key(KeyCode::Char('h'), KeyModifiers::NONE), t0);
        assert!(!pty.agent_lease_held());
        assert!(pty.take_agent_release_notice());
        assert!(!pty.take_agent_release_notice(), "notice is one-shot");
        pty.drain_intents();
        // The agent's input in the same instant is held back, not written.
        assert_eq!(pty.agent_write_at(b"rm x\r", t0), AgentInput::Queued);
        assert!(writes(&mut pty).is_empty());
        assert_eq!(pty.queued_agent_bytes(), 5);
    }

    #[test]
    fn agent_input_stays_queued_until_the_human_is_idle_then_flushes_in_order() {
        let mut pty = attach();
        let t0 = Instant::now();
        pty.handle_at(&key(KeyCode::Char('h'), KeyModifiers::NONE), t0);
        pty.drain_intents();
        let mid = t0 + HUMAN_IDLE / 2;
        assert_eq!(pty.agent_write_at(b"a", mid), AgentInput::Queued);
        // A further human key restarts the idle clock.
        pty.handle_at(&key(KeyCode::Char('i'), KeyModifiers::NONE), mid);
        pty.drain_intents();
        assert_eq!(
            pty.agent_write_at(b"b", t0 + HUMAN_IDLE),
            AgentInput::Queued
        );
        assert!(writes(&mut pty).is_empty());
        // Idle long enough since the last keypress: the lease returns and the queue drains.
        let later = mid + HUMAN_IDLE;
        pty.poll_agent_at(later);
        assert!(pty.agent_lease_held());
        assert_eq!(writes(&mut pty), vec![b"a".to_vec(), b"b".to_vec()]);
        assert_eq!(pty.agent_write_at(b"c", later), AgentInput::Sent);
    }

    #[test]
    fn human_paste_releases_but_key_release_does_not() {
        let mut pty = attach();
        let t0 = Instant::now();
        let mut release = KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE);
        release.kind = KeyEventKind::Release;
        pty.handle_at(&Event::Key(release), t0);
        assert!(
            pty.agent_lease_held(),
            "a key release is not a human keypress"
        );
        pty.handle_at(&Event::Paste("p".into()), t0);
        assert!(!pty.agent_lease_held());
    }

    #[test]
    fn agent_queue_is_bounded_and_refuses_overflow() {
        let mut pty = attach();
        let t0 = Instant::now();
        pty.handle_at(&key(KeyCode::Char('h'), KeyModifiers::NONE), t0);
        let big = vec![b'x'; MAX_AGENT_QUEUE_BYTES];
        assert_eq!(pty.agent_write_at(&big, t0), AgentInput::Queued);
        assert_eq!(pty.agent_write_at(b"y", t0), AgentInput::Refused);
    }

    #[test]
    fn adopting_a_new_lease_fences_later_writes() {
        let mut pty = attach();
        pty.adopt_lease(PtyLease {
            epoch: 4,
            next_sequence: 0,
        });
        pty.handle(&key(KeyCode::Char('z'), KeyModifiers::NONE));
        assert_eq!(
            pty.drain_intents(),
            vec![PtyIntent::Write {
                bytes: b"z".to_vec(),
                epoch: 4,
                sequence: 0
            }]
        );
    }

    fn grid(pty: &PtyAttach) -> String {
        pty.screen().contents()
    }

    #[test]
    fn cursor_addressing_and_clear_paint_a_full_screen_grid() {
        let mut pty = attach();
        pty.feed_raw(b"[2J[1;1Hhead[3;5Hbody[2;1Hmid");
        assert_eq!(grid(&pty), "head\nmid\n    body");
        assert_eq!(pty.screen().cursor_position(), (1, 3));
    }

    #[test]
    fn alternate_screen_is_isolated_and_restored() {
        let mut pty = attach();
        pty.feed_raw(b"shell prompt$");
        pty.feed_raw(b"[?1049h[2J[1;1Hvim buffer");
        assert_eq!(grid(&pty), "vim buffer");
        assert!(pty.screen().alternate_screen());
        pty.feed_raw(b"[?1049l");
        assert_eq!(grid(&pty), "shell prompt$");
    }

    #[test]
    fn snapshot_ingest_feeds_the_emulator_without_replaying_old_output() {
        let mut pty = attach();
        pty.ingest_snapshot("[2J[1;1Hone");
        pty.ingest_snapshot("[2J[1;1Hone[2;1Htwo");
        assert_eq!(grid(&pty), "one\ntwo");
    }

    #[test]
    fn colors_attributes_and_cursor_reach_the_buffer() {
        use ratatui::{
            buffer::Buffer,
            layout::Rect,
            style::{Color, Modifier},
        };
        let mut pty = attach();
        pty.feed_raw(b"[1;31mR[0m[38;2;1;2;3;48;5;200mX[7mI[0m[1;3H");
        let area = Rect::new(2, 1, 20, 5);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 8));
        let cursor = render_grid(pty.screen(), &mut buf, area);
        assert_eq!(cursor, Some((4, 1)));
        let red = &buf[(2, 1)];
        assert_eq!(red.symbol(), "R");
        assert!(red.modifier.contains(Modifier::BOLD));
        let truecolor = &buf[(3, 1)];
        assert_eq!(truecolor.fg, Color::Rgb(1, 2, 3));
        assert_eq!(truecolor.bg, Color::Indexed(200));
        assert!(buf[(4, 1)].modifier.contains(Modifier::REVERSED));
        pty.feed_raw(b"[?25l");
        assert_eq!(render_grid(pty.screen(), &mut buf, area), None);
    }

    #[test]
    fn resize_resizes_the_emulator_with_the_pty() {
        let mut pty = attach();
        assert_eq!(pty.screen().size(), (29, 100));
        pty.handle(&Event::Resize {
            width: 80,
            height: 24,
        });
        assert_eq!(pty.screen().size(), (23, 80));
        assert_eq!(
            pty.drain_intents(),
            vec![PtyIntent::Resize { rows: 23, cols: 80 }]
        );
    }
}
