//! Push-to-talk policy and start/end handoff.
//!
//! Ported from HeardRight `heardright_core::ptt_policy`: pure decisions plus [`PttHandoff`], the
//! atomic state machine that defers a stop/send/cancel arriving while the start callback is still
//! running and guarantees exactly one caller owns end dispatch once recording is published.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};

/// Keep optimistic acknowledgement ahead of backend work. Backend state/error
/// remains authoritative after action completes.
pub fn run_feedback_before_action<T>(
    feedback: impl FnOnce(),
    action: impl FnOnce() -> T,
) -> T {
    feedback();
    action()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PttAbortKind {
    KeyDown,
    ModifierJoined,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PttPhase {
    Arming,
    Starting,
    Recording,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PttModifiers(pub u8);

impl PttModifiers {
    pub const CTRL: u8 = 1 << 0;
    pub const SHIFT: u8 = 1 << 1;
    pub const ALT: u8 = 1 << 2;
    pub const COMMAND_OR_WIN: u8 = 1 << 3;
    pub const FN: u8 = 1 << 4;

    pub fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PttAbortCause {
    pub kind: PttAbortKind,
    pub key_code: Option<u32>,
    pub modifiers: PttModifiers,
    pub joined: PttModifiers,
    pub phase: PttPhase,
    pub swallowed: bool,
}

pub fn format_abort(platform: &str, cause: PttAbortCause) -> String {
    let flags = |value: PttModifiers| {
        format!(
            "ctrl:{},shift:{},alt:{},command_or_win:{},fn:{}",
            value.contains(PttModifiers::CTRL),
            value.contains(PttModifiers::SHIFT),
            value.contains(PttModifiers::ALT),
            value.contains(PttModifiers::COMMAND_OR_WIN),
            value.contains(PttModifiers::FN),
        )
    };
    format!(
        "platform={platform} kind={} key_code={} modifiers={} joined={} phase={} swallowed={}",
        match cause.kind {
            PttAbortKind::KeyDown => "key_down",
            PttAbortKind::ModifierJoined => "modifier_joined",
        },
        cause
            .key_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "none".into()),
        flags(cause.modifiers),
        flags(cause.joined),
        match cause.phase {
            PttPhase::Arming => "arming",
            PttPhase::Starting => "starting",
            PttPhase::Recording => "recording",
        },
        cause.swallowed,
    )
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PttEnd {
    None = 0,
    Stop = 1,
    Send = 2,
    Cancel = 3,
}

pub struct PttHandoff {
    state: AtomicU8,
    abort: AtomicU64,
}

impl Default for PttHandoff {
    fn default() -> Self {
        Self::new()
    }
}

impl PttHandoff {
    const IDLE: u8 = 0;
    const STARTING: u8 = 1;
    const RECORDING: u8 = 5;

    pub const fn new() -> Self {
        Self {
            state: AtomicU8::new(Self::IDLE),
            abort: AtomicU64::new(0),
        }
    }

    pub fn begin_start(&self) -> bool {
        self.state
            .compare_exchange(
                Self::IDLE,
                Self::STARTING,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    pub fn is_active(&self) -> bool {
        self.state.load(Ordering::SeqCst) != Self::IDLE
    }

    pub fn reset(&self) {
        self.state.store(Self::IDLE, Ordering::SeqCst);
        self.abort.store(0, Ordering::SeqCst);
    }

    pub fn phase(&self) -> Option<PttPhase> {
        match self.state.load(Ordering::SeqCst) {
            Self::STARTING..=4 => Some(PttPhase::Starting),
            Self::RECORDING..=8 => Some(PttPhase::Recording),
            _ => None,
        }
    }

    /// While start callback runs, defer stop/cancel. Once recording is
    /// published, exactly one caller owns callback dispatch.
    pub fn request_end(
        &self,
        end: PttEnd,
        cause: Option<PttAbortCause>,
    ) -> Option<(PttEnd, Option<PttAbortCause>)> {
        if end == PttEnd::Cancel {
            if let Some(cause) = cause {
                self.abort.store(encode_abort(cause), Ordering::SeqCst);
            }
        }
        loop {
            let state = self.state.load(Ordering::SeqCst);
            match state {
                Self::IDLE => {
                    self.abort.store(0, Ordering::SeqCst);
                    return None;
                }
                Self::STARTING..=4 => {
                    let pending = (state - Self::STARTING).max(end as u8);
                    if self
                        .state
                        .compare_exchange(
                            state,
                            Self::STARTING + pending,
                            Ordering::SeqCst,
                            Ordering::SeqCst,
                        )
                        .is_ok()
                    {
                        return None;
                    }
                }
                Self::RECORDING..=8 => {
                    let pending = (state - Self::RECORDING).max(end as u8);
                    if self
                        .state
                        .compare_exchange(state, Self::IDLE, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        let end = Self::decode_end(pending);
                        return Some((end, self.take_abort(end)));
                    }
                }
                _ => self.reset(),
            }
        }
    }

    /// Publish completion only after start callback returns. Any release/chord
    /// received meanwhile becomes one immediate end callback.
    pub fn complete_start(
        &self,
        still_valid: bool,
    ) -> Option<(PttEnd, Option<PttAbortCause>)> {
        loop {
            let state = self.state.load(Ordering::SeqCst);
            if !(Self::STARTING..=4).contains(&state) {
                return None;
            }
            let mut pending = state - Self::STARTING;
            if pending == PttEnd::None as u8 && !still_valid {
                pending = PttEnd::Stop as u8;
            }
            let next = if pending == PttEnd::None as u8 {
                Self::RECORDING
            } else {
                Self::IDLE
            };
            if self
                .state
                .compare_exchange(state, next, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                if pending == PttEnd::None as u8 {
                    return None;
                }
                let end = Self::decode_end(pending);
                return Some((end, self.take_abort(end)));
            }
        }
    }

    fn decode_end(value: u8) -> PttEnd {
        match value {
            1 => PttEnd::Stop,
            2 => PttEnd::Send,
            3 => PttEnd::Cancel,
            _ => PttEnd::None,
        }
    }

    fn take_abort(&self, end: PttEnd) -> Option<PttAbortCause> {
        if end != PttEnd::Cancel {
            return None;
        }
        decode_abort(self.abort.swap(0, Ordering::SeqCst))
    }
}

fn encode_abort(cause: PttAbortCause) -> u64 {
    let kind = matches!(cause.kind, PttAbortKind::ModifierJoined) as u64;
    let phase = match cause.phase {
        PttPhase::Arming => 0,
        PttPhase::Starting => 1,
        PttPhase::Recording => 2,
    };
    1 | (kind << 1)
        | ((phase as u64) << 2)
        | ((cause.swallowed as u64) << 4)
        | ((cause.modifiers.0 as u64) << 5)
        | ((cause.joined.0 as u64) << 10)
        | ((cause.key_code.is_some() as u64) << 15)
        | ((cause.key_code.unwrap_or_default() as u64) << 16)
}

fn decode_abort(value: u64) -> Option<PttAbortCause> {
    if value & 1 == 0 {
        return None;
    }
    Some(PttAbortCause {
        kind: if value & (1 << 1) != 0 {
            PttAbortKind::ModifierJoined
        } else {
            PttAbortKind::KeyDown
        },
        phase: match (value >> 2) & 0b11 {
            1 => PttPhase::Starting,
            2 => PttPhase::Recording,
            _ => PttPhase::Arming,
        },
        swallowed: value & (1 << 4) != 0,
        modifiers: PttModifiers(((value >> 5) & 0x1f) as u8),
        joined: PttModifiers(((value >> 10) & 0x1f) as u8),
        key_code: (value & (1 << 15) != 0).then_some((value >> 16) as u32),
    })
}

pub fn should_arm_ptt(enabled: bool, dictation_active: bool, other_mods: bool) -> bool {
    enabled && !dictation_active && !other_mods
}

/// A second key only disambiguates PTT from an ordinary modifier chord near
/// initial press. Once hold is established, later typing must not discard
/// captured speech.
pub const PTT_CHORD_ABORT_WINDOW_MS: u64 = 1_000;

pub fn should_abort_ptt_chord(hold_elapsed_ms: u64) -> bool {
    hold_elapsed_ms < PTT_CHORD_ABORT_WINDOW_MS
}

/// While PTT remains physically held after an in-session command ends capture,
/// ignore modifier resync caused by dispatched shortcuts. Only a clean Ctrl-up
/// event ends suppression.
pub fn should_clear_ptt_hold_suppression(ctrl_down: bool, other_mods: bool) -> bool {
    !ctrl_down && !other_mods
}

/// Any suppressing fn chord must be backed by both the modifier bit on the key
/// event and a preceding fn FLAGS_CHANGED transition observed by this tap.
/// This rejects stale/synthetic Space or Escape events that inherit SecondaryFn.
pub fn should_accept_fn_chord(
    event_has_fn: bool,
    cached_fn_down: bool,
    enabled: bool,
) -> bool {
    event_has_fn && cached_fn_down && enabled
}

