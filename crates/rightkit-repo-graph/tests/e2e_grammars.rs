//! End-to-end: every grammar Blueprint loads, from a crate or from the vendored set, links through its C FFI entry point,
//! loads into a real tree-sitter parser, and parses representative source into
//! a tree that spans the input. This is the exact path the Blueprint indexer
//! takes when it resolves a language for a file.

use rightkit_repo_graph_grammars::{ql, systemrdl, vue};
use tree_sitter::{Language, Parser};

fn parse(grammar: impl Into<Language>, source: &str) -> tree_sitter::Tree {
    let language: Language = grammar.into();
    let mut parser = Parser::new();
    parser
        .set_language(&language)
        .expect("grammar ABI is compatible with the linked tree-sitter");
    parser
        .parse(source, None)
        .expect("parser produces a tree for in-memory source")
}

#[test]
fn every_catalog_grammar_parses_representative_source_into_a_spanning_tree() {
    let cases: [(&str, Language, &str, bool); 8] = [
        (
            "elisp",
            tree_sitter_elisp::LANGUAGE.into(),
            "(defun add-one (x)\n  (+ x 1))\n",
            true,
        ),
        (
            "embedded_template",
            tree_sitter_embedded_template::LANGUAGE.into(),
            "<div><%= name %></div>\n",
            true,
        ),
        (
            "ql",
            ql::LANGUAGE.into(),
            "from int i where i = 1 select i\n",
            false,
        ),
        (
            "rescript",
            arborium_rescript::language().into(),
            "let add = (a, b) => a + b\n",
            true,
        ),
        (
            "solidity",
            tree_sitter_solidity::LANGUAGE.into(),
            "contract Vault {\n    uint256 public total;\n}\n",
            true,
        ),
        (
            "systemrdl",
            systemrdl::LANGUAGE.into(),
            "addrmap top {\n};\n",
            false,
        ),
        (
            "tlaplus",
            tree_sitter_tlaplus::LANGUAGE.into(),
            "---- MODULE Spec ----\n====\n",
            false,
        ),
        (
            "vue",
            vue::LANGUAGE.into(),
            "<template>\n  <div>hi</div>\n</template>\n",
            true,
        ),
    ];
    for (name, grammar, source, expect_clean) in cases {
        let tree = parse(grammar, source);
        let root = tree.root_node();
        assert!(
            root.end_byte() > 0 && root.end_byte() <= source.len(),
            "{name}: root spans input"
        );
        assert!(
            root.child_count() > 0,
            "{name}: parsed something: {}",
            root.to_sexp()
        );
        if expect_clean {
            assert!(
                !root.has_error(),
                "{name}: clean parse expected: {}",
                root.to_sexp()
            );
        }
    }
}

#[test]
fn one_parser_retargets_between_grammars_and_broken_source_yields_an_error_tree() {
    // The same parser can be retargeted between grammars without cross-talk.
    let language = Language::from(tree_sitter_solidity::LANGUAGE);
    let mut parser = Parser::new();
    parser.set_language(&language).unwrap();
    let first = parser.parse("contract A {}\n", None).unwrap();
    parser
        .set_language(&Language::from(arborium_rescript::language()))
        .unwrap();
    let second = parser.parse("let x = 1\n", None).unwrap();
    assert!(!first.root_node().has_error() && !second.root_node().has_error());
    assert_ne!(first.root_node().to_sexp(), second.root_node().to_sexp());

    // Garbage never panics or hangs: it yields an error-bearing tree.
    let broken = parse(tree_sitter_solidity::LANGUAGE, "contract {{{ ;;; )))\n");
    assert!(broken.root_node().has_error());
}
