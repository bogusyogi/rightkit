//! Extended tree-sitter skeletonization variant (see [`crate::skeletonization::SkeletonVariant`]).
//!
//! Compared with the standard variant it also parses TSX with the TSX grammar (and `.mts`/`.cts`
//! as TypeScript), keeps imports verbatim, unwraps Python decorated definitions and JS/TS
//! `export` wrappers, names Rust impls `Trait for Type`, gives repeated region names a `#n`
//! suffix, keeps a first-line fallback for every other named top-level node, and uses a stricter
//! task-relevance match.

use crate::skeletonization::{Region, SkeletonResult, DEFAULT_SIZE_THRESHOLD};
use std::path::Path;
use tree_sitter::{Language, Node, Parser, TreeCursor};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
}

fn lang_for_path(path: &Path) -> Option<Lang> {
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => Some(Lang::Rust),
        "py" => Some(Lang::Python),
        "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
        "ts" | "mts" | "cts" => Some(Lang::TypeScript),
        // JSX inside TypeScript only parses with the TSX grammar.
        "tsx" => Some(Lang::Tsx),
        _ => None,
    }
}

fn ts_language(lang: Lang) -> Language {
    // Every grammar now exposes a `LANGUAGE: LanguageFn` constant instead of a
    // `language()` function, and `Language: From<LanguageFn>`. That also
    // retires the `std::mem::transmute` this function previously needed for
    // JavaScript alone -- the grammars are consistent now, so the safe
    // conversion covers all four and no unsafe block is required.
    match lang {
        Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
        Lang::Python => tree_sitter_python::LANGUAGE.into(),
        Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
        Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
    }
}

/// Skeletonize a code file for worker input.
///
/// - Files under `size_threshold` pass through unchanged.
/// - Files over the threshold: signatures-only, with task-relevant bodies kept.
/// - Unsupported languages pass through unchanged.
pub fn skeletonize_for_task(
    path: &Path,
    src: &str,
    task_text: &str,
    size_threshold: usize,
) -> SkeletonResult {
    if src.len() < size_threshold {
        return SkeletonResult {
            output: src.to_string(),
            regions: Vec::new(),
            was_skeletonized: false,
        };
    }

    let Some(lang) = lang_for_path(path) else {
        return SkeletonResult {
            output: src.to_string(),
            regions: Vec::new(),
            was_skeletonized: false,
        };
    };

    let mut parser = Parser::new();
    if parser.set_language(&ts_language(lang)).is_err() {
        return SkeletonResult {
            output: src.to_string(),
            regions: Vec::new(),
            was_skeletonized: false,
        };
    }

    let Some(tree) = parser.parse(src, None) else {
        return SkeletonResult {
            output: src.to_string(),
            regions: Vec::new(),
            was_skeletonized: false,
        };
    };

    let task_terms = extract_task_terms(task_text);
    let mut items = Vec::new();
    collect_top_level(&mut tree.walk(), &mut items);

    let mut out = Vec::new();
    let mut regions = Vec::new();
    let mut seen_names: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

    for node in items {
        let Some(item) = process_node(src, node, lang, &task_terms) else {
            continue;
        };
        // Region names are unique so `expand_region` is unambiguous: a repeated
        // name (two `impl Foo` blocks, overloads) gets `#2`, `#3`, ...
        let name = if item.name.is_empty() {
            item.name
        } else {
            let seen = seen_names.entry(item.name.clone()).or_insert(0);
            *seen += 1;
            if *seen == 1 {
                item.name
            } else {
                format!("{}#{seen}", item.name)
            }
        };
        regions.push(Region {
            name,
            source_byte_start: node.start_byte(),
            source_byte_end: node.end_byte(),
            kept_body: item.kept_body,
        });
        out.push(item.text);
    }

    if out.is_empty() {
        SkeletonResult {
            output: src.to_string(),
            regions: Vec::new(),
            was_skeletonized: false,
        }
    } else {
        SkeletonResult {
            output: out.join("\n"),
            regions,
            was_skeletonized: true,
        }
    }
}

/// Skeletonize with the default size threshold.
pub fn skeletonize(path: &Path, src: &str, task_text: &str) -> SkeletonResult {
    skeletonize_for_task(path, src, task_text, DEFAULT_SIZE_THRESHOLD)
}

/// Expand a named region: returns the full source text of that item from the
/// original source, or `None` if the region is not found. Unnamed regions
/// (imports, attributes, other top-level items) cannot be expanded by name.
pub fn expand_region(src: &str, regions: &[Region], region_name: &str) -> Option<String> {
    if region_name.is_empty() {
        return None;
    }
    let region = regions.iter().find(|r| r.name == region_name)?;
    src.get(region.source_byte_start..region.source_byte_end)
        .map(|s| s.to_string())
}

struct ProcessedItem {
    name: String,
    text: String,
    kept_body: bool,
}

/// Extract identifier-like terms from the task text for matching.
fn extract_task_terms(task_text: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let mut current = String::new();

    for ch in task_text.chars() {
        if ch.is_alphanumeric() || ch == '_' {
            current.push(ch);
        } else {
            if !current.is_empty() && current.len() >= 2 {
                terms.push(current.to_lowercase());
            }
            current.clear();
        }
    }
    if !current.is_empty() && current.len() >= 2 {
        terms.push(current.to_lowercase());
    }

    terms.sort();
    terms.dedup();
    terms
}

/// Minimum prefix length for stem-based matching between task terms and identifiers.
const MIN_STEM_PREFIX: usize = 5;

/// Check if a name is task-relevant (matches any extracted term).
///
/// Uses substring matching and prefix-stem matching (words sharing a prefix of
/// at least `MIN_STEM_PREFIX` chars are considered related, so "authenticate"
/// matches "authentication").
/// An unnamed item is never relevant (every term "contains" the empty
/// string), and a term or name under three characters ("in", "to", "is")
/// matches only a whole identifier part, never any substring.
fn is_task_relevant(name: &str, task_terms: &[String]) -> bool {
    let lower = name.to_lowercase();
    if lower.is_empty() {
        return false;
    }
    let long = |text: &str| text.chars().count() >= 3;
    task_terms.iter().any(|t| {
        if lower == *t
            || (long(t) && lower.contains(t.as_str()))
            || (long(&lower) && t.contains(lower.as_str()))
        {
            return true;
        }
        if lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|part| part == t.as_str())
        {
            return true;
        }
        // Prefix-stem matching: find the common prefix length.
        let common = lower
            .chars()
            .zip(t.chars())
            .take_while(|(a, b)| a == b)
            .count();
        common >= MIN_STEM_PREFIX
    })
}

fn collect_top_level<'a>(cursor: &mut TreeCursor<'a>, out: &mut Vec<Node<'a>>) {
    let node = cursor.node();
    if (node.kind() == "source_file" || node.kind() == "module" || node.kind() == "program")
        && cursor.goto_first_child()
    {
        loop {
            out.push(cursor.node());
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

fn process_node(src: &str, node: Node, lang: Lang, task_terms: &[String]) -> Option<ProcessedItem> {
    let kind = node.kind();
    match lang {
        Lang::Rust => process_rust_node(src, node, kind, task_terms),
        Lang::Python => process_python_node(src, node, kind, task_terms),
        Lang::JavaScript | Lang::TypeScript | Lang::Tsx => {
            process_js_node(src, node, kind, task_terms)
        }
    }
    .or_else(|| fallback_item(src, node))
}

/// A top-level item no rule above handles is never dropped in silence: a
/// one-line item stays as it is, a longer one keeps its first line and an
/// elision mark, so every declared name still shows. Comments are left out.
fn fallback_item(src: &str, node: Node) -> Option<ProcessedItem> {
    if !node.is_named() || node.kind().contains("comment") {
        return None;
    }
    let text = node.utf8_text(src.as_bytes()).ok()?.trim();
    let mut lines = text.lines();
    let first = lines.next()?.trim_end();
    let text = if lines.next().is_some() {
        format!("{first} …")
    } else {
        first.to_string()
    };
    Some(ProcessedItem {
        name: String::new(),
        text,
        kept_body: false,
    })
}

/// An item kept verbatim with no region name (imports).
fn verbatim_item(src: &str, node: Node) -> Option<ProcessedItem> {
    Some(ProcessedItem {
        name: String::new(),
        text: node.utf8_text(src.as_bytes()).ok()?.trim().to_string(),
        kept_body: false,
    })
}

/// Wrap the processed inner declaration of a wrapper node (Python decorators,
/// JS/TS `export`) with the wrapper's own leading text.
fn wrapped_item(src: &str, outer: Node, inner: Node, item: ProcessedItem) -> ProcessedItem {
    let prefix = src
        .get(outer.start_byte()..inner.start_byte())
        .unwrap_or("");
    ProcessedItem {
        name: item.name,
        text: format!("{prefix}{}", item.text),
        kept_body: item.kept_body,
    }
}

/// `const a = …` / `let a = …` / `var a = …` have no `name` field: they are
/// named by their first declarator.
fn js_item_name(node: Node, src: &str) -> String {
    if matches!(node.kind(), "lexical_declaration" | "variable_declaration") {
        let declarator = node
            .named_children(&mut node.walk())
            .find(|child| child.kind() == "variable_declarator");
        return declarator.map(|d| node_name(d, src)).unwrap_or_default();
    }
    node_name(node, src)
}

fn node_name(node: Node, src: &str) -> String {
    node.child_by_field_name("name")
        .and_then(|n| n.utf8_text(src.as_bytes()).ok())
        .unwrap_or("")
        .to_string()
}

fn process_rust_node(
    src: &str,
    node: Node,
    kind: &str,
    task_terms: &[String],
) -> Option<ProcessedItem> {
    match kind {
        "function_item" | "struct_item" | "enum_item" | "trait_item" | "impl_item" | "mod_item"
        | "type_item" | "const_item" | "static_item" | "union_item" => {
            let (name, relevant) = if kind == "impl_item" {
                // An impl has no `name`: it is named by its type (and trait).
                let text_of = |field: &str| {
                    node.child_by_field_name(field)
                        .and_then(|n| n.utf8_text(src.as_bytes()).ok())
                        .unwrap_or("")
                        .to_string()
                };
                let (ty, tr) = (text_of("type"), text_of("trait"));
                let relevant =
                    is_task_relevant(&ty, task_terms) || is_task_relevant(&tr, task_terms);
                let name = if tr.is_empty() {
                    ty
                } else {
                    format!("{tr} for {ty}")
                };
                (name, relevant)
            } else {
                let name = node_name(node, src);
                let relevant = is_task_relevant(&name, task_terms);
                (name, relevant)
            };

            if relevant {
                // Keep full body.
                let text = node.utf8_text(src.as_bytes()).ok()?.to_string();
                Some(ProcessedItem {
                    name,
                    text,
                    kept_body: true,
                })
            } else {
                // Stub the body.
                let body = node
                    .child_by_field_name("body")
                    .or_else(|| {
                        node.children(&mut node.walk())
                            .find(|c| c.kind() == "block")
                    })
                    .or_else(|| {
                        node.children(&mut node.walk())
                            .find(|c| c.kind() == "declaration_list")
                    })
                    .or_else(|| {
                        node.children(&mut node.walk())
                            .find(|c| c.kind() == "field_declaration_list")
                    });

                let text = match body {
                    Some(body) => {
                        let start = node.start_byte();
                        let body_start = body.start_byte();
                        let prefix = src.get(start..body_start).unwrap_or("").trim_end();
                        format!("{prefix}{{ … }}")
                    }
                    None => node.utf8_text(src.as_bytes()).ok()?.trim().to_string(),
                };

                Some(ProcessedItem {
                    name,
                    text,
                    kept_body: false,
                })
            }
        }
        "use_declaration" => {
            let text = node.utf8_text(src.as_bytes()).ok()?.trim().to_string();
            Some(ProcessedItem {
                name: String::new(),
                text,
                kept_body: false,
            })
        }
        _ => None,
    }
}

fn process_python_node(
    src: &str,
    node: Node,
    kind: &str,
    task_terms: &[String],
) -> Option<ProcessedItem> {
    match kind {
        "decorated_definition" => {
            let definition = node.child_by_field_name("definition")?;
            let item = process_python_node(src, definition, definition.kind(), task_terms)?;
            Some(wrapped_item(src, node, definition, item))
        }
        "import_statement" | "import_from_statement" | "future_import_statement" => {
            verbatim_item(src, node)
        }
        "function_definition" | "class_definition" => {
            let name = node_name(node, src);
            let relevant = is_task_relevant(&name, task_terms);

            if relevant {
                let text = node.utf8_text(src.as_bytes()).ok()?.to_string();
                Some(ProcessedItem {
                    name,
                    text,
                    kept_body: true,
                })
            } else {
                let text = node.utf8_text(src.as_bytes()).ok()?;
                let header = text.lines().next().unwrap_or("").trim_end();
                let stub = if header.ends_with(':') {
                    format!("{header}\n    …")
                } else {
                    format!("{header}: \n    …")
                };
                Some(ProcessedItem {
                    name,
                    text: stub,
                    kept_body: false,
                })
            }
        }
        _ => None,
    }
}

fn process_js_node(
    src: &str,
    node: Node,
    kind: &str,
    task_terms: &[String],
) -> Option<ProcessedItem> {
    match kind {
        "export_statement" => {
            let declaration = node.child_by_field_name("declaration")?;
            let item = process_js_node(src, declaration, declaration.kind(), task_terms)
                .or_else(|| fallback_item(src, declaration))?;
            Some(wrapped_item(src, node, declaration, item))
        }
        "import_statement" => verbatim_item(src, node),
        "function_declaration"
        | "generator_function_declaration"
        | "class_declaration"
        | "abstract_class_declaration"
        | "interface_declaration"
        | "enum_declaration"
        | "type_alias_declaration"
        | "lexical_declaration"
        | "variable_declaration" => {
            let name = js_item_name(node, src);
            let relevant = is_task_relevant(&name, task_terms);

            if relevant {
                let text = node.utf8_text(src.as_bytes()).ok()?.to_string();
                Some(ProcessedItem {
                    name,
                    text,
                    kept_body: true,
                })
            } else {
                let text = node.utf8_text(src.as_bytes()).ok()?;
                let first = text.lines().next().unwrap_or("").trim_end();
                let stub = if first.contains('{') {
                    let sig = first.split('{').next().unwrap_or(first).trim_end();
                    format!("{sig}{{ … }}")
                } else {
                    first.to_string()
                };
                Some(ProcessedItem {
                    name,
                    text: stub,
                    kept_body: false,
                })
            }
        }
        _ => None,
    }
}
