//! Focused-field / AX context reading: locating the exact focused element
//! for a process, resolving the editable descendant when the focused node
//! itself is a container, and reading its text, selection, and window title
//! into a `FocusedField` for polish/recovery context.
//!
//! Split out of the former single-file `macos/focus.rs`
//! (docs/agent-rules.md: pure move, no logic changes).

use core_foundation::base::{CFHash, CFType, TCFType};
use core_foundation::string::CFString;

use super::super::ffi::{
    ax_app_element, ax_attribute_settable, ax_bool, ax_children, ax_copy, ax_copy_with_error,
    ax_dom_classes, ax_element_pid, ax_parameterized_copy, ax_selected_range, ax_string,
    ax_string_bounded, ax_system_element, bounded_proof_string, AXIsProcessTrusted, AXUIElementRef,
    AXUIElementSetMessagingTimeout, AX_MESSAGING_TIMEOUT_SECONDS,
};
use super::super::frontmost::frontmost;
use super::pin::{focus_pin_running_identity_matches, FocusPin};

/// Accessibility snapshot of the focused text field — on-device context that
/// grounds app-aware polish in what the user is actually writing. Uses the AX
/// API the app is already trusted for (same permission as CGEvent insertion).
/// Secure (password) fields are never read.
#[derive(Clone, Debug, Default)]
pub struct FocusedField {
    pub window_title: Option<String>,
    /// Existing text in the focused field (middle-truncated to the cap).
    pub field_text: Option<String>,
    /// The user's current selection inside the field, if any.
    pub selected_text: Option<String>,
    /// Character immediately before the caret/selection start, when the field
    /// was fully read, is not placeholder-only, and is not a terminal. `None`
    /// means "no spacing hint": start of field, unknown, or excluded target.
    pub char_before_caret: Option<char>,
}

/// Focused element for the exact hotkey-time process.
///
/// AX context is a boundary-only enrichment, not recording-loop authority. Use
/// the system-wide focused element when it still belongs to `pid`; otherwise
/// fall back to read-only app/window queries. Never mutate another app's
/// accessibility mode to obtain optional polish context. Every AX server call
/// uses a finite messaging timeout; `kAXErrorNoValue` is normal absence.
pub(in crate::desktop::macos) fn focused_element_for_pid(pid: i32) -> Option<CFType> {
    unsafe {
        if let Some(system) = ax_system_element() {
            let system_ref = system.as_CFTypeRef() as AXUIElementRef;
            let _ = AXUIElementSetMessagingTimeout(system_ref, AX_MESSAGING_TIMEOUT_SECONDS);
            if let Ok(focused) = ax_copy_with_error(system_ref, "AXFocusedUIElement") {
                let focused = CFType::wrap_under_create_rule(focused);
                let focused_ref = focused.as_CFTypeRef() as AXUIElementRef;
                if ax_element_pid(focused_ref) == Some(pid) {
                    let _ =
                        AXUIElementSetMessagingTimeout(focused_ref, AX_MESSAGING_TIMEOUT_SECONDS);
                    return Some(focused);
                }
            }
        }

        let app = ax_app_element(pid)?;
        let app_ref = app.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(app_ref, AX_MESSAGING_TIMEOUT_SECONDS);

        let focused = ax_copy_with_error(app_ref, "AXFocusedUIElement").ok();
        if let Some(focused) = focused {
            let focused = CFType::wrap_under_create_rule(focused);
            let _ = AXUIElementSetMessagingTimeout(
                focused.as_CFTypeRef() as AXUIElementRef,
                AX_MESSAGING_TIMEOUT_SECONDS,
            );
            return Some(focused);
        }

        for attr in ["AXFocusedWindow", "AXMainWindow"] {
            if let Ok(window) = ax_copy_with_error(app_ref, attr) {
                let window = CFType::wrap_under_create_rule(window);
                let _ = AXUIElementSetMessagingTimeout(
                    window.as_CFTypeRef() as AXUIElementRef,
                    AX_MESSAGING_TIMEOUT_SECONDS,
                );
                return Some(window);
            }
        }
    }
    eprintln!("macos_ax_context_unavailable pid={pid} reason=no_focused_element_or_window");
    None
}

/// Return only an element proven to be the currently focused element for
/// `pid`. Unlike `focused_element_for_pid`, this deliberately has no window
/// fallback: a focused/main window is not evidence that its text control is
/// non-secure, and using it here would make the paste security gate fail open.
pub(in crate::desktop::macos) fn actual_focused_element_for_pid(pid: i32) -> Option<CFType> {
    unsafe {
        if let Some(system) = ax_system_element() {
            let system_ref = system.as_CFTypeRef() as AXUIElementRef;
            let _ = AXUIElementSetMessagingTimeout(system_ref, AX_MESSAGING_TIMEOUT_SECONDS);
            if let Ok(focused) = ax_copy_with_error(system_ref, "AXFocusedUIElement") {
                let focused = CFType::wrap_under_create_rule(focused);
                let focused_ref = focused.as_CFTypeRef() as AXUIElementRef;
                if ax_element_pid(focused_ref) == Some(pid) {
                    let _ =
                        AXUIElementSetMessagingTimeout(focused_ref, AX_MESSAGING_TIMEOUT_SECONDS);
                    return Some(focused);
                }
            }
        }

        // App-level AXFocusedUIElement is still an actual control, unlike
        // AXFocusedWindow/AXMainWindow. Verify its owner before accepting it.
        let app = ax_app_element(pid)?;
        let app_ref = app.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(app_ref, AX_MESSAGING_TIMEOUT_SECONDS);
        let focused = ax_copy_with_error(app_ref, "AXFocusedUIElement").ok()?;
        let focused = CFType::wrap_under_create_rule(focused);
        let focused_ref = focused.as_CFTypeRef() as AXUIElementRef;
        (ax_element_pid(focused_ref) == Some(pid)).then_some(focused)
    }
}

/// Stable, privacy-free identity for the currently focused AX element owned by
/// `pid`. This is live enrichment only; stop capture uses the native
/// CoreGraphics window ID and never calls this helper.
pub fn focused_target_identity_for_pid(pid: i32) -> Option<isize> {
    unsafe {
        let focused = actual_focused_element_for_pid(pid)?;
        let focused_ref = focused.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(focused_ref, AX_MESSAGING_TIMEOUT_SECONDS);
        let window = CFType::wrap_under_create_rule(ax_copy(focused_ref, "AXWindow")?);
        let window_hash = CFHash(window.as_CFTypeRef()) as u64;
        let element_hash = CFHash(focused.as_CFTypeRef()) as u64;
        Some((window_hash.rotate_left(17) ^ element_hash) as isize)
    }
}

pub(in crate::desktop::macos) fn focused_window_for_pid(pid: i32) -> Option<CFType> {
    unsafe {
        let app = ax_app_element(pid)?;
        let app_ref = app.as_CFTypeRef() as AXUIElementRef;
        let _ = AXUIElementSetMessagingTimeout(app_ref, AX_MESSAGING_TIMEOUT_SECONDS);
        for attr in ["AXFocusedWindow", "AXMainWindow"] {
            if let Some(window) = ax_copy(app_ref, attr) {
                let window = CFType::wrap_under_create_rule(window);
                let _ = AXUIElementSetMessagingTimeout(
                    window.as_CFTypeRef() as AXUIElementRef,
                    AX_MESSAGING_TIMEOUT_SECONDS,
                );
                return Some(window);
            }
        }
        None
    }
}

/// Chromium contenteditable controls may expose text only through marker
/// parameterized attributes on an ancestor web area.
pub(in crate::desktop::macos) unsafe fn ax_text_marker_string(
    element: AXUIElementRef,
    range_attr: &'static str,
) -> Option<String> {
    let original = element;
    let selected_range = (range_attr != "AXTextMarkerRangeForUIElement")
        .then(|| ax_copy(original, range_attr))
        .flatten()
        .map(|range| CFType::wrap_under_create_rule(range));
    let mut owner = CFType::wrap_under_get_rule(element);
    for _ in 0..16 {
        let owner_ref = owner.as_CFTypeRef() as AXUIElementRef;
        let range = if range_attr == "AXTextMarkerRangeForUIElement" {
            ax_parameterized_copy(owner_ref, range_attr, original)
                .map(|range| CFType::wrap_under_create_rule(range))
        } else {
            selected_range.clone()
        };
        if let Some(range) = range {
            if let Some(string) = ax_parameterized_copy(
                owner_ref,
                "AXStringForTextMarkerRange",
                range.as_CFTypeRef(),
            ) {
                if let Some(value) = CFType::wrap_under_create_rule(string)
                    .downcast_into::<CFString>()
                    .and_then(bounded_proof_string)
                {
                    return Some(value);
                }
            }
        }
        let Some(parent) = ax_copy(owner_ref, "AXParent") else {
            break;
        };
        owner = CFType::wrap_under_create_rule(parent);
    }
    None
}

/// Search a focused subtree/window for an editable text candidate. Generic
/// groups and web areas are traversal roots, never text sources.
pub(in crate::desktop::macos) unsafe fn focused_descendant(
    root: &CFType,
    deadline: std::time::Instant,
) -> Option<CFType> {
    use core_foundation::array::{CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef};

    const MAX_VISITS: usize = 1536;
    let mut queue = std::collections::VecDeque::from([root.clone()]);
    let mut seen = AxSeen::new();
    let mut visits = 0usize;
    let mut best: Option<(i32, CFType)> = None;

    while let Some(owner) = queue.pop_front() {
        if visits >= MAX_VISITS || std::time::Instant::now() >= deadline {
            break;
        }
        if ax_element_seen_or_insert(&mut seen, &owner) {
            continue;
        }
        visits += 1;
        let element = owner.as_CFTypeRef() as AXUIElementRef;
        let role = ax_string(element, "AXRole");
        let role_str = role.as_deref().unwrap_or("");
        let subrole = ax_string(element, "AXSubrole");
        if role_str == "AXSecureTextField" || subrole.as_deref() == Some("AXSecureTextField") {
            continue;
        }
        if role_str != "AXWindow" {
            let focused = ax_bool(element, "AXFocused") == Some(true);
            let native_editable = matches!(
                role_str,
                "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField"
            );
            let role_description = ax_string(element, "AXRoleDescription")
                .unwrap_or_default()
                .to_ascii_lowercase();
            let settable = native_editable
                || ax_attribute_settable(element, "AXValue")
                || ax_attribute_settable(element, "AXSelectedText");
            let text_semantics = !matches!(role_str, "AXStaticText" | "AXWebArea" | "AXGroup")
                && (role_description.contains("text") || role_description.contains("edit"));
            let marker_support = (native_editable || focused || text_semantics)
                && ax_parameterized_copy(element, "AXTextMarkerRangeForUIElement", element)
                    .map(|range| CFType::wrap_under_create_rule(range))
                    .is_some();
            let has_value = ax_string(element, "AXValue")
                .map(|value| !value.trim().is_empty())
                .unwrap_or(false);

            if let Some(score) = editable_candidate_score(
                role_str,
                &role_description,
                focused,
                marker_support,
                settable,
                has_value,
            ) {
                if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                    best = Some((score, owner.clone()));
                }
            }
        }
        if let Some(children) = ax_copy(element, "AXChildren") {
            let children = CFType::wrap_under_create_rule(children);
            let array = children.as_CFTypeRef() as CFArrayRef;
            for index in 0..CFArrayGetCount(array) {
                let child = CFArrayGetValueAtIndex(array, index);
                if !child.is_null() {
                    queue.push_back(CFType::wrap_under_get_rule(child));
                }
            }
        }
    }
    best.map(|(_, element)| element)
}

pub(in crate::desktop::macos) fn editable_candidate_score(
    role: &str,
    role_description: &str,
    focused: bool,
    marker_support: bool,
    settable: bool,
    has_value: bool,
) -> Option<i32> {
    let native_editable = matches!(
        role,
        "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSearchField"
    );
    let text_semantics = !matches!(role, "AXStaticText" | "AXWebArea" | "AXGroup")
        && (role_description.contains("text") || role_description.contains("edit"));
    let eligible = native_editable || (focused && text_semantics && (marker_support || settable));
    eligible.then_some(
        i32::from(focused) * 1_000
            + i32::from(native_editable) * 100
            + i32::from(marker_support) * 20
            + i32::from(settable) * 10
            + i32::from(has_value),
    )
}

/// Keep the head and tail of long field text; the middle is the least useful
/// context and the cheapest to drop. Char-boundary safe.
pub(in crate::desktop::macos) fn truncate_middle(text: &str, max_chars: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let head = max_chars * 2 / 3;
    let tail = max_chars - head;
    let mut out: String = chars[..head].iter().collect();
    out.push_str(" […] ");
    out.extend(chars[chars.len() - tail..].iter());
    out
}

/// Read the focused UI element's text, selection, and window title via AX.
/// Returns None when nothing is focused, the process isn't AX-trusted, or the
/// focused element is a secure (password) field.
pub(in crate::desktop::macos) fn secure_state_for_roles(
    role: Option<&str>,
    subrole: Option<&str>,
) -> Option<bool> {
    let known_role = role.is_some_and(|role| {
        matches!(
            role,
            "AXTextField"
                | "AXTextArea"
                | "AXComboBox"
                | "AXSearchField"
                | "AXSecureTextField"
                | "AXWebArea"
                | "AXWindow"
                | "AXGroup"
                | "AXDocument"
        )
    });
    let known_subrole = subrole.is_some_and(|subrole| {
        matches!(
            subrole,
            "AXSecureTextField" | "AXStandardWindow" | "AXDialog"
        )
    });
    if !known_role && !known_subrole {
        return None;
    }
    Some(role == Some("AXSecureTextField") || subrole == Some("AXSecureTextField"))
}

pub fn focused_field_is_secure_for_pid(pid: i32) -> Option<bool> {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return None;
        }
        let focused_owner = actual_focused_element_for_pid(pid)?;
        let focused = focused_owner.as_CFTypeRef() as AXUIElementRef;
        secure_state_for_roles(
            ax_string(focused, "AXRole").as_deref(),
            ax_string(focused, "AXSubrole").as_deref(),
        )
    }
}

/// Security classification for exact stop-time editor, without PID-based
/// focused-element rediscovery.
pub fn focused_field_is_secure_for_pin(pin: &FocusPin) -> Option<bool> {
    unsafe {
        if AXIsProcessTrusted() == 0 || !focus_pin_running_identity_matches(pin) {
            return None;
        }
        let element = pin.element.as_CFTypeRef() as AXUIElementRef;
        if ax_element_pid(element) != Some(pin.pid) {
            return None;
        }
        secure_state_for_roles(
            ax_string(element, "AXRole").as_deref(),
            ax_string(element, "AXSubrole").as_deref(),
        )
    }
}

pub fn focused_field_context(max_chars: usize) -> Option<FocusedField> {
    let pid = frontmost()?.pid;
    focused_field_context_for_pid(pid, max_chars)
}

/// Read context from exact app captured at hotkey time. This prevents a later
/// sidecar focus sample from silently binding polish to another application.
pub fn focused_field_context_for_pid(pid: i32, max_chars: usize) -> Option<FocusedField> {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return None;
        }
        let focused_owner = focused_element_for_pid(pid)?;
        let focused = focused_owner.as_CFTypeRef() as AXUIElementRef;
        if ax_string(focused, "AXRole").as_deref() == Some("AXSecureTextField")
            || ax_string(focused, "AXSubrole").as_deref() == Some("AXSecureTextField")
        {
            return None;
        }
        let clean = |t: String| Some(t.trim().to_string()).filter(|t| !t.is_empty());
        // Same reading rules as `focused_field_context_and_fingerprint_for_pin`
        // (marker fallback, placeholder-as-empty); a divergence made editors
        // that publish text only through markers capture fine, then fail the
        // selection re-check.
        let (raw_field, raw_selected) = raw_field_and_selection(focused);
        let placeholder_only = raw_field
            .as_deref()
            .is_some_and(|raw| value_is_placeholder(raw, &placeholder_texts(focused)));
        let field_text = raw_field
            .filter(|_| !placeholder_only)
            .and_then(clean)
            .map(|t| truncate_middle(&t, max_chars));
        let selected_text = raw_selected
            .and_then(clean)
            .map(|t| truncate_middle(&t, max_chars));
        let window_title = if ax_string(focused, "AXRole").as_deref() == Some("AXWindow") {
            ax_string(focused, "AXTitle").and_then(clean)
        } else {
            ax_copy(focused, "AXWindow").and_then(|window| {
                let window = core_foundation::base::CFType::wrap_under_create_rule(window);
                ax_string(window.as_CFTypeRef() as AXUIElementRef, "AXTitle").and_then(clean)
            })
        };
        if window_title.is_none() && field_text.is_none() && selected_text.is_none() {
            return None;
        }
        Some(FocusedField {
            window_title,
            field_text,
            selected_text,
            char_before_caret: None,
        })
    }
}

/// Read context from retained exact editor captured at recording start.
pub fn focused_field_context_for_pin(pin: &FocusPin, max_chars: usize) -> Option<FocusedField> {
    focused_field_context_and_fingerprint_for_pin(pin, max_chars).map(|(context, _)| context)
}

/// Read exact editor context & its full content fingerprint from one retained
/// AX read. Missing AXValue/AXSelectedText means unreadable; successful empty
/// values remain distinguishable from failed reads by this outer `Option`.
pub fn focused_field_context_and_fingerprint_for_pin(
    pin: &FocusPin,
    max_chars: usize,
) -> Option<(FocusedField, Option<u64>)> {
    read_editor_context_for_pin(pin, max_chars).ok()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorContextReadFailure {
    Unreadable,
    UnsupportedTerminal,
}

/// Distinguish transient AX misses from terminal output that must never be
/// treated as editable context or retried as a delayed editor.
pub fn read_editor_context_for_pin(
    pin: &FocusPin,
    max_chars: usize,
) -> Result<(FocusedField, Option<u64>), EditorContextReadFailure> {
    unsafe {
        if AXIsProcessTrusted() == 0
            || focused_field_is_secure_for_pin(pin) != Some(false)
            || !focus_pin_running_identity_matches(pin)
        {
            return Err(EditorContextReadFailure::Unreadable);
        }
        let focused = pin.element.as_CFTypeRef() as AXUIElementRef;
        // A terminal surface's AXValue is rendered output/scrollback, not
        // the application's editable input. Binding delivery to that value
        // makes a redraw look like a user edit. Leave start context unreadable
        // instead of claiming an empty editor; ordinary stop-target delivery
        // still requires its own identity/security checks & insertion proof.
        // Editable native controls hosted by a terminal (e.g. Find) retain
        // normal content binding.
        if !supports_editor_content_binding(
            &pin.bundle_identifier,
            !is_terminal_bundle(&pin.bundle_identifier)
                || ax_attribute_settable(focused, "AXValue"),
        ) {
            return Err(EditorContextReadFailure::UnsupportedTerminal);
        }
        let (raw_field, raw_selected) = raw_field_and_selection(focused);
        // AXSelectedText alone is only a selection projection, never proof of
        // full editor contents. Require one successful full read so a failed
        // read plus empty selection cannot become ConfirmedEmpty.
        let raw_field = raw_field.ok_or(EditorContextReadFailure::Unreadable)?;
        let fingerprint = {
            use std::hash::{Hash, Hasher};
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            raw_field.hash(&mut hasher);
            Some(hasher.finish())
        };
        // An empty web composer reports its placeholder as AXValue (measured:
        // ChatGPT desktop, "\nDo anything", no AXPlaceholderValue).
        // Positive placeholder evidence makes it a confirmed-empty editor.
        let placeholder_only = value_is_placeholder(&raw_field, &placeholder_texts(focused));
        let clean = |text: String| Some(text.trim().to_string()).filter(|text| !text.is_empty());
        let field_text = (!placeholder_only)
            .then(|| raw_field.clone())
            .and_then(clean)
            .map(|text| truncate_middle(&text, max_chars));
        let selected_text = raw_selected
            .and_then(clean)
            .map(|text| truncate_middle(&text, max_chars));
        let window_title = ax_copy(focused, "AXWindow").and_then(|window| {
            let window = core_foundation::base::CFType::wrap_under_create_rule(window);
            ax_string(window.as_CFTypeRef() as AXUIElementRef, "AXTitle").and_then(clean)
        });
        // Spacing hint for delivery: judged at the caret (UTF-16 units, the
        // unit AX uses), end of field when the range is unavailable. Terminal
        // scrollback is not prose, so terminals never get a hint.
        let char_before_caret = (!placeholder_only && !is_terminal_bundle(&pin.bundle_identifier))
            .then(|| {
                let units: Vec<u16> = raw_field.encode_utf16().collect();
                let caret = ax_selected_range(focused)
                    .map(|(location, _)| location)
                    .unwrap_or(units.len())
                    .min(units.len());
                String::from_utf16_lossy(&units[..caret])
                    .chars()
                    .next_back()
            })
            .flatten();
        Ok((
            FocusedField {
                window_title,
                field_text,
                selected_text,
                char_before_caret,
            },
            fingerprint,
        ))
    }
}

/// The ONE way to read an editor's full contents and selection. Start capture
/// and the engine's later selection re-check must agree byte for byte, so both
/// go through here. Bounded full read: oversized content is unreadable rather
/// than a truncated fingerprint that could falsely prove unchanged content.
/// Chromium/WebKit composers may publish text only through marker ranges, so
/// that representation is an equal source.
pub(in crate::desktop::macos) unsafe fn raw_field_and_selection(
    focused: AXUIElementRef,
) -> (Option<String>, Option<String>) {
    (
        ax_string_bounded(focused, "AXValue")
            .or_else(|| ax_text_marker_string(focused, "AXTextMarkerRangeForUIElement")),
        ax_string_bounded(focused, "AXSelectedText")
            .or_else(|| ax_text_marker_string(focused, "AXSelectedTextMarkerRange")),
    )
}

pub(in crate::desktop::macos) fn is_terminal_bundle(bundle_identifier: &str) -> bool {
    matches!(
        bundle_identifier,
        "com.apple.Terminal"
            | "com.googlecode.iterm2"
            | "com.mitchellh.ghostty"
            | "dev.warp.Warp-Stable"
            | "net.kovidgoyal.kitty"
            | "co.zeit.hyper"
            | "org.alacritty"
            | "com.github.wez.wezterm"
    )
}

fn supports_editor_content_binding(bundle_identifier: &str, value_settable: bool) -> bool {
    !is_terminal_bundle(bundle_identifier) || value_settable
}

/// True only when the editor's whole value is exactly one advertised
/// placeholder. Real text that merely contains the placeholder stays content.
pub(in crate::desktop::macos) fn value_is_placeholder(
    value: &str,
    placeholders: &[String],
) -> bool {
    // Slate marks an empty text node with U+FEFF; it is not Unicode whitespace.
    let normalize = |text: &str| -> String {
        text.chars()
            .filter(|c| !matches!(c, '\u{FEFF}' | '\u{200B}'))
            .collect::<String>()
            .trim()
            .to_string()
    };
    let value = normalize(value);
    !value.is_empty()
        && placeholders
            .iter()
            .any(|placeholder| normalize(placeholder) == value)
}

/// Slate (Devin/Windsurf, measured) renders U+FEFF only inside an
/// EMPTY text node. A paragraph holding that marker plus further static text
/// can therefore only be marker + placeholder overlay; return the overlay.
pub(in crate::desktop::macos) fn slate_placeholder(paragraph_texts: &[String]) -> Option<String> {
    let (first, rest) = paragraph_texts.split_first()?;
    (first == "\u{FEFF}" && !rest.is_empty()).then(|| rest.concat())
}

/// Placeholder strings the editor itself advertises: `AXPlaceholderValue`, or
/// the text under a shallow descendant whose DOM class names it a placeholder
/// (ProseMirror/Chromium decoration, present only while the editor is empty).
pub(in crate::desktop::macos) unsafe fn placeholder_texts(element: AXUIElementRef) -> Vec<String> {
    let mut found: Vec<String> = ax_string_bounded(element, "AXPlaceholderValue")
        .into_iter()
        .collect();
    let mut queue = vec![(CFType::wrap_under_get_rule(element), 0usize, false)];
    let mut visited = 0usize;
    let mut placeholder_text = String::new();
    while let Some((node, depth, inside)) = queue.pop() {
        visited += 1;
        if visited > 48 {
            break;
        }
        let node_ref = node.as_CFTypeRef() as AXUIElementRef;
        let inside = inside
            || (depth > 0
                && ax_dom_classes(node_ref)
                    .iter()
                    .any(|class| class.to_ascii_lowercase().contains("placeholder")));
        if inside && ax_string(node_ref, "AXRole").as_deref() == Some("AXStaticText") {
            if let Some(text) = ax_string_bounded(node_ref, "AXValue") {
                placeholder_text.push_str(&text);
            }
        }
        let children = ax_children(node_ref);
        let paragraph_texts: Vec<String> = children
            .iter()
            .filter(|child| {
                ax_string(child.as_CFTypeRef() as AXUIElementRef, "AXRole").as_deref()
                    == Some("AXStaticText")
            })
            .filter_map(|child| {
                ax_string_bounded(child.as_CFTypeRef() as AXUIElementRef, "AXValue")
            })
            .collect();
        if let Some(overlay) = slate_placeholder(&paragraph_texts) {
            found.push(overlay);
        }
        if depth < 4 {
            for child in children {
                queue.push((child, depth + 1, inside));
            }
        }
    }
    if !placeholder_text.trim().is_empty() {
        found.push(placeholder_text);
    }
    found
}

pub(in crate::desktop::macos) type AxSeen = std::collections::HashMap<usize, Vec<CFType>>;

#[inline]
pub(in crate::desktop::macos) fn ax_element_seen_or_insert(
    seen: &mut AxSeen,
    candidate: &CFType,
) -> bool {
    let hash = unsafe { CFHash(candidate.as_CFTypeRef()) };
    // CFHash narrows candidates to a small bucket; CFEqual resolves collisions.
    let bucket = seen.entry(hash).or_default();
    if bucket.iter().any(|seen_el| seen_el == candidate) {
        true
    } else {
        bucket.push(candidate.clone());
        false
    }
}

#[cfg(test)]
#[path = "context_tests.rs"]
mod tests;
