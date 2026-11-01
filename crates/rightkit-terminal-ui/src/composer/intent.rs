//! What the composer asks the host application to do.
//!
//! The composer only *emits* intent -- it never cancels a streaming turn,
//! aborts a shell, or quits the process itself. Those effects live in the
//! host (`app`), which has the state (is a turn streaming? is a shell
//! running?) the composer doesn't own and shouldn't reach into.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Intent {
    /// The user submitted the composer's contents (Enter on a non-empty
    /// buffer, outside shell-input mode). Carries the fully expanded text,
    /// with any paste markers already substituted back in.
    Submit(String),
    /// Ctrl+D on an empty composer, or double Ctrl+C within 500ms.
    Quit,
    /// First link of the escape priority chain: a turn is streaming.
    CancelStreamingTurn,
    /// Second link: a shell command is running.
    AbortRunningShell,
    /// Third link: composer is in shell-input mode.
    ExitShellInputMode,
    /// Last link: composer is empty and idle; double-Escape within 500ms
    /// asks the host to open its picker.
    OpenPicker,
    /// Copy the supplied text to the system clipboard via OSC52 with
    /// tmux/SSH fallback (S:I4 direct port of codex clipboard_copy.rs).
    /// The host should attempt OSC52 first, wrapping for tmux when needed,
    /// and fall back to a native provider if the terminal does not support it.
    Copy(String),
    /// S:I5 — the user typed one of the six truthfully-distinct command
    /// effects (`/new`, `/clear`, `/reset`, `/rewind`, `/undo`, `/redo`).
    /// Resolution (name + alias, case/whitespace-insensitive) already
    /// happened in the composer against `command_effects::CommandEffect`;
    /// the host decides what each effect does to its own session state and
    /// reports the outcome. `quit`/`exit`/`q` stay `Intent::Quit` — they are
    /// deliberately NOT part of this registry.
    CommandEffect(crate::command_effects::CommandEffect),
    /// Shift+Tab: advance the live session's permission mode one step
    /// (Ask → Accept edits → Auto → Yolo → Ask). The host owns the mode.
    CyclePermissionMode,
    /// Tab while a turn runs: push this text into the running turn now
    /// (`interject::Kind::Steer`). Enter, by contrast, emits `Submit` and the
    /// host queues it.
    Push(String),
    /// Up on an empty prompt with messages queued: take the last queued
    /// message back into the composer for editing.
    EditLastQueued,
    /// ctrl+x on an empty prompt with messages queued: drop the last one.
    RemoveLastQueued,
    /// Ctrl+G: edit the draft in `$VISUAL`/`$EDITOR`. Carries the expanded
    /// draft text; the host owns the terminal hand-off and loads the edited
    /// text back with `Composer::set_text`.
    OpenExternalEditor(String),
}
