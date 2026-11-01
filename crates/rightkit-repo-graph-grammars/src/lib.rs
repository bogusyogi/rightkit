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

#[cfg(test)]
mod tests {
    fn parse(language: tree_sitter_language::LanguageFn, source: &str) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language.into())
            .expect("vendored grammar loads with a compatible ABI");
        let tree = parser.parse(source, None).expect("parse produces a tree");
        let root = tree.root_node();
        assert!(!root.kind().is_empty());
        assert_eq!(root.end_byte(), source.len());
    }

    #[test]
    fn ql_grammar_parses_snippet() {
        parse(super::ql::LANGUAGE, "from int x where x = 1 select x\n");
    }

    #[test]
    fn systemrdl_grammar_parses_snippet() {
        parse(super::systemrdl::LANGUAGE, "addrmap top {\n};\n");
    }

    #[test]
    fn vue_grammar_parses_snippet() {
        parse(super::vue::LANGUAGE, "<template><div>hi</div></template>\n");
    }
}
