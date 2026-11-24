//! Classifies a finished voice command's `(action, detail)` pair.
//! The host supplies every displayed label.

/// The kind of a completed command. Variants name what happened, not any wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOutcome {
    LaunchApp,
    MouseClick,
    MouseScroll,
    PowerLock,
    PowerSleep,
    PowerRestart,
    PowerShutdown,
    PowerSignOut,
    PowerOther,
    ClearClipboard,
    BackspaceLast,
    AlwaysOnTop,
    ForceQuit,
    SpecialOther,
    LastPasteTransform,
    RunShortcut,
    SwitchWindow,
    Copy,
    Paste,
    Save,
    Undo,
    Redo,
    Back,
    Forward,
    Refresh,
    Close,
    NewTab,
    Find,
    Other,
}

/// Classify an engine command result. Case-insensitive; `detail` may be absent.
pub fn classify(action: &str, detail: Option<&str>) -> CommandOutcome {
    use CommandOutcome::*;
    let action = action.to_ascii_lowercase();
    let detail = detail.unwrap_or("").to_ascii_lowercase();
    let haystack = format!("{action} {detail}");

    if action == "launch_app" {
        return LaunchApp;
    }
    if action == "mouse_click" {
        return MouseClick;
    }
    if action == "mouse_scroll" {
        return MouseScroll;
    }
    if action == "power" {
        if haystack.contains("lock") {
            return PowerLock;
        }
        if haystack.contains("sleep") {
            return PowerSleep;
        }
        if haystack.contains("restart") {
            return PowerRestart;
        }
        if haystack.contains("shutdown") {
            return PowerShutdown;
        }
        if haystack.contains("sign out") {
            return PowerSignOut;
        }
        return PowerOther;
    }
    if action == "special" {
        if haystack.contains("clear_clipboard") {
            return ClearClipboard;
        }
        if haystack.contains("backspace_last") {
            return BackspaceLast;
        }
        if haystack.contains("always_on_top") {
            return AlwaysOnTop;
        }
        if haystack.contains("force_quit") {
            return ForceQuit;
        }
        return SpecialOther;
    }
    if action == "last_paste_transform" {
        return LastPasteTransform;
    }
    if action == "run_shortcut" {
        return RunShortcut;
    }
    if action == "key_sequence" {
        if haystack.contains("cmd+tab")
            || haystack.contains("alt+tab")
            || haystack.contains("ctrl+tab")
            || haystack.contains("ctrl+shift+tab")
        {
            return SwitchWindow;
        }
        if haystack.contains("ctrl+c") || haystack.contains("cmd+c") {
            return Copy;
        }
        if haystack.contains("ctrl+v") || haystack.contains("cmd+v") {
            return Paste;
        }
        if haystack.contains("ctrl+s") || haystack.contains("cmd+s") {
            return Save;
        }
        if haystack.contains("ctrl+z") || haystack.contains("cmd+z") {
            return Undo;
        }
        if haystack.contains("ctrl+y")
            || haystack.contains("cmd+shift+z")
            || haystack.contains("ctrl+shift+z")
        {
            return Redo;
        }
        if haystack.contains("alt+left") || haystack.contains("cmd+[") {
            return Back;
        }
        if haystack.contains("alt+right") || haystack.contains("cmd+]") {
            return Forward;
        }
        if haystack.contains("ctrl+r") || haystack.contains("cmd+r") || haystack.contains("f5") {
            return Refresh;
        }
        if haystack.contains("ctrl+w") || haystack.contains("cmd+w") {
            return Close;
        }
        if haystack.contains("ctrl+t") || haystack.contains("cmd+t") {
            return NewTab;
        }
        if haystack.contains("ctrl+f") || haystack.contains("cmd+f") {
            return Find;
        }
    }
    Other
}
