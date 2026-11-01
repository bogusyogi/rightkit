//! The one line that always names where nav is looking.
//!
//! Kept as a pure function over plain strings rather than a method on the
//! view: the failure mode this guards against is a breadcrumb that silently
//! goes stale because it was cached instead of recomputed from the current
//! position on every frame.

/// Render `session › agent › subagent`, however deep `path` goes.
///
/// `path` is root-first (session's own agent first, deepest agent last).
/// Empty `path` still names the session -- the breadcrumb never goes blank,
/// because "nowhere in particular" is not a position nav can be in.
///
/// Position only: the model/provider attribution lives on the identity
/// strip (`project · actor/model`, S:I6), so this line no longer repeats it
/// -- startup used to print the same session/provider pair twice.
pub fn breadcrumb(session_name: &str, path: &[String]) -> String {
    let mut out = session_name.to_string();
    for name in path {
        out.push_str(" \u{203a} ");
        out.push_str(name);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_session_alone_at_the_root() {
        let b = breadcrumb("main", &[]);
        assert_eq!(b, "main");
    }

    #[test]
    fn grows_one_segment_per_descent() {
        let b = breadcrumb("main", &["builder".into()]);
        assert_eq!(b, "main \u{203a} builder");

        let b = breadcrumb("main", &["builder".into(), "sub-fixer".into()]);
        assert_eq!(b, "main \u{203a} builder \u{203a} sub-fixer");
    }

    #[test]
    fn carries_no_model_attribution() {
        // Attribution is the identity strip's job; a breadcrumb that also
        // printed it was the duplicated startup presentation.
        let b = breadcrumb("main", &["builder".into()]);
        assert!(!b.contains('[') && !b.contains(']'), "{b:?}");
    }
}
