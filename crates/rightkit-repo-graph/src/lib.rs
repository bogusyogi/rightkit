//! Deterministic tree-sitter worker-input skeletonization.
//!
//! For code files over a size threshold, produces a signatures-only form.
//! Task-conditioned: function bodies relevant to the task text are kept verbatim.
//! Unsupported languages pass through unchanged.

use std::path::Path;
use tree_sitter::{Language, Node, Parser, TreeCursor};

/// Default character threshold: files under this size pass through unchanged.
pub const DEFAULT_SIZE_THRESHOLD: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
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
        "ts" | "tsx" => Some(Lang::TypeScript),
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
    }
}

/// A tracked region in the skeleton output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Region {
    /// The function/struct/class name.
    pub name: String,
    /// Byte offset in the *original* source where this item starts.
    pub source_byte_start: usize,
    /// Byte offset in the *original* source where this item ends.
    pub source_byte_end: usize,
    /// Whether the body was kept (true) or stubbed (false).
    pub kept_body: bool,
}

/// Result of skeletonization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkeletonResult {
    /// The skeletonized (or passthrough) output.
    pub output: String,
    /// Tracked regions for potential expansion.
    pub regions: Vec<Region>,
    /// Whether skeletonization was actually applied.
    pub was_skeletonized: bool,
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

    for node in items {
        let Some(item) = process_node(src, node, lang, &task_terms) else {
            continue;
        };
        regions.push(Region {
            name: item.name,
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
/// original source, or `None` if the region is not found.
pub fn expand_region(src: &str, regions: &[Region], region_name: &str) -> Option<String> {
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
fn is_task_relevant(name: &str, task_terms: &[String]) -> bool {
    let lower = name.to_lowercase();
    task_terms.iter().any(|t| {
        if lower.contains(t.as_str()) || t.contains(lower.as_str()) {
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
        Lang::JavaScript | Lang::TypeScript => process_js_node(src, node, kind, task_terms),
    }
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
        | "type_item" | "const_item" | "static_item" => {
            let name = node_name(node, src);
            let relevant = is_task_relevant(&name, task_terms);

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
        "function_declaration" | "class_declaration" | "lexical_declaration" => {
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

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL_SRC: &str = "fn main() { println!(\"hi\"); }";

    #[test]
    fn small_file_passes_through() {
        let result = skeletonize_for_task(Path::new("x.rs"), SMALL_SRC, "test", 2000);
        assert!(!result.was_skeletonized);
        assert_eq!(result.output, SMALL_SRC);
        assert!(result.regions.is_empty());
    }

    #[test]
    fn unsupported_lang_passes_through() {
        let large = "x".repeat(3000);
        let result = skeletonize_for_task(Path::new("x.zig"), &large, "test", 2000);
        assert!(!result.was_skeletonized);
    }

    #[test]
    fn rust_signatures_only_when_irrelevant() {
        let src = r#"
fn helper(x: i32) -> i32 {
    x + 1
}

fn other(y: &str) -> String {
    y.to_string()
}

struct Config {
    name: String,
    value: u32,
}
"#;
        let large_src = format!("{}\n{}", "x".repeat(2000), src);
        let result = skeletonize_for_task(Path::new("x.rs"), &large_src, "fix auth bug", 200);
        assert!(result.was_skeletonized);
        // Bodies should be stubbed — no `x + 1` or `y.to_string()`
        assert!(!result.output.contains("x + 1"));
        assert!(!result.output.contains("y.to_string()"));
        // Signatures present
        assert!(result.output.contains("fn helper"));
        assert!(result.output.contains("fn other"));
        assert!(result.output.contains("struct Config"));
    }

    #[test]
    fn rust_task_relevant_body_kept() {
        let src = r#"
fn helper(x: i32) -> i32 {
    x + 1
}

fn authenticate(token: &str) -> bool {
    token == "valid"
}
"#;
        let large_src = format!("{}\n{}", "x".repeat(2000), src);
        let result =
            skeletonize_for_task(Path::new("x.rs"), &large_src, "fix authenticate bug", 200);
        assert!(result.was_skeletonized);
        // authenticate is task-relevant, body kept
        assert!(result.output.contains("token == \"valid\""));
        // helper is not task-relevant, body stubbed
        assert!(!result.output.contains("x + 1"));
        // Region tracking
        let auth_region = result
            .regions
            .iter()
            .find(|r| r.name == "authenticate")
            .unwrap();
        assert!(auth_region.kept_body);
        let helper_region = result.regions.iter().find(|r| r.name == "helper").unwrap();
        assert!(!helper_region.kept_body);
    }

    #[test]
    fn python_task_relevant_body_kept() {
        let src = r#"
def process(data):
    return data.strip()

def authenticate(user, token):
    return token == "valid" and user.is_active
"#;
        let large_src = format!("{}\n{}", "x".repeat(2000), src);
        let result =
            skeletonize_for_task(Path::new("x.py"), &large_src, "fix authenticate flow", 200);
        assert!(result.was_skeletonized);
        // authenticate body kept
        assert!(result.output.contains("token == \"valid\""));
        // process body stubbed
        assert!(!result.output.contains("data.strip()"));
    }

    #[test]
    fn javascript_language_bridge_parses_functions() {
        let src = format!(
            "{}\nfunction authenticate(token) {{ return token === 'valid'; }}",
            "// padding\n".repeat(300)
        );
        let result = skeletonize_for_task(Path::new("x.js"), &src, "fix authenticate bug", 200);
        assert!(result.was_skeletonized);
        assert!(result.output.contains("function authenticate"));
        assert!(result.output.contains("token === 'valid'"));
    }

    #[test]
    fn typescript_task_relevant_body_kept() {
        let src = r#"
function process(data: string): string {
    return data.trim();
}

function authenticate(user: string, token: string): boolean {
    return token === "valid" && user.length > 0;
}
"#;
        let large_src = format!("{}\n{}", "x".repeat(2000), src);
        let result = skeletonize_for_task(Path::new("x.ts"), &large_src, "fix authenticate", 200);
        assert!(result.was_skeletonized);
        assert!(result.output.contains("token === \"valid\""));
        assert!(!result.output.contains("data.trim()"));
    }

    #[test]
    fn region_expansion_returns_full_body() {
        let src = r#"
fn helper(x: i32) -> i32 {
    x + 1
}

fn authenticate(token: &str) -> bool {
    token == "valid"
}
"#;
        let large_src = format!("{}\n{}", "x".repeat(2000), src);
        let result = skeletonize_for_task(Path::new("x.rs"), &large_src, "fix auth", 200);
        assert!(result.was_skeletonized);

        // Expand helper — should get the full original source text
        let expanded = expand_region(&large_src, &result.regions, "helper");
        assert!(expanded.is_some());
        let expanded = expanded.unwrap();
        assert!(expanded.contains("fn helper(x: i32) -> i32"));
        assert!(expanded.contains("x + 1"));
    }

    #[test]
    fn region_expansion_unknown_returns_none() {
        let src = "fn foo() {}";
        let result = skeletonize_for_task(Path::new("x.rs"), src, "", 0);
        assert!(expand_region(src, &result.regions, "nonexistent").is_none());
    }

    #[test]
    fn empty_input_no_panic() {
        let result = skeletonize_for_task(Path::new("x.rs"), "", "task", 0);
        assert!(!result.was_skeletonized);
    }

    #[test]
    fn rust_use_declarations_preserved() {
        let src = r#"
use std::collections::HashMap;
use std::sync::Arc;

fn main() {
    let m = HashMap::new();
}
"#;
        let large_src = format!("{}\n{}", "x".repeat(2000), src);
        let result = skeletonize_for_task(Path::new("x.rs"), &large_src, "test", 200);
        assert!(result.output.contains("use std::collections::HashMap"));
        assert!(result.output.contains("use std::sync::Arc"));
    }

    #[test]
    fn task_terms_case_insensitive() {
        let src = r#"
fn handle_auth_request(req: Request) -> Response {
    Response::ok()
}
"#;
        let large_src = format!("{}\n{}", "x".repeat(2000), src);
        // "AUTH" should match "handle_auth_request"
        let result = skeletonize_for_task(Path::new("x.rs"), &large_src, "fix AUTH bug", 200);
        assert!(result.was_skeletonized);
        assert!(result.output.contains("Response::ok()"));
    }
}
