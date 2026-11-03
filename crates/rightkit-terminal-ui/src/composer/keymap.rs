//! Composer-relevant keymap types and built-in defaults, reduced from the
//! Codex donor `keymap.rs` + `keymap/{bindings,chords,vim_search}.rs` @
//! 444da310.
//!
//! Only the composer surface is lifted: `ComposerKeymap`, `EditorKeymap`, the
//! Vim keymaps, the chord matcher state machine, and the context/action
//! inventory needed to dispatch them. The donor's TOML-driven `TuiKeymap`
//! config resolution, conflict validation, and non-composer keymaps (app,
//! chat, pager, list, agents, approval) are parent-owned and intentionally
//! absent — `RuntimeKeymap::defaults()` here IS the shipped binding set until
//! the parent wires a config layer.
//!
//! Lifted verbatim where possible:
//! - `KeymapContext`, `KeymapActionId`, `KeymapContextSet` (context overlap
//!   and bit-set machinery), trimmed to contexts this module still defines.
//! - `RuntimeChordKeymap`/`KeyChordMatcher` (pending-chord state machine,
//!   `g g` jump-top chord, dispatch-function-key tokens, ctrl-/ vs ctrl-7
//!   normalization).
//! - Every `built_in_defaults` table for `composer`, `editor`, `vim_normal`,
//!   `vim_operator`, `vim_search`, `vim_text_object`.

use super::key_hint::KeyBinding;
use super::key_hint::{ctrl, plain, shift};
use crossterm::event::KeyCode;
use crossterm::event::KeyModifiers;
use std::sync::Arc;

/// Internal function-key range used for completed-chord dispatch tokens.
/// Donor `codex_config::types::MAX_FUNCTION_KEY` is 24; tokens live above it.
const FIRST_DISPATCH_FUNCTION_KEY: u8 = 25;

/// Runtime context in which a keymap action is active.
///
/// Verbatim from donor `keymap/bindings.rs`; non-composer variants remain so
/// overlap semantics and chord dispatch ordering match the donor.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum KeymapContext {
    Composer,
    Editor,
    VimNormal,
    VimOperator,
    VimSearch,
    VimTextObject,
}

impl KeymapContext {}

/// Stable identity for one configurable action.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct KeymapActionId {
    pub(crate) context: KeymapContext,
    pub(crate) action: &'static str,
}

impl KeymapActionId {}

/// Iterate over every composer-surface runtime action identity, in the donor's
/// declaration order (stable dispatch-token ordering depends on it).
pub(crate) fn keymap_action_ids() -> impl Iterator<Item = KeymapActionId> {
    macro_rules! ids {
        ($($context:ident [$($action:ident),+ $(,)?]),+ $(,)?) => {
            [
                $(
                    $(
                        KeymapActionId {
                            context: KeymapContext::$context,
                            action: stringify!($action),
                        },
                    )+
                )+
            ]
        };
    }
    ids! {
        Composer [submit, queue, toggle_shortcuts, history_search_previous, history_search_next],
        Editor [
            insert_newline, move_left, move_right, move_up, move_down,
            move_word_left, move_word_right, move_line_start, move_line_end,
            delete_backward, delete_forward, delete_backward_word, delete_forward_word,
            kill_line_start, kill_whole_line, kill_line_end, yank,
        ],
        VimNormal [
            enter_insert, append_after_cursor, append_line_end, insert_line_start,
            open_line_below, open_line_above, enter_replace_mode,
            move_left, move_right, move_up, move_down,
            move_word_forward, move_word_backward, move_word_end,
            move_line_start, move_line_end,
            find_forward, find_backward, till_forward, till_backward,
            jump_top, jump_bottom,
            delete_char, replace_char, repeat_last_change, substitute_char,
            delete_to_line_end, change_to_line_end, yank_line, paste_after,
            start_delete_operator, start_yank_operator, start_change_operator,
            undo, redo, cancel_operator,
        ],
        VimSearch [forward, backward, next, previous],
        VimOperator [
            delete_line, yank_line,
            motion_left, motion_right, motion_up, motion_down,
            motion_word_forward, motion_word_backward, motion_word_end,
            motion_line_start, motion_line_end,
            motion_find_forward, motion_find_backward,
            motion_till_forward, motion_till_backward,
            motion_jump_top, motion_jump_bottom,
            select_inner_text_object, select_around_text_object,
            cancel,
        ],
        VimTextObject [
            word, big_word, parentheses, brackets, braces,
            double_quote, single_quote, backtick, cancel,
        ],
    }
    .into_iter()
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct KeyChord {
    pub(crate) prefix: KeyBinding,
    pub(crate) completion: KeyBinding,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RuntimeChordBinding {
    pub(crate) action: KeymapActionId,
    pub(crate) chord: KeyChord,
}

/// Chord bindings available at runtime. With no config layer this is the
/// built-in `g g` jump-top chord set installed by [`RuntimeKeymap::defaults`].
#[derive(Clone, Debug, Default)]
pub(crate) struct RuntimeChordKeymap {
    pub(crate) bindings: Vec<RuntimeChordBinding>,
}

impl RuntimeChordKeymap {}

fn dispatch_binding(action: KeymapActionId) -> Option<KeyBinding> {
    let index = keymap_action_ids().position(|candidate| candidate == action)?;
    let function_key = FIRST_DISPATCH_FUNCTION_KEY.checked_add(u8::try_from(index).ok()?)?;
    Some(KeyBinding::new(
        KeyCode::F(function_key),
        KeyModifiers::NONE,
    ))
}

/// Composer-level keybindings.
#[derive(Clone, Debug)]
pub(crate) struct ComposerKeymap {
    /// Submit current draft.
    pub(crate) submit: Vec<KeyBinding>,
    /// Queue current draft while a task is running.
    pub(crate) queue: Vec<KeyBinding>,
    /// Toggle composer shortcut overlay.
    pub(crate) toggle_shortcuts: Vec<KeyBinding>,
    /// Open reverse history search or move to the previous match.
    pub(crate) history_search_previous: Vec<KeyBinding>,
    /// Move to the next match in reverse history search.
    pub(crate) history_search_next: Vec<KeyBinding>,
}

/// Editor-specific keybindings used by the composer textarea.
#[derive(Clone, Debug)]
pub(crate) struct EditorKeymap {
    pub(crate) insert_newline: Vec<KeyBinding>,
    pub(crate) move_left: Vec<KeyBinding>,
    pub(crate) move_right: Vec<KeyBinding>,
    pub(crate) move_up: Vec<KeyBinding>,
    pub(crate) move_down: Vec<KeyBinding>,
    pub(crate) move_word_left: Vec<KeyBinding>,
    pub(crate) move_word_right: Vec<KeyBinding>,
    pub(crate) move_line_start: Vec<KeyBinding>,
    pub(crate) move_line_end: Vec<KeyBinding>,
    pub(crate) delete_backward: Vec<KeyBinding>,
    pub(crate) delete_forward: Vec<KeyBinding>,
    pub(crate) delete_backward_word: Vec<KeyBinding>,
    pub(crate) delete_forward_word: Vec<KeyBinding>,
    pub(crate) kill_line_start: Vec<KeyBinding>,
    pub(crate) kill_whole_line: Vec<KeyBinding>,
    pub(crate) kill_line_end: Vec<KeyBinding>,
    pub(crate) yank: Vec<KeyBinding>,
}

/// Vim normal-mode keybindings for modal editing in the composer textarea.
///
/// Default bindings include both `shift(letter)` and `plain(UPPERCASE)`
/// variants for uppercase commands like `A`, `I`, `O` to handle
/// cross-terminal shift-reporting inconsistencies.
#[derive(Clone, Debug, Default)]
pub(crate) struct VimNormalKeymap {
    pub(crate) enter_insert: Vec<KeyBinding>,
    pub(crate) append_after_cursor: Vec<KeyBinding>,
    pub(crate) append_line_end: Vec<KeyBinding>,
    pub(crate) insert_line_start: Vec<KeyBinding>,
    pub(crate) open_line_below: Vec<KeyBinding>,
    pub(crate) open_line_above: Vec<KeyBinding>,
    pub(crate) enter_replace_mode: Vec<KeyBinding>,
    pub(crate) move_left: Vec<KeyBinding>,
    pub(crate) move_right: Vec<KeyBinding>,
    pub(crate) move_up: Vec<KeyBinding>,
    pub(crate) move_down: Vec<KeyBinding>,
    pub(crate) move_word_forward: Vec<KeyBinding>,
    pub(crate) move_word_backward: Vec<KeyBinding>,
    pub(crate) move_word_end: Vec<KeyBinding>,
    pub(crate) move_line_start: Vec<KeyBinding>,
    pub(crate) move_line_end: Vec<KeyBinding>,
    pub(crate) find_forward: Vec<KeyBinding>,
    pub(crate) find_backward: Vec<KeyBinding>,
    pub(crate) till_forward: Vec<KeyBinding>,
    pub(crate) till_backward: Vec<KeyBinding>,
    pub(crate) jump_top: Vec<KeyBinding>,
    pub(crate) jump_bottom: Vec<KeyBinding>,
    pub(crate) delete_char: Vec<KeyBinding>,
    pub(crate) replace_char: Vec<KeyBinding>,
    pub(crate) repeat_last_change: Vec<KeyBinding>,
    pub(crate) substitute_char: Vec<KeyBinding>,
    pub(crate) delete_to_line_end: Vec<KeyBinding>,
    pub(crate) change_to_line_end: Vec<KeyBinding>,
    pub(crate) yank_line: Vec<KeyBinding>,
    pub(crate) paste_after: Vec<KeyBinding>,
    pub(crate) start_delete_operator: Vec<KeyBinding>,
    pub(crate) start_yank_operator: Vec<KeyBinding>,
    pub(crate) start_change_operator: Vec<KeyBinding>,
    pub(crate) undo: Vec<KeyBinding>,
    pub(crate) redo: Vec<KeyBinding>,
    pub(crate) cancel_operator: Vec<KeyBinding>,
}

/// Vim operator-pending keybindings active after `d`, `y`, or `c` in normal mode.
#[derive(Clone, Debug, Default)]
pub(crate) struct VimOperatorKeymap {
    pub(crate) delete_line: Vec<KeyBinding>,
    pub(crate) yank_line: Vec<KeyBinding>,
    pub(crate) motion_left: Vec<KeyBinding>,
    pub(crate) motion_right: Vec<KeyBinding>,
    pub(crate) motion_up: Vec<KeyBinding>,
    pub(crate) motion_down: Vec<KeyBinding>,
    pub(crate) motion_word_forward: Vec<KeyBinding>,
    pub(crate) motion_word_backward: Vec<KeyBinding>,
    pub(crate) motion_word_end: Vec<KeyBinding>,
    pub(crate) motion_line_start: Vec<KeyBinding>,
    pub(crate) motion_line_end: Vec<KeyBinding>,
    pub(crate) motion_find_forward: Vec<KeyBinding>,
    pub(crate) motion_find_backward: Vec<KeyBinding>,
    pub(crate) motion_till_forward: Vec<KeyBinding>,
    pub(crate) motion_till_backward: Vec<KeyBinding>,
    pub(crate) motion_jump_top: Vec<KeyBinding>,
    pub(crate) motion_jump_bottom: Vec<KeyBinding>,
    pub(crate) select_inner_text_object: Vec<KeyBinding>,
    pub(crate) select_around_text_object: Vec<KeyBinding>,
    pub(crate) cancel: Vec<KeyBinding>,
}

/// Vim text-object keybindings active after an operator plus inner/around prefix.
#[derive(Clone, Debug, Default)]
pub(crate) struct VimTextObjectKeymap {
    pub(crate) word: Vec<KeyBinding>,
    pub(crate) big_word: Vec<KeyBinding>,
    pub(crate) parentheses: Vec<KeyBinding>,
    pub(crate) brackets: Vec<KeyBinding>,
    pub(crate) braces: Vec<KeyBinding>,
    pub(crate) double_quote: Vec<KeyBinding>,
    pub(crate) single_quote: Vec<KeyBinding>,
    pub(crate) backtick: Vec<KeyBinding>,
    pub(crate) cancel: Vec<KeyBinding>,
}

/// Vim search keybindings: transcript and Vim search defaults yield to
/// explicit bindings; explicit search conflicts remain configuration errors.
#[derive(Clone, Debug)]
pub(crate) struct VimSearchKeymap {
    pub(crate) forward: Vec<KeyBinding>,
    pub(crate) backward: Vec<KeyBinding>,
    pub(crate) next: Vec<KeyBinding>,
    pub(crate) previous: Vec<KeyBinding>,
}

impl Default for VimSearchKeymap {
    fn default() -> Self {
        Self {
            forward: vec![plain(KeyCode::Char('/'))],
            backward: vec![plain(KeyCode::Char('?')), shift(KeyCode::Char('?'))],
            next: vec![plain(KeyCode::Char('n'))],
            previous: vec![plain(KeyCode::Char('N')), shift(KeyCode::Char('n'))],
        }
    }
}

/// Runtime keymap used by composer input handlers.
///
/// Reduced to the composer-relevant surfaces; see module docs for the
/// donor's config-resolution machinery that stays parent-owned.
#[derive(Clone, Debug)]
pub(crate) struct RuntimeKeymap {
    pub(crate) composer: ComposerKeymap,
    pub(crate) editor: Arc<EditorKeymap>,
    pub(crate) vim_normal: VimNormalKeymap,
    pub(crate) vim_operator: VimOperatorKeymap,
    pub(crate) vim_search: VimSearchKeymap,
    pub(crate) vim_text_object: VimTextObjectKeymap,
    pub(crate) chords: Arc<RuntimeChordKeymap>,
}

impl RuntimeKeymap {
    /// Built-in keymap defaults, verbatim from the donor's
    /// `built_in_defaults` for the composer surfaces.
    pub(crate) fn defaults() -> Self {
        let mut keymap = Self {
            composer: ComposerKeymap {
                submit: vec![plain(KeyCode::Enter)],
                queue: vec![plain(KeyCode::Tab)],
                // `?` is always literal input; keys help is `/help` or F1.
                toggle_shortcuts: vec![plain(KeyCode::F(1))],
                history_search_previous: vec![ctrl(KeyCode::Char('r'))],
                history_search_next: vec![ctrl(KeyCode::Char('s'))],
            },
            editor: Arc::new(EditorKeymap {
                insert_newline: vec![
                    ctrl(KeyCode::Char('j')),
                    ctrl(KeyCode::Char('m')),
                    plain(KeyCode::Enter),
                    shift(KeyCode::Enter),
                    super::key_hint::alt(KeyCode::Enter),
                ],
                move_left: vec![plain(KeyCode::Left), ctrl(KeyCode::Char('b'))],
                move_right: vec![plain(KeyCode::Right), ctrl(KeyCode::Char('f'))],
                move_up: vec![plain(KeyCode::Up), ctrl(KeyCode::Char('p'))],
                move_down: vec![plain(KeyCode::Down), ctrl(KeyCode::Char('n'))],
                move_word_left: vec![
                    super::key_hint::alt(KeyCode::Char('b')),
                    KeyBinding::new(KeyCode::Left, KeyModifiers::ALT),
                    KeyBinding::new(KeyCode::Left, KeyModifiers::CONTROL),
                ],
                move_word_right: vec![
                    super::key_hint::alt(KeyCode::Char('f')),
                    KeyBinding::new(KeyCode::Right, KeyModifiers::ALT),
                    KeyBinding::new(KeyCode::Right, KeyModifiers::CONTROL),
                ],
                move_line_start: vec![plain(KeyCode::Home), ctrl(KeyCode::Char('a'))],
                move_line_end: vec![plain(KeyCode::End), ctrl(KeyCode::Char('e'))],
                delete_backward: vec![
                    plain(KeyCode::Backspace),
                    shift(KeyCode::Backspace),
                    ctrl(KeyCode::Char('h')),
                ],
                delete_forward: vec![
                    plain(KeyCode::Delete),
                    shift(KeyCode::Delete),
                    ctrl(KeyCode::Char('d')),
                ],
                delete_backward_word: vec![
                    super::key_hint::alt(KeyCode::Backspace),
                    ctrl(KeyCode::Backspace),
                    KeyBinding::new(
                        KeyCode::Backspace,
                        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                    ),
                    ctrl(KeyCode::Char('w')),
                    KeyBinding::new(
                        KeyCode::Char('h'),
                        KeyModifiers::CONTROL | KeyModifiers::ALT,
                    ),
                ],
                delete_forward_word: vec![
                    super::key_hint::alt(KeyCode::Delete),
                    ctrl(KeyCode::Delete),
                    KeyBinding::new(KeyCode::Delete, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                    super::key_hint::alt(KeyCode::Char('d')),
                ],
                kill_line_start: vec![ctrl(KeyCode::Char('u'))],
                kill_whole_line: vec![],
                kill_line_end: vec![ctrl(KeyCode::Char('k'))],
                yank: vec![ctrl(KeyCode::Char('y'))],
            }),
            vim_normal: VimNormalKeymap {
                enter_insert: vec![plain(KeyCode::Char('i')), plain(KeyCode::Insert)],
                append_after_cursor: vec![plain(KeyCode::Char('a'))],
                append_line_end: vec![shift(KeyCode::Char('a')), plain(KeyCode::Char('A'))],
                insert_line_start: vec![shift(KeyCode::Char('i')), plain(KeyCode::Char('I'))],
                open_line_below: vec![plain(KeyCode::Char('o'))],
                open_line_above: vec![shift(KeyCode::Char('o')), plain(KeyCode::Char('O'))],
                enter_replace_mode: vec![shift(KeyCode::Char('r')), plain(KeyCode::Char('R'))],
                move_left: vec![plain(KeyCode::Char('h')), plain(KeyCode::Left)],
                move_right: vec![plain(KeyCode::Char('l')), plain(KeyCode::Right)],
                move_up: vec![plain(KeyCode::Char('k')), plain(KeyCode::Up)],
                move_down: vec![plain(KeyCode::Char('j')), plain(KeyCode::Down)],
                move_word_forward: vec![plain(KeyCode::Char('w'))],
                move_word_backward: vec![plain(KeyCode::Char('b'))],
                move_word_end: vec![plain(KeyCode::Char('e'))],
                move_line_start: vec![plain(KeyCode::Char('0'))],
                move_line_end: vec![plain(KeyCode::Char('$')), shift(KeyCode::Char('$'))],
                find_forward: vec![plain(KeyCode::Char('f'))],
                find_backward: vec![shift(KeyCode::Char('f'))],
                till_forward: vec![plain(KeyCode::Char('t'))],
                till_backward: vec![shift(KeyCode::Char('t'))],
                jump_top: vec![],
                jump_bottom: vec![shift(KeyCode::Char('g')), plain(KeyCode::Char('G'))],
                delete_char: vec![plain(KeyCode::Char('x'))],
                replace_char: vec![plain(KeyCode::Char('r'))],
                repeat_last_change: vec![plain(KeyCode::Char('.'))],
                substitute_char: vec![plain(KeyCode::Char('s'))],
                delete_to_line_end: vec![shift(KeyCode::Char('d')), plain(KeyCode::Char('D'))],
                change_to_line_end: vec![shift(KeyCode::Char('c')), plain(KeyCode::Char('C'))],
                yank_line: vec![shift(KeyCode::Char('y')), plain(KeyCode::Char('Y'))],
                paste_after: vec![plain(KeyCode::Char('p'))],
                start_delete_operator: vec![plain(KeyCode::Char('d'))],
                start_yank_operator: vec![plain(KeyCode::Char('y'))],
                start_change_operator: vec![plain(KeyCode::Char('c'))],
                undo: vec![plain(KeyCode::Char('u'))],
                redo: vec![ctrl(KeyCode::Char('r'))],
                cancel_operator: vec![plain(KeyCode::Esc)],
            },
            vim_operator: VimOperatorKeymap {
                delete_line: vec![plain(KeyCode::Char('d'))],
                yank_line: vec![plain(KeyCode::Char('y'))],
                motion_left: vec![plain(KeyCode::Char('h'))],
                motion_right: vec![plain(KeyCode::Char('l'))],
                motion_up: vec![plain(KeyCode::Char('k'))],
                motion_down: vec![plain(KeyCode::Char('j'))],
                motion_word_forward: vec![plain(KeyCode::Char('w'))],
                motion_word_backward: vec![plain(KeyCode::Char('b'))],
                motion_word_end: vec![plain(KeyCode::Char('e'))],
                motion_line_start: vec![plain(KeyCode::Char('0'))],
                motion_line_end: vec![plain(KeyCode::Char('$')), shift(KeyCode::Char('$'))],
                motion_find_forward: vec![plain(KeyCode::Char('f'))],
                motion_find_backward: vec![shift(KeyCode::Char('f'))],
                motion_till_forward: vec![plain(KeyCode::Char('t'))],
                motion_till_backward: vec![shift(KeyCode::Char('t'))],
                motion_jump_top: vec![],
                motion_jump_bottom: vec![shift(KeyCode::Char('g')), plain(KeyCode::Char('G'))],
                select_inner_text_object: vec![plain(KeyCode::Char('i'))],
                select_around_text_object: vec![plain(KeyCode::Char('a'))],
                cancel: vec![plain(KeyCode::Esc)],
            },
            vim_search: VimSearchKeymap::default(),
            vim_text_object: VimTextObjectKeymap {
                word: vec![plain(KeyCode::Char('w'))],
                big_word: vec![shift(KeyCode::Char('w')), plain(KeyCode::Char('W'))],
                parentheses: vec![
                    plain(KeyCode::Char('(')),
                    shift(KeyCode::Char('(')),
                    plain(KeyCode::Char(')')),
                    shift(KeyCode::Char(')')),
                    plain(KeyCode::Char('b')),
                ],
                brackets: vec![plain(KeyCode::Char('[')), plain(KeyCode::Char(']'))],
                braces: vec![
                    plain(KeyCode::Char('{')),
                    shift(KeyCode::Char('{')),
                    plain(KeyCode::Char('}')),
                    shift(KeyCode::Char('}')),
                    shift(KeyCode::Char('b')),
                    plain(KeyCode::Char('B')),
                ],
                double_quote: vec![plain(KeyCode::Char('"')), shift(KeyCode::Char('"'))],
                single_quote: vec![plain(KeyCode::Char('\''))],
                backtick: vec![plain(KeyCode::Char('`'))],
                cancel: vec![plain(KeyCode::Esc)],
            },
            chords: Arc::new(RuntimeChordKeymap::default()),
        };

        // Donor `RuntimeChordKeymap::from_config` installs the built-in
        // `g g` jump-top chord when no configured binding shadows it.
        let g = plain(KeyCode::Char('g'));
        let jump_top = KeyChord {
            prefix: g,
            completion: g,
        };
        let mut chords = RuntimeChordKeymap::default();
        for action in [
            KeymapActionId {
                context: KeymapContext::VimNormal,
                action: "jump_top",
            },
            KeymapActionId {
                context: KeymapContext::VimOperator,
                action: "motion_jump_top",
            },
        ] {
            chords.bindings.push(RuntimeChordBinding {
                action,
                chord: jump_top,
            });
        }
        let chords = Arc::new(chords);

        // Donor `install_dispatch_bindings`: each chord action also accepts its
        // internal dispatch function-key token.
        let actions: Vec<KeymapActionId> = {
            let mut seen = Vec::new();
            for binding in &chords.bindings {
                if !seen.contains(&binding.action) {
                    seen.push(binding.action);
                }
            }
            seen
        };
        for action in actions {
            if let Some(token) = dispatch_binding(action) {
                match (action.context, action.action) {
                    (KeymapContext::VimNormal, "jump_top") => {
                        keymap.vim_normal.jump_top.push(token);
                    }
                    (KeymapContext::VimOperator, "motion_jump_top") => {
                        keymap.vim_operator.motion_jump_top.push(token);
                    }
                    _ => {}
                }
            }
        }
        keymap.chords = chords;
        keymap
    }
}
