// ENG-91 — Terminal-idle detection & stream sanitization.
//
// Two independent, observe-only building blocks for anything that streams
// raw terminal byte output (interactive terminals, PTY-backed sessions):
//
// 1. [`AnsiStreamSanitizer`] strips CSI (Control Sequence Introducer) and
//    OSC (Operating System Command) escape sequences from a byte stream,
//    correctly carrying an in-progress sequence across chunk/read
//    boundaries instead of corrupting output or silently dropping bytes.
// 2. [`PromptMarkerIdleDetector`] determines "the shell is truly idle"
//    by watching for a caller-owned, unique sentinel string written to the
//    shell prompt — never by polling for N seconds of silence, which is
//    indistinguishable from a slow-but-still-running command.
//
// Neither type spawns a process or owns a socket; both are pure, allocation
// -light state machines fed by whoever is reading the terminal's output.

/// Streaming sanitizer that removes CSI/OSC escape sequences from terminal
/// output while correctly handling sequences split across separate `feed`
/// calls (i.e. across read-buffer boundaries).
///
/// # Recognized sequences
/// - CSI: `ESC '[' <parameter bytes 0x30..=0x3F> <intermediate bytes
///   0x20..=0x2F> <final byte 0x40..=0x7E>` — stripped entirely.
/// - OSC: `ESC ']' ... (BEL | ESC '\\')` — stripped entirely, terminated by
///   either BEL (0x07) or the two-byte String Terminator `ESC \`.
/// - A bare ESC not followed by `[` or `]` is treated as a (non-CSI/OSC)
///   escape whose meaning this sanitizer does not know; it is dropped along
///   with the one byte that follows, matching the "strip escape noise" goal
///   without misinterpreting arbitrary control codes as printable text.
///
/// Any byte outside these sequences passes through unchanged, including
/// other control characters (e.g. `\n`, `\r`, `\t`) which callers may still
/// want for their own idle/prompt-detection logic.
#[derive(Debug, Default, Clone)]
pub struct AnsiStreamSanitizer {
    state: SanitizerState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SanitizerState {
    /// Not currently inside any escape sequence.
    Normal,
    /// Saw ESC (0x1B); waiting on the next byte to classify the sequence.
    SawEsc,
    /// Inside a CSI sequence (`ESC [ ...`); waiting for the final byte.
    InCsi,
    /// Inside an OSC sequence (`ESC ] ...`); waiting for BEL or `ESC \`.
    InOsc,
    /// Inside an OSC sequence and just saw ESC, checking for the `\` of ST.
    InOscSawEsc,
    /// Saw a bare ESC not followed by `[` or `]`; the single following byte
    /// (whatever it is) completes this two-byte sequence and is dropped.
    #[allow(dead_code)]
    SawEscUnknown,
}

impl Default for SanitizerState {
    fn default() -> Self {
        SanitizerState::Normal
    }
}

const ESC: u8 = 0x1B;
const BEL: u8 = 0x07;

impl AnsiStreamSanitizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed the next chunk of raw terminal bytes and return the sanitized
    /// bytes (CSI/OSC sequences removed). Any escape sequence left
    /// incomplete at the end of `chunk` is remembered internally and
    /// resolved (and still stripped) once the rest of it arrives in a later
    /// `feed` call — sequences are never corrupted or leaked into the output
    /// just because a read happened to split them.
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len());
        for &byte in chunk {
            match self.state {
                SanitizerState::Normal => {
                    if byte == ESC {
                        self.state = SanitizerState::SawEsc;
                    } else {
                        out.push(byte);
                    }
                }
                SanitizerState::SawEsc => match byte {
                    b'[' => self.state = SanitizerState::InCsi,
                    b']' => self.state = SanitizerState::InOsc,
                    _ => {
                        // Unknown two-byte escape: drop this byte too, then
                        // resume normal streaming.
                        self.state = SanitizerState::Normal;
                    }
                },
                SanitizerState::SawEscUnknown => {
                    // Unreachable: SawEsc resolves unknown escapes in one
                    // step above. Kept only so the enum stays exhaustive if
                    // that ever changes.
                    self.state = SanitizerState::Normal;
                    if byte == ESC {
                        self.state = SanitizerState::SawEsc;
                    } else {
                        out.push(byte);
                    }
                }
                SanitizerState::InCsi => {
                    // Parameter bytes 0x30-0x3F, intermediate 0x20-0x2F,
                    // final byte 0x40-0x7E ends the sequence.
                    if (0x40..=0x7E).contains(&byte) {
                        self.state = SanitizerState::Normal;
                    }
                    // else: still inside the CSI sequence, keep consuming.
                }
                SanitizerState::InOsc => {
                    if byte == BEL {
                        self.state = SanitizerState::Normal;
                    } else if byte == ESC {
                        self.state = SanitizerState::InOscSawEsc;
                    }
                    // else: still inside the OSC payload.
                }
                SanitizerState::InOscSawEsc => {
                    if byte == b'\\' {
                        // `ESC \` = String Terminator; OSC sequence ends.
                        self.state = SanitizerState::Normal;
                    } else {
                        // Not a valid ST; treat the ESC as still inside the
                        // OSC payload and re-evaluate this byte from there.
                        self.state = SanitizerState::InOsc;
                        if byte == BEL {
                            self.state = SanitizerState::Normal;
                        } else if byte == ESC {
                            self.state = SanitizerState::InOscSawEsc;
                        }
                    }
                }
            }
        }
        out
    }

    /// Whether a sequence is currently in progress (i.e. carried state exists
    /// that has not yet been resolved by a terminating byte). Useful for
    /// diagnostics/tests; not required for correct streaming use.
    pub fn has_pending_sequence(&self) -> bool {
        self.state != SanitizerState::Normal
    }
}

/// Detects true shell idle by watching for a unique, caller-owned prompt
/// marker rather than polling for a period of output silence.
///
/// The caller is responsible for actually causing the marker to appear at
/// the shell prompt (e.g. by setting `PS1`/`PROMPT_COMMAND` to emit it, or by
/// echoing it as a command-separator sentinel). This type only recognizes
/// the marker once it shows up in the byte stream; it never guesses idleness
/// from elapsed time, because a slow-but-still-running command is
/// indistinguishable from a truly idle shell under a silence timeout.
#[derive(Debug, Clone)]
pub struct PromptMarkerIdleDetector {
    marker: String,
    /// Tail of not-yet-matched output, bounded to `marker.len() - 1` bytes,
    /// so a marker split across two `feed` calls is still recognized.
    carry: String,
    idle: bool,
}

impl PromptMarkerIdleDetector {
    /// Build a detector for `marker`. Panics on an empty marker: an empty
    /// sentinel would match everywhere and defeat the whole purpose.
    pub fn new(marker: impl Into<String>) -> Self {
        let marker = marker.into();
        assert!(
            !marker.is_empty(),
            "PromptMarkerIdleDetector requires a non-empty marker"
        );
        Self {
            marker,
            carry: String::new(),
            idle: false,
        }
    }

    /// Generate a unique prompt marker for a session, combining a timestamp
    /// with a monotonic in-process counter so it cannot plausibly collide
    /// with real command output, even across two calls in the same instant.
    pub fn generate_marker(session_id: &str) -> String {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
        format!("__RIGHTKIT_PROMPT_{session_id}_{nanos:x}_{seq:x}__")
    }

    /// The marker this detector is watching for.
    pub fn marker(&self) -> &str {
        &self.marker
    }

    /// Feed the next chunk of (already UTF-8, ideally ANSI-sanitized) output.
    /// Returns `true` the moment the marker is first observed; once idle,
    /// stays idle until [`Self::reset`] is called for the next command.
    pub fn feed(&mut self, chunk: &str) -> bool {
        if self.idle {
            return true;
        }
        self.carry.push_str(chunk);
        if self.carry.contains(&self.marker) {
            self.idle = true;
            self.carry.clear();
            return true;
        }
        // Bound the carry buffer: only the trailing (marker.len() - 1) bytes
        // could ever combine with future input to complete the marker.
        let max_carry = self.marker.len().saturating_sub(1);
        if self.carry.len() > max_carry {
            let trim_at = self.carry.len() - max_carry;
            // Trim at a char boundary at or before `trim_at` to keep the
            // carry buffer valid UTF-8.
            let mut boundary = trim_at;
            while boundary > 0 && !self.carry.is_char_boundary(boundary) {
                boundary -= 1;
            }
            self.carry.drain(..boundary);
        }
        false
    }

    /// True once the owned prompt marker has been observed. Never becomes
    /// true from elapsed time or output silence alone.
    pub fn is_idle(&self) -> bool {
        self.idle
    }

    /// Reset for the next command: clears the idle flag and any partial
    /// carry so a previous command's tail cannot be mistaken for progress on
    /// the next one.
    pub fn reset(&mut self) {
        self.idle = false;
        self.carry.clear();
    }
}
