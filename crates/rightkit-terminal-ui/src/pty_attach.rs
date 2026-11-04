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
            term: vt100::Parser::new(nonzero(rows), nonzero(cols), 0),
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
                    self.term
                        .screen_mut()
                        .set_size(nonzero(rows), nonzero(cols));
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

/// vt100 0.18 sizes are non-zero; a zero request becomes the smallest grid.
fn nonzero(n: u16) -> std::num::NonZeroU16 {
    std::num::NonZeroU16::new(n).unwrap_or(std::num::NonZeroU16::MIN)
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
    let (rows, cols) = (rows.get(), cols.get());
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
