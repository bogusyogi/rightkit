//! Token-bounded, in-memory repository-map ranking for worker handoffs.
//!
//! This is intentionally a small context projection, not a repository index:
//! callers supply already-admitted source text, ranking keeps only enough
//! skeletonized material for a bounded handoff payload, & nothing is persisted.

use std::collections::HashSet;
use std::path::Path;

/// Default budget for a repository map embedded in a handoff brief.
pub const DEFAULT_REPOSITORY_MAP_TOKENS: usize = 256;

/// One source file eligible for a repository map.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMapSource {
    pub path: String,
    pub content: String,
}

/// A ranked, skeletonized repository-map entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMapEntry {
    pub path: String,
    pub score: usize,
    pub content: String,
}

/// A deterministic repository map whose rendered form cannot exceed its budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMap {
    pub entries: Vec<RepositoryMapEntry>,
    pub token_estimate: usize,
}

/// Deterministic observation from one held-out context condition.
///
/// Completion is supplied by the held-out harness rather than inferred from
/// source text. Retrieval is recorded separately so proof cannot treat a
/// matching symbol in an arbitrary excerpt as task completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMapContextObservation {
    pub retrieved_path: Option<String>,
    pub completed: bool,
    pub lookup_calls: usize,
}

/// Paired map-enabled & map-disabled observations for one held-out task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMapEvaluationObservation {
    pub map_enabled: RepositoryMapContextObservation,
    pub map_disabled: RepositoryMapContextObservation,
}

/// One deterministic held-out task used to evaluate repository-map utility.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMapEvaluationCase {
    pub task_text: String,
    pub expected_path: String,
    pub expected_symbol: String,
    pub sources: Vec<RepositoryMapSource>,
    pub observation: RepositoryMapEvaluationObservation,
}

/// Per-case comparison between map-disabled & map-enabled context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMapEvaluationCaseResult {
    pub expected_path: String,
    pub map_enabled_retrieved_path: Option<String>,
    pub map_disabled_retrieved_path: Option<String>,
    pub map_enabled_completion: bool,
    pub map_disabled_completion: bool,
    pub map_enabled_lookup_calls: usize,
    pub map_disabled_lookup_calls: usize,
    pub map_token_estimate: usize,
    pub accepted: bool,
}

/// Recorded evidence that map-enabled context met S:E12 acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMapEvalProof {
    corpus_id: String,
    case_count: usize,
    token_budget: usize,
    passed: bool,
}

impl RepositoryMapEvalProof {
    /// Only a report produced by the evaluator can enable proof-gated use.
    pub fn is_usable(&self) -> bool {
        self.passed
            && !self.corpus_id.is_empty()
            && self.case_count > 0
            && self.token_budget > 0
            && self.token_budget <= DEFAULT_REPOSITORY_MAP_TOKENS
    }

    pub fn corpus_id(&self) -> &str {
        &self.corpus_id
    }

    pub fn case_count(&self) -> usize {
        self.case_count
    }

    pub fn token_budget(&self) -> usize {
        self.token_budget
    }

    pub fn passed(&self) -> bool {
        self.passed
    }
}

/// Held-out evaluation report. This harness never discovers files or calls a
/// provider; observations come from a deterministic held-out harness and are
/// never inferred from source text by this evaluator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepositoryMapEvaluationReport {
    pub proof: RepositoryMapEvalProof,
    pub cases: Vec<RepositoryMapEvaluationCaseResult>,
}

/// Compare map-disabled & map-enabled context under one token budget.
pub fn evaluate_repository_map(
    cases: impl IntoIterator<Item = RepositoryMapEvaluationCase>,
    max_tokens: usize,
    size_threshold: usize,
) -> RepositoryMapEvaluationReport {
    let cases = cases
        .into_iter()
        .map(|case| {
            let map =
                RepositoryMap::build(&case.task_text, case.sources, max_tokens, size_threshold);
            let map_enabled_retrieved_path = map.entries.first().map(|entry| entry.path.clone());
            let map_disabled_retrieved_path = case.observation.map_disabled.retrieved_path.clone();
            let map_enabled_retrieval_matches = map_enabled_retrieved_path
                == case.observation.map_enabled.retrieved_path
                && map_enabled_retrieved_path.as_deref() == Some(case.expected_path.as_str());
            // A path match alone is not useful map evidence: a too-tight
            // budget can retain only a header or an unrelated skeleton from
            // that file. The held-out target symbol must survive rendering.
            let map_enabled_symbol_matches = map.entries.iter().any(|entry| {
                entry.path == case.expected_path && entry.content.contains(&case.expected_symbol)
            });
            let map_enabled_completion = map_enabled_retrieval_matches
                && map_enabled_symbol_matches
                && case.observation.map_enabled.completed;
            let map_disabled_completion = case.observation.map_disabled.completed;
            let map_enabled_lookup_calls = case.observation.map_enabled.lookup_calls;
            let map_disabled_lookup_calls = case.observation.map_disabled.lookup_calls;
            let accepted = map.token_estimate <= max_tokens
                && map_enabled_retrieval_matches
                && map_enabled_symbol_matches
                && (map_enabled_lookup_calls < map_disabled_lookup_calls
                    || (map_enabled_completion && !map_disabled_completion));
            RepositoryMapEvaluationCaseResult {
                expected_path: case.expected_path,
                map_enabled_retrieved_path,
                map_disabled_retrieved_path,
                map_enabled_completion,
                map_disabled_completion,
                map_enabled_lookup_calls,
                map_disabled_lookup_calls,
                map_token_estimate: map.token_estimate,
                accepted,
            }
        })
        .collect::<Vec<_>>();
    let proof = RepositoryMapEvalProof {
        corpus_id: "S:E12-held-out-v1".into(),
        case_count: cases.len(),
        token_budget: max_tokens,
        passed: !cases.is_empty() && cases.iter().all(|case| case.accepted),
    };
    RepositoryMapEvaluationReport { proof, cases }
}

impl RepositoryMap {
    /// Rank caller-provided sources for `task_text` & retain a bounded map.
    ///
    /// Files are never discovered, indexed, or written by this module. Each
    /// accepted source is passed through the host app's task-conditioned
    /// skeletonizer before its map excerpt consumes budget.
    pub fn build(
        task_text: &str,
        sources: impl IntoIterator<Item = RepositoryMapSource>,
        max_tokens: usize,
        size_threshold: usize,
    ) -> Self {
        // Keep the projection bounded at its proof-gated ceiling even when a
        // caller supplies an untrusted or stale configuration value. The
        // evaluator and handoff gate both treat 256 tokens as the maximum
        // admissible budget, so construction must enforce that invariant too.
        let max_tokens = max_tokens.min(DEFAULT_REPOSITORY_MAP_TOKENS);
        if max_tokens == 0 {
            return Self {
                entries: Vec::new(),
                token_estimate: 0,
            };
        }

        let terms = task_terms(task_text);
        let sources = sources
            .into_iter()
            .filter(|source| !source.path.is_empty())
            .collect::<Vec<_>>();

        // Term relevance (task-conditioned) drives the personalization vector
        // that seeds PageRank, mirroring aider's `repomap.py`: chat/task
        // context biases which files the random walk favors, rather than the
        // walk running unpersonalized over the reference graph alone.
        let term_scores = sources
            .iter()
            .map(|source| relevance_score(&source.path, &source.content, &terms) as f64)
            .collect::<Vec<_>>();
        let pagerank_scores = personalized_pagerank(&sources, &term_scores);

        let mut ranked = sources
            .into_iter()
            .zip(term_scores)
            .zip(pagerank_scores)
            .map(|((source, term_score), pagerank_score)| {
                let skeleton = rightkit_repo_graph::skeletonize_for_task(
                    Path::new(&source.path),
                    &source.content,
                    task_text,
                    size_threshold,
                );
                // PageRank centrality (0..1) is folded in as a small tiebreak
                // over the dominant term-relevance signal so a corpus with no
                // cross-file references (the common single-source case) keeps
                // ranking purely on task relevance, while a real repository
                // graph lets referenced-by-many files surface higher.
                let combined = (term_score * 1000.0 + pagerank_score * 1000.0).round() as usize;
                RepositoryMapEntry {
                    path: source.path,
                    score: combined,
                    content: skeleton.output,
                }
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .score
                .cmp(&left.score)
                .then_with(|| left.path.cmp(&right.path))
        });

        let max_chars = max_tokens.saturating_mul(4);
        let (entries, used_chars) = fill_budget_via_binary_search(ranked, max_chars);

        Self {
            token_estimate: used_chars.div_ceil(4),
            entries,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl std::fmt::Display for RepositoryMap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        for entry in &self.entries {
            writeln!(f, "## {}", entry.path)?;
            writeln!(f, "{}", entry.content)?;
        }
        Ok(())
    }
}

fn task_terms(text: &str) -> Vec<String> {
    let mut terms = text
        .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
        .filter(|term| term.len() >= 2)
        .map(str::to_ascii_lowercase)
        .collect::<Vec<_>>();
    terms.sort();
    terms.dedup();
    terms
}

fn relevance_score(path: &str, content: &str, terms: &[String]) -> usize {
    let path = path.to_ascii_lowercase();
    let content = content.to_ascii_lowercase();
    terms.iter().fold(0usize, |score, term| {
        let path_hits = path.match_indices(term).count();
        let content_hits = content.match_indices(term).count();
        score + path_hits.saturating_mul(8) + content_hits.min(16)
    })
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    text.chars().take(max_chars).collect()
}

/// Rendered length (chars) of the top `count` ranked entries at full content,
/// i.e. `## path\ncontent\n` for each, used as the monotonic probe function
/// for the binary search below.
fn rendered_len(ranked: &[RepositoryMapEntry], count: usize) -> usize {
    ranked
        .iter()
        .take(count)
        .map(|entry| {
            format!("## {}\n", entry.path).chars().count() + 1 + entry.content.chars().count()
        })
        .sum()
}

/// Token-bounded selection via binary search over how many top-ranked entries
/// fit the budget at full content, ported from aider's `repomap.py` binary
/// search over tag count against the token budget (there: bisecting the
/// rendered-tree size against `max_map_tokens`). Entry count vs. rendered
/// size is monotonic non-decreasing, so binary search — not a linear/greedy
/// scan — finds the maximal whole-entry prefix that fits; one further entry
/// is then included truncated to spend any remaining budget.
fn fill_budget_via_binary_search(
    ranked: Vec<RepositoryMapEntry>,
    max_chars: usize,
) -> (Vec<RepositoryMapEntry>, usize) {
    if ranked.is_empty() || max_chars == 0 {
        return (Vec::new(), 0);
    }

    let mut lo = 0usize;
    let mut hi = ranked.len();
    while lo < hi {
        let mid = lo + (hi - lo).div_ceil(2);
        if rendered_len(&ranked, mid) <= max_chars {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    let full_count = lo;
    let mut used_chars = rendered_len(&ranked, full_count);
    let mut entries: Vec<RepositoryMapEntry> =
        ranked.iter().take(full_count).cloned().collect::<Vec<_>>();

    if full_count < ranked.len() {
        let mut candidate = ranked[full_count].clone();
        let header_len = format!("## {}\n", candidate.path).chars().count();
        let trailing_newline = 1;
        let budget_left = max_chars.saturating_sub(used_chars);
        if header_len + trailing_newline < budget_left {
            let remaining = budget_left - header_len - trailing_newline;
            candidate.content = truncate_chars(&candidate.content, remaining);
            if !candidate.content.is_empty() {
                used_chars += header_len + candidate.content.chars().count() + trailing_newline;
                entries.push(candidate);
            }
        }
    }

    (entries, used_chars)
}

/// Identifier-length tokens ignored as graph-edge signal: Rust/Python/JS/TS
/// keywords too common to indicate a real cross-file reference.
const IDENTIFIER_STOPWORDS: &[&str] = &[
    "fn",
    "let",
    "pub",
    "use",
    "mod",
    "impl",
    "struct",
    "enum",
    "trait",
    "self",
    "Self",
    "true",
    "false",
    "return",
    "match",
    "if",
    "else",
    "for",
    "while",
    "loop",
    "const",
    "static",
    "type",
    "where",
    "async",
    "await",
    "dyn",
    "unsafe",
    "extern",
    "move",
    "break",
    "continue",
    "yield",
    "crate",
    "super",
    "def",
    "class",
    "import",
    "from",
    "with",
    "lambda",
    "None",
    "True",
    "False",
    "function",
    "var",
    "export",
    "default",
    "interface",
    "and",
    "or",
    "not",
    "in",
    "as",
    "is",
    "the",
    "this",
    "that",
    "then",
];

/// Extract identifier-like tokens (>=3 ascii alnum/underscore chars, not a
/// stopword) that stand in for aider's tree-sitter tag defs/refs — this crate
/// has no per-language AST here, so identifier co-occurrence across file
/// boundaries approximates the reference graph aider builds from real tags.
fn extract_identifiers(content: &str) -> HashSet<String> {
    let stop: HashSet<&str> = IDENTIFIER_STOPWORDS.iter().copied().collect();
    content
        .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .filter(|tok| tok.len() >= 3 && !tok.chars().next().unwrap_or('0').is_ascii_digit())
        .filter(|tok| !stop.contains(tok))
        .map(|tok| tok.to_string())
        .collect()
}

/// Extract symbol-definition identifiers (`fn name`, `def name`, `class
/// name`, `function name`) — the def half of aider's tree-sitter tag defs/
/// refs pair. Kept deliberately narrow (definition keyword immediately
/// followed by the identifier) so this never overlaps with a same-named call
/// site, which is what makes the reference graph below directed rather than
/// symmetric.
fn extract_definitions(content: &str) -> HashSet<String> {
    const DEF_KEYWORDS: [&str; 4] = ["fn ", "def ", "class ", "function "];
    let mut defs = HashSet::new();
    for keyword in DEF_KEYWORDS {
        let mut search_from = 0usize;
        while let Some(offset) = content[search_from..].find(keyword) {
            let ident_start = search_from + offset + keyword.len();
            let ident: String = content[ident_start..]
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if ident.len() >= 3 {
                defs.insert(ident);
            }
            search_from = ident_start;
            if search_from >= content.len() {
                break;
            }
        }
    }
    defs
}

/// Personalized PageRank over a reference graph built from identifier
/// definitions & references between sources, seeded by task-relevance term
/// scores — a direct port of aider `repomap.py`'s `get_ranked_tags`: build a
/// referencer→definer graph, run `networkx.pagerank` with a personalization
/// vector biased toward chat/task-mentioned files, then use the resulting
/// per-node rank to order the map. Returns one normalized score per source,
/// same order as `sources`, in `[0, 1]` summing to ~1.0 (or uniform when the
/// graph has no edges).
fn personalized_pagerank(sources: &[RepositoryMapSource], term_scores: &[f64]) -> Vec<f64> {
    let n = sources.len();
    if n == 0 {
        return Vec::new();
    }
    if n == 1 {
        return vec![1.0];
    }

    let tokens: Vec<HashSet<String>> = sources
        .iter()
        .map(|source| extract_identifiers(&source.content))
        .collect();
    let defs: Vec<HashSet<String>> = sources
        .iter()
        .map(|source| extract_definitions(&source.content))
        .collect();

    // aider's graph direction: a file that *references* a symbol gets an
    // edge pointing at the file that *defines* it (like a citation graph),
    // so PageRank accumulates rank on symbols/files many others depend on —
    // not the reverse. `edges[referencer] = Vec<(definer, weight)>`; weight
    // is how many of the definer's defined identifiers recur as tokens in
    // the referencer's content. This distinguishes defining from merely
    // co-occurring, unlike a symmetric identifier-intersection graph, which
    // would give every pair of files sharing a name an edge in both
    // directions and erase the "who is depended upon" signal PageRank needs.
    let mut edges: Vec<Vec<(usize, f64)>> = vec![Vec::new(); n];
    let mut out_weight = vec![0.0f64; n];
    for definer in 0..n {
        if defs[definer].is_empty() {
            continue;
        }
        for referencer in 0..n {
            if definer == referencer {
                continue;
            }
            let weight = defs[definer].intersection(&tokens[referencer]).count() as f64;
            if weight > 0.0 {
                edges[referencer].push((definer, weight));
                out_weight[referencer] += weight;
            }
        }
    }

    // Personalization vector: normalized task-relevance scores, falling back
    // to uniform when no source has any task-term hits.
    let term_total: f64 = term_scores.iter().sum();
    let personalization: Vec<f64> = if term_total > 0.0 {
        term_scores.iter().map(|s| s / term_total).collect()
    } else {
        vec![1.0 / n as f64; n]
    };

    const DAMPING: f64 = 0.85;
    const ITERATIONS: usize = 50;
    const TOLERANCE: f64 = 1e-9;

    let mut rank = personalization.clone();
    for _ in 0..ITERATIONS {
        let mut next: Vec<f64> = personalization
            .iter()
            .map(|p| (1.0 - DAMPING) * p)
            .collect();

        // Dangling nodes (no outgoing edges) redistribute their rank
        // according to the personalization vector, as networkx.pagerank does.
        let dangling_mass: f64 = (0..n)
            .filter(|&i| out_weight[i] == 0.0)
            .map(|i| rank[i])
            .sum();

        for definer in 0..n {
            if out_weight[definer] == 0.0 {
                continue;
            }
            for &(referencer, weight) in &edges[definer] {
                next[referencer] += DAMPING * rank[definer] * (weight / out_weight[definer]);
            }
        }
        for (i, next_i) in next.iter_mut().enumerate() {
            *next_i += DAMPING * dangling_mass * personalization[i];
        }

        let delta: f64 = next
            .iter()
            .zip(rank.iter())
            .map(|(a, b)| (a - b).abs())
            .sum();
        rank = next;
        if delta < TOLERANCE {
            break;
        }
    }

    let total: f64 = rank.iter().sum();
    if total > 0.0 {
        rank.iter().map(|r| r / total).collect()
    } else {
        vec![1.0 / n as f64; n]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held_out_cases() -> Vec<RepositoryMapEvaluationCase> {
        vec![
            RepositoryMapEvaluationCase {
                task_text: "trace session reconnect policy".into(),
                expected_path: "src/session/reconnect.rs".into(),
                expected_symbol: "reconnect_with_backoff".into(),
                sources: vec![
                    RepositoryMapSource {
                        path: "src/cache.rs".into(),
                        content: "fn cache_lookup() {}".into(),
                    },
                    RepositoryMapSource {
                        path: "src/session/reconnect.rs".into(),
                        content: "fn reconnect_with_backoff() { /* bounded */ }".into(),
                    },
                ],
                observation: RepositoryMapEvaluationObservation {
                    map_enabled: RepositoryMapContextObservation {
                        retrieved_path: Some("src/session/reconnect.rs".into()),
                        completed: true,
                        lookup_calls: 0,
                    },
                    map_disabled: RepositoryMapContextObservation {
                        retrieved_path: None,
                        completed: false,
                        lookup_calls: 1,
                    },
                },
            },
            RepositoryMapEvaluationCase {
                task_text: "validate signed artifact receipt".into(),
                expected_path: "crates/artifacts/src/receipt.rs".into(),
                expected_symbol: "verify_receipt".into(),
                sources: vec![
                    RepositoryMapSource {
                        path: "crates/config/src/loader.rs".into(),
                        content: "fn load_config() {}".into(),
                    },
                    RepositoryMapSource {
                        path: "crates/artifacts/src/receipt.rs".into(),
                        content: "fn verify_receipt() { /* hash-bound */ }".into(),
                    },
                ],
                observation: RepositoryMapEvaluationObservation {
                    map_enabled: RepositoryMapContextObservation {
                        retrieved_path: Some("crates/artifacts/src/receipt.rs".into()),
                        completed: true,
                        lookup_calls: 0,
                    },
                    map_disabled: RepositoryMapContextObservation {
                        retrieved_path: None,
                        completed: false,
                        lookup_calls: 1,
                    },
                },
            },
        ]
    }

    #[test]
    fn ranks_task_relevant_sources_within_token_budget() {
        let map = RepositoryMap::build(
            "repair authentication",
            [
                RepositoryMapSource {
                    path: "src/cache.rs".into(),
                    content: "fn cache() {}".into(),
                },
                RepositoryMapSource {
                    path: "src/authentication.rs".into(),
                    content: "fn authenticate() {}".into(),
                },
            ],
            16,
            1,
        );

        assert_eq!(map.entries[0].path, "src/authentication.rs");
        assert!(map.token_estimate <= 16);
    }

    #[test]
    fn pagerank_favors_a_file_referenced_by_many_others_on_tied_term_relevance() {
        // Task text mentions none of the identifiers below, so term relevance
        // ties at zero for all three sources and PageRank centrality alone
        // must break the tie: `hub` is referenced by both `leaf_one` and
        // `leaf_two`, so it should out-rank either leaf.
        let map = RepositoryMap::build(
            "unrelated task text",
            [
                RepositoryMapSource {
                    path: "src/leaf_one.rs".into(),
                    content: "fn leaf_one_fn() { hub_helper(); }".into(),
                },
                RepositoryMapSource {
                    path: "src/leaf_two.rs".into(),
                    content: "fn leaf_two_fn() { hub_helper(); }".into(),
                },
                RepositoryMapSource {
                    path: "src/hub.rs".into(),
                    content: "fn hub_helper() { /* shared */ }".into(),
                },
            ],
            256,
            1,
        );

        assert_eq!(map.entries[0].path, "src/hub.rs");
    }

    #[test]
    fn pagerank_is_uniform_and_harmless_with_no_reference_graph() {
        // No source shares any identifier, so PageRank has no edges to work
        // with (uniform rank) and ranking must fall back to term relevance —
        // matches the pre-PageRank behavior this ported the algorithm onto.
        let map = RepositoryMap::build(
            "repair authentication",
            [
                RepositoryMapSource {
                    path: "src/cache.rs".into(),
                    content: "fn cache() {}".into(),
                },
                RepositoryMapSource {
                    path: "src/authentication.rs".into(),
                    content: "fn authenticate() {}".into(),
                },
            ],
            256,
            1,
        );

        assert_eq!(map.entries[0].path, "src/authentication.rs");
    }

    #[test]
    fn binary_search_budget_selects_maximal_whole_entry_prefix() {
        let ranked = vec![
            RepositoryMapEntry {
                path: "a".into(),
                score: 3,
                content: "aaaa".into(),
            },
            RepositoryMapEntry {
                path: "b".into(),
                score: 2,
                content: "bbbb".into(),
            },
            RepositoryMapEntry {
                path: "c".into(),
                score: 1,
                content: "cccccccccccccccccccc".into(),
            },
        ];
        // "## a\naaaa\n" = 10 chars, "## b\nbbbb\n" = 10 chars -> both fit
        // whole in 30, the third does not fit whole (26 more chars) but has
        // enough remaining budget (10) to be truncated and included.
        let (entries, used_chars) = fill_budget_via_binary_search(ranked, 30);

        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, "a");
        assert_eq!(entries[1].path, "b");
        assert_eq!(entries[2].path, "c");
        assert!(entries[2].content.len() < "cccccccccccccccccccc".len());
        assert!(used_chars <= 30);
    }

    #[test]
    fn binary_search_budget_empty_inputs_are_bounded() {
        assert_eq!(
            fill_budget_via_binary_search(Vec::new(), 100),
            (Vec::new(), 0)
        );
        let ranked = vec![RepositoryMapEntry {
            path: "a".into(),
            score: 1,
            content: "x".into(),
        }];
        assert_eq!(fill_budget_via_binary_search(ranked, 0), (Vec::new(), 0));
    }

    #[test]
    fn zero_budget_produces_no_map() {
        let map = RepositoryMap::build(
            "task",
            [RepositoryMapSource {
                path: "src/lib.rs".into(),
                content: "fn main() {}".into(),
            }],
            0,
            1,
        );
        assert!(map.is_empty());
    }

    #[test]
    fn held_out_eval_requires_completion_or_fewer_lookup_calls() {
        let report = evaluate_repository_map(held_out_cases(), 32, 1);

        assert!(report.proof.is_usable());
        assert!(report.proof.passed);
        assert_eq!(report.proof.case_count, 2);
        assert!(report.cases.iter().all(|case| {
            case.map_token_estimate <= report.proof.token_budget
                && case.map_enabled_completion
                && case.map_enabled_lookup_calls < case.map_disabled_lookup_calls
        }));
    }

    #[test]
    fn held_out_eval_uses_completion_observation_not_source_symbol_presence() {
        let mut cases = held_out_cases();
        cases[0].observation.map_enabled.completed = false;
        cases[0].observation.map_enabled.lookup_calls = 1;
        cases[0].observation.map_disabled.lookup_calls = 1;

        let report = evaluate_repository_map(cases, 32, 1);

        assert!(!report.proof.is_usable());
        assert!(!report.cases[0].map_enabled_completion);
        assert!(!report.cases[0].accepted);
    }

    #[test]
    fn held_out_eval_is_deterministic() {
        let left = evaluate_repository_map(held_out_cases(), 32, 1);
        let right = evaluate_repository_map(held_out_cases(), 32, 1);

        assert_eq!(left, right);
    }
}
