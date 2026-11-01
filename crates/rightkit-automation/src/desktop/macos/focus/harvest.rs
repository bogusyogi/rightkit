//! Breadth-first harvest of visible text from the focused element's window,
//! for screen-context auto-vocabulary
//! (docs/plans/screen_context_vocabulary_2026-07-19.md). AX-only by design.

use core_foundation::base::TCFType;

use super::super::ffi::{
    ax_copy, ax_string, ax_system_element, AXIsProcessTrusted, AXUIElementRef,
};
use super::context::{ax_element_seen_or_insert, AxSeen};

/// Breadth-first harvest of visible text from the focused element's window,
/// expanding OUTWARD from the focused element (its subtree first, then each
/// successive ancestor's subtree) so the text nearest the caret fills the
/// budget before distant chrome does. Powers screen-context auto-vocabulary
/// (docs/plans/screen_context_vocabulary_2026-07-19.md). AX-only by design —
/// same permission as text insertion; no screenshots, no OCR.
///
/// Secure fields are skipped subtree-and-all. Returns raw strings (values,
/// titles, descriptions, placeholders); ranking beyond the outward walk order
/// and all tokenisation live in the engine, which is unit-testable.
pub fn window_text_harvest(budget_chars: usize) -> Vec<String> {
    unsafe {
        if AXIsProcessTrusted() == 0 {
            return Vec::new();
        }
        let Some(system) = ax_system_element() else {
            return Vec::new();
        };
        let Some(focused) = ax_copy(
            system.as_CFTypeRef() as AXUIElementRef,
            "AXFocusedUIElement",
        ) else {
            return Vec::new();
        };
        let focused = core_foundation::base::CFType::wrap_under_create_rule(focused);

        // Ancestor chain up to (and including) the window. Walk each level's
        // subtree in turn; logical AX equality stops re-visiting a previously
        // covered inner subtree when we expand to its parent.
        let mut ancestors: Vec<core_foundation::base::CFType> = vec![focused];
        loop {
            let last = ancestors.last().unwrap().as_CFTypeRef() as AXUIElementRef;
            if ax_string(last, "AXRole").as_deref() == Some("AXWindow") || ancestors.len() >= 24 {
                break;
            }
            let Some(parent) = ax_copy(last, "AXParent") else {
                break;
            };
            ancestors.push(core_foundation::base::CFType::wrap_under_create_rule(
                parent,
            ));
        }

        let mut out: Vec<String> = Vec::new();
        let mut used = 0usize;
        // AX can return distinct CF object pointers for one logical element.
        // CFType's PartialEq delegates to CFEqual, unlike pointer-address
        // hashing; retain up to MAX_VISITS objects for valid comparisons.
        let mut seen = AxSeen::new();
        // Hard caps so a pathological tree can neither run long nor grow
        // unbounded: element-visit ceiling plus the caller's char budget.
        let mut visits = 0usize;
        const MAX_VISITS: usize = 4_000;

        'levels: for level in ancestors.iter() {
            let mut queue: std::collections::VecDeque<core_foundation::base::CFType> =
                std::collections::VecDeque::new();
            queue.push_back(level.clone());
            while let Some(el_owner) = queue.pop_front() {
                let el = el_owner.as_CFTypeRef() as AXUIElementRef;
                if ax_element_seen_or_insert(&mut seen, &el_owner) {
                    continue; // covered by an inner (closer) level already
                }
                visits += 1;
                if visits > MAX_VISITS || used >= budget_chars {
                    break 'levels;
                }
                let role = ax_string(el, "AXRole");
                if role.as_deref() == Some("AXSecureTextField")
                    || ax_string(el, "AXSubrole").as_deref() == Some("AXSecureTextField")
                {
                    continue; // never read or descend into secure fields
                }
                for attr in ["AXValue", "AXTitle", "AXDescription", "AXPlaceholderValue"] {
                    if let Some(text) = ax_string(el, attr) {
                        let text = text.trim();
                        if text.len() >= 2 {
                            used += text.chars().count();
                            out.push(text.to_string());
                            if used >= budget_chars {
                                break 'levels;
                            }
                        }
                    }
                }
                if let Some(children) = ax_copy(el, "AXChildren") {
                    // CFArray<CFType> is not a ConcreteCFType, so downcast_into
                    // is unavailable; go through the raw CFArray API instead.
                    // wrap_under_create_rule owns the +1 from AXUIElementCopy…;
                    // the retained items are owned clones of the array entries.
                    use core_foundation::array::{
                        CFArrayGetCount, CFArrayGetValueAtIndex, CFArrayRef,
                    };
                    let children_owner =
                        core_foundation::base::CFType::wrap_under_create_rule(children);
                    let array = children_owner.as_CFTypeRef() as CFArrayRef;
                    let count = CFArrayGetCount(array);
                    for i in 0..count {
                        let item = CFArrayGetValueAtIndex(array, i);
                        if !item.is_null() {
                            queue.push_back(core_foundation::base::CFType::wrap_under_get_rule(
                                item,
                            ));
                        }
                    }
                }
            }
        }
        out
    }
}
