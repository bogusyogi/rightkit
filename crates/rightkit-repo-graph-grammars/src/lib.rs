//! Vendored tree-sitter grammars for languages with no crates.io crate that
//! carries the same grammar for tree-sitter-language 0.1 (ql, systemrdl, vue). See `VENDORED.md` for the
//! upstream repo/commit each grammar was vendored from and why.

use tree_sitter_language::LanguageFn;

macro_rules! vendored_language {
    ($vis_fn:ident, $extern_name:ident) => {
        extern "C" {
            fn $extern_name() -> *const ();
        }

        pub const $vis_fn: LanguageFn = unsafe { LanguageFn::from_raw($extern_name) };
    };
}

pub mod ql {
    use super::*;
    vendored_language!(LANGUAGE, tree_sitter_ql);
}

pub mod systemrdl {
    use super::*;
    vendored_language!(LANGUAGE, tree_sitter_systemrdl);
}

pub mod vue {
    use super::*;
    vendored_language!(LANGUAGE, tree_sitter_vue);
}
