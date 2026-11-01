//! Conductor→agent handoff-brief compaction (WS-5, layer 4).
//!
//! When the conductor spawns a worker, it assembles a compacted **handoff brief**
//! rather than forwarding the full conductor context. The brief contains only:
//!
//! 1. The worker's task description
//! 2. A condensed goal summary
//! 3. Relevant context from completed dependency tasks (summarized, not full output)
//! 4. The agent's charter (if available)
//! 5. Scope constraints and done-when criteria
//!
//! Token estimation uses `chars / 4` as a reasonable approximation for English text.

use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use crate::coding::conductor::WorkerResult;
use crate::coding::registry::AgentDescriptor;
use crate::coding::repo_map::{RepositoryMap, RepositoryMapEvalProof, RepositoryMapSource};
use rightkit_agent_engine_protocol::TaskNode;

/// Maximum characters kept from a completed task's output when building context.
/// Prevents one large output from dominating the brief.
const MAX_CONTEXT_ITEM_CHARS: usize = 500;
const MAX_HANDOFF_SIDECAR_BYTES: usize = 8 * 1024 * 1024;
const MAX_HANDOFF_SIDECAR_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_HANDOFF_SIDECAR_ENTRIES: usize = 128;

#[derive(Debug, Clone)]
pub struct HandoffArchiveConfig {
    pub directory: PathBuf,
    pub virtual_prefix: String,
    pub max_item_bytes: usize,
    pub max_total_bytes: u64,
    pub max_entries: usize,
}

impl HandoffArchiveConfig {
    pub fn for_workspace(workspace_root: &Path) -> Self {
        Self {
            directory: workspace_root.join(".rightkit").join("handoff-results"),
            virtual_prefix: ".rightkit/handoff-results".to_string(),
            max_item_bytes: MAX_HANDOFF_SIDECAR_BYTES,
            max_total_bytes: MAX_HANDOFF_SIDECAR_TOTAL_BYTES,
            max_entries: MAX_HANDOFF_SIDECAR_ENTRIES,
        }
    }
}

/// A compacted handoff brief passed from conductor to worker instead of full context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffBrief {
    /// The primary task description for this worker.
    pub task_description: String,
    /// A condensed summary of the overall goal (1-2 sentences).
    pub goal_summary: String,
    /// Relevant context items from completed dependency tasks.
    pub context_items: Vec<ContextItem>,
    /// Ranked, skeletonized repository context for survey or synthesis work.
    pub repository_map: Option<RepositoryMap>,
    /// The agent's charter / system prompt excerpt.
    pub agent_charter: String,
    /// Explicit constraints and done-when criteria.
    pub constraints: Vec<String>,
    /// Estimated token count for this brief.
    pub token_estimate: usize,
}

/// A single context item from a completed dependency task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextItem {
    /// Identifier of the source task.
    pub source_task_id: String,
    /// Summarized output from that task (truncated to MAX_CONTEXT_ITEM_CHARS).
    pub summary: String,
    /// Content-addressed full dependency output, when the summary was folded.
    pub archive_ref: Option<String>,
    /// Workspace-relative path readable by the worker.
    pub archive_path: Option<String>,
    pub full_len: usize,
}

/// Configuration for task-conditioned skeletonization of oversized code content.
#[derive(Debug, Clone)]
pub struct SkeletonizeConfig {
    /// The task text used to determine which function bodies to keep.
    pub task_text: String,
    /// Character threshold — content under this size is not skeletonized.
    pub size_threshold: usize,
    /// Map from task_id to the source file path for that task's output.
    pub task_source_paths: HashMap<String, String>,
}

impl SkeletonizeConfig {
    /// Build the production handoff policy for a task.
    ///
    /// Skeletonization is deliberately limited to survey/synthesis work.  An
    /// implementation, edit, review, or verification task must receive exact
    /// dependency bytes because a missing body can change the requested
    /// mutation or the verifier's verdict.  Source paths are admitted only
    /// when the dependency explicitly labels its output with `Source path:`;
    /// guessing a language from code text would make the optimization unsafe.
    pub fn for_handoff(node: &TaskNode, completed_results: &[WorkerResult]) -> Option<Self> {
        if !is_survey_or_synthesis(node) || requires_exact_dependency_bytes(node) {
            return None;
        }

        let task_source_paths = completed_results
            .iter()
            .filter_map(|result| {
                explicit_source_path(&result.output)
                    .map(|path| (result.task_id.clone(), path.to_string()))
            })
            .collect::<HashMap<_, _>>();
        if task_source_paths.is_empty() {
            return None;
        }

        Some(Self {
            task_text: format!("{} {}", node.title, node.done_when),
            size_threshold: rightkit_repo_graph::DEFAULT_SIZE_THRESHOLD,
            task_source_paths,
        })
    }
}

fn is_survey_or_synthesis(node: &TaskNode) -> bool {
    let category = node.category.trim().to_ascii_lowercase();
    matches!(
        category.as_str(),
        "survey" | "synthesis" | "research" | "mapping" | "large-context"
    )
}

fn requires_exact_dependency_bytes(node: &TaskNode) -> bool {
    format!("{} {}", node.title, node.done_when)
        .split(|character: char| !character.is_ascii_alphanumeric())
        .any(|word| {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "edit"
                    | "implement"
                    | "fix"
                    | "patch"
                    | "refactor"
                    | "migrate"
                    | "delete"
                    | "verify"
                    | "verification"
                    | "test"
                    | "review"
            )
        })
}

fn explicit_source_path(output: &str) -> Option<&str> {
    let first = output.lines().next()?.trim();
    let path = first.strip_prefix("Source path:")?.trim();
    if path.is_empty()
        || Path::new(path).is_absolute()
        || path.split(['/', '\\']).any(|part| part == "..")
    {
        return None;
    }
    Some(path)
}

/// Builder for assembling a [`HandoffBrief`].
pub struct HandoffBriefBuilder<'a> {
    goal: &'a str,
    node: &'a TaskNode,
    completed_results: &'a [WorkerResult],
    agent: Option<&'a AgentDescriptor>,
    max_context_items: usize,
    skeletonize_config: Option<SkeletonizeConfig>,
    repository_map_enabled: bool,
    repository_map_proof: Option<RepositoryMapEvalProof>,
    archive_config: Option<HandoffArchiveConfig>,
}

impl<'a> HandoffBriefBuilder<'a> {
    pub fn new(goal: &'a str, node: &'a TaskNode, completed_results: &'a [WorkerResult]) -> Self {
        Self {
            goal,
            node,
            completed_results,
            agent: None,
            max_context_items: 5,
            skeletonize_config: None,
            repository_map_enabled: false,
            repository_map_proof: None,
            archive_config: None,
        }
    }

    pub fn with_agent(mut self, agent: &'a AgentDescriptor) -> Self {
        self.agent = Some(agent);
        self
    }

    pub fn with_max_context_items(mut self, max: usize) -> Self {
        self.max_context_items = max;
        self
    }

    /// Enable task-conditioned skeletonization for oversized code content.
    ///
    /// When enabled, context items with oversized code output will be skeletonized
    /// (signatures-only with task-relevant bodies kept) instead of blindly truncated.
    pub fn with_skeletonization(mut self, config: SkeletonizeConfig) -> Self {
        self.skeletonize_config = Some(config);
        self
    }

    /// Enable repository-map projection only after its held-out evaluation gate
    /// has passed. Production handoffs keep this off until that evidence exists.
    pub fn with_repository_map(mut self) -> Self {
        // Map projection is proof-gated; callers must provide evaluator output.
        self.repository_map_enabled = false;
        self
    }

    /// Enable repository-map projection using an evaluator-produced proof.
    /// Invalid or failed evidence leaves map projection disabled.
    pub fn with_repository_map_after(mut self, proof: RepositoryMapEvalProof) -> Self {
        if proof.is_usable() {
            self.repository_map_enabled = true;
            self.repository_map_proof = Some(proof);
        } else {
            self.repository_map_enabled = false;
            self.repository_map_proof = None;
        }
        self
    }

    pub fn with_archive(mut self, config: HandoffArchiveConfig) -> Self {
        self.archive_config = Some(config);
        self
    }

    /// Build the handoff brief.
    pub fn build(self) -> HandoffBrief {
        let goal_summary = summarize_goal(self.goal);
        let task_description = build_task_description(self.node);
        let context_items = self.collect_context_items();
        let repository_map = self.collect_repository_map();
        let agent_charter = self
            .agent
            .map(|a| a.system_prompt.as_str())
            .unwrap_or("")
            .to_string();
        let constraints = build_constraints(self.node);

        let mut brief = HandoffBrief {
            task_description,
            goal_summary,
            context_items,
            repository_map,
            agent_charter,
            constraints,
            token_estimate: 0,
        };
        brief.token_estimate = estimate_tokens(&brief);
        brief
    }

    /// Collect context items from completed tasks that this node depends on.
    fn collect_context_items(&self) -> Vec<ContextItem> {
        let dep_ids: std::collections::HashSet<&str> =
            self.node.deps.iter().map(|s| s.as_str()).collect();

        self.completed_results
            .iter()
            .filter(|r| dep_ids.contains(r.task_id.as_str()))
            .take(self.max_context_items)
            .map(|r| {
                let summary = match &self.skeletonize_config {
                    Some(config) => {
                        compact_code_context(&r.output, &r.task_id, config, MAX_CONTEXT_ITEM_CHARS)
                    }
                    None => truncate_and_summarize(&r.output, MAX_CONTEXT_ITEM_CHARS),
                };
                let archive = if r.output.chars().count() > MAX_CONTEXT_ITEM_CHARS {
                    self.archive_config
                        .as_ref()
                        .and_then(|config| archive_dependency_output(config, &r.output).ok())
                } else {
                    None
                };
                let summary = if archive.is_none()
                    && self.archive_config.is_some()
                    && r.output.chars().count() > MAX_CONTEXT_ITEM_CHARS
                {
                    format!("{summary} [full output not archived: bounded store refused it]")
                } else {
                    summary
                };
                ContextItem {
                    source_task_id: r.task_id.clone(),
                    summary,
                    archive_ref: archive.as_ref().map(|item| item.ref_id.clone()),
                    archive_path: archive.as_ref().map(|item| item.virtual_path.clone()),
                    full_len: r.output.len(),
                }
            })
            .collect()
    }

    /// Build a bounded repository map only from dependency sources already
    /// admitted by task-conditioned skeletonization. This keeps map discovery
    /// out of handoff construction: no filesystem walk, cache, or index exists.
    fn collect_repository_map(&self) -> Option<RepositoryMap> {
        if !self.repository_map_enabled {
            return None;
        }
        let proof = self.repository_map_proof.as_ref()?;
        if !proof.is_usable() {
            return None;
        }
        let config = self.skeletonize_config.as_ref()?;
        let dep_ids: std::collections::HashSet<&str> = self
            .node
            .deps
            .iter()
            .map(|dependency| dependency.as_str())
            .collect();
        let sources = self
            .completed_results
            .iter()
            .filter(|result| dep_ids.contains(result.task_id.as_str()))
            .filter_map(|result| {
                let path = config.task_source_paths.get(&result.task_id)?.clone();
                Some(RepositoryMapSource {
                    path,
                    content: source_body(&result.output).to_string(),
                })
            });
        let map = RepositoryMap::build(
            &config.task_text,
            sources,
            proof.token_budget(),
            config.size_threshold,
        );
        (!map.is_empty()).then_some(map)
    }
}

/// Render the brief to a payload string suitable for `WorkerTask.payload`.
impl std::fmt::Display for HandoffBrief {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "## Handoff Brief")?;

        if !self.goal_summary.is_empty() {
            writeln!(f, "\n### Goal\n{}", self.goal_summary)?;
        }

        writeln!(f, "\n### Task\n{}", self.task_description)?;

        if !self.context_items.is_empty() {
            writeln!(f, "\n### Relevant Context")?;
            for item in &self.context_items {
                writeln!(f, "[from {}]: {}", item.source_task_id, item.summary)?;
                if let (Some(reference), Some(path)) = (&item.archive_ref, &item.archive_path) {
                    writeln!(
                        f,
                        "[full output: {path}; ref:{reference}; {} bytes]",
                        item.full_len
                    )?;
                }
            }
        }

        if let Some(repository_map) = &self.repository_map {
            writeln!(f, "\n### Repository Map\n{repository_map}")?;
        }

        if !self.agent_charter.is_empty() {
            writeln!(f, "\n### Agent Charter\n{}", self.agent_charter)?;
        }

        if !self.constraints.is_empty() {
            writeln!(f, "\n### Constraints")?;
            for c in &self.constraints {
                writeln!(f, "- {}", c)?;
            }
        }

        Ok(())
    }
}

struct ArchivedDependencyOutput {
    ref_id: String,
    virtual_path: String,
}

fn archive_dependency_output(
    config: &HandoffArchiveConfig,
    output: &str,
) -> std::io::Result<ArchivedDependencyOutput> {
    if output.len() > config.max_item_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "handoff output exceeds item limit",
        ));
    }
    if config.max_entries == 0 || output.len() as u64 > config.max_total_bytes {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "handoff output exceeds aggregate archive bounds",
        ));
    }
    if config
        .directory
        .parent()
        .and_then(|parent| fs::symlink_metadata(parent).ok())
        .is_some_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "handoff archive parent must not be a symlink",
        ));
    }
    if fs::symlink_metadata(&config.directory)
        .is_ok_and(|metadata| metadata.file_type().is_symlink())
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "handoff archive directory must not be a symlink",
        ));
    }
    fs::create_dir_all(&config.directory)?;
    if let Some(host_dir) = config.directory.parent() {
        let gitignore = host_dir.join(".gitignore");
        if !gitignore.exists() {
            fs::write(gitignore, "*\n")?;
        }
    }

    let ref_id = rightkit_fs::digest::sha256_bytes(output.as_bytes());
    let path = config.directory.join(format!("{ref_id}.txt"));
    match rightkit_fs::atomic_write_with_options(
        &path,
        rightkit_fs::AtomicWriteOptions {
            replace: rightkit_fs::ReplaceMode::NoReplace,
            ..rightkit_fs::AtomicOptions::default().into()
        },
        |file| std::io::Write::write_all(file, output.as_bytes()),
    ) {
        Ok(()) => {}
        Err(rightkit_fs::FsError::AlreadyExists(_)) => {
            let metadata = fs::symlink_metadata(&path)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() != output.len() as u64
                || fs::read(&path)? != output.as_bytes()
            {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "handoff sidecar path is not the expected content-addressed file",
                ));
            }
        }
        Err(error) => return Err(error.into()),
    }
    prune_handoff_archive(config, &ref_id)?;
    Ok(ArchivedDependencyOutput {
        virtual_path: format!(
            "{}/{}.txt",
            config.virtual_prefix.trim_end_matches('/'),
            ref_id
        ),
        ref_id,
    })
}

fn prune_handoff_archive(config: &HandoffArchiveConfig, keep_ref: &str) -> std::io::Result<()> {
    let mut entries = fs::read_dir(&config.directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = entry.metadata().ok()?;
            if !metadata.is_file() || path.extension().and_then(|ext| ext.to_str()) != Some("txt") {
                return None;
            }
            Some((path, metadata.len(), metadata.modified().ok()))
        })
        .collect::<Vec<_>>();
    entries.sort_by_key(|(_, _, modified)| *modified);
    let mut total = entries.iter().map(|(_, len, _)| *len).sum::<u64>();
    let mut count = entries.len();
    for (path, len, _) in entries {
        if total <= config.max_total_bytes && count <= config.max_entries {
            break;
        }
        if path.file_stem().and_then(|stem| stem.to_str()) == Some(keep_ref) {
            continue;
        }
        if fs::remove_file(path).is_ok() {
            total = total.saturating_sub(len);
            count = count.saturating_sub(1);
        }
    }
    let remaining = fs::read_dir(&config.directory)?
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let ty = entry.file_type().ok()?;
            let metadata = entry.metadata().ok()?;
            (ty.is_file() && entry.path().extension().and_then(|ext| ext.to_str()) == Some("txt"))
                .then_some(metadata.len())
        })
        .collect::<Vec<_>>();
    if remaining.len() > config.max_entries
        || remaining.iter().sum::<u64>() > config.max_total_bytes
    {
        return Err(std::io::Error::other(
            "handoff archive could not be pruned to its configured bounds",
        ));
    }
    Ok(())
}

/// Summarize the overall goal into 1-2 sentences.
fn summarize_goal(goal: &str) -> String {
    let trimmed = goal.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    // Take the first two sentences as the summary.
    let mut end = 0;
    let mut sentence_count = 0;
    for (i, ch) in trimmed.char_indices() {
        if matches!(ch, '.' | '?' | '!') {
            sentence_count += 1;
            end = i + ch.len_utf8();
            if sentence_count >= 2 {
                break;
            }
        }
    }
    if end == 0 {
        // No sentence-ending punctuation found; take first 200 chars.
        trimmed.chars().take(200).collect()
    } else {
        trimmed[..end].to_string()
    }
}

/// Build the task description from the task node.
fn build_task_description(node: &TaskNode) -> String {
    let mut desc = node.title.clone();
    if !node.done_when.is_empty() {
        desc.push_str(&format!("\nDone when: {}", node.done_when));
    }
    desc
}

/// Build explicit constraints from the task node.
fn build_constraints(node: &TaskNode) -> Vec<String> {
    let mut constraints = Vec::new();

    if !node.assigned_role.is_empty() {
        constraints.push(format!("Acting as role: {}", node.assigned_role));
    }

    if !node.category.is_empty() {
        constraints.push(format!("Task category: {}", node.category));
    }

    if !node.escalation_path.is_empty() {
        constraints.push(format!("Escalation path: {}", node.escalation_path));
    }

    constraints
}

/// Truncate output to `max_chars` and append an ellipsis marker if truncated.
fn truncate_and_summarize(output: &str, max_chars: usize) -> String {
    if output.chars().count() <= max_chars {
        output.to_string()
    } else {
        let truncated: String = output.chars().take(max_chars).collect();
        format!("{truncated}... [truncated]")
    }
}

/// Compact a code context item using skeletonization when a source path is available.
///
/// If the task has a mapped source path and the output exceeds the skeletonization
/// threshold, applies tree-sitter skeletonization (signatures-only with task-relevant
/// bodies kept). Otherwise falls back to truncation.
fn compact_code_context(
    output: &str,
    task_id: &str,
    config: &SkeletonizeConfig,
    max_chars: usize,
) -> String {
    let Some(source_path) = config.task_source_paths.get(task_id) else {
        return truncate_and_summarize(output, max_chars);
    };

    let path = Path::new(source_path);
    let code = source_body(output);
    let result = rightkit_repo_graph::skeletonize_for_task(
        path,
        code,
        &config.task_text,
        config.size_threshold,
    );

    if result.was_skeletonized {
        // Skeletonized content — still truncate if it exceeds max_chars
        if result.output.len() <= max_chars {
            result.output
        } else {
            let truncated: String = result.output.chars().take(max_chars).collect();
            format!("{truncated}... [skeletonized, truncated]")
        }
    } else {
        // Not skeletonized (too small or unsupported lang) — use normal truncation
        truncate_and_summarize(output, max_chars)
    }
}

fn source_body(output: &str) -> &str {
    output
        .lines()
        .next()
        .and_then(|line| line.trim().strip_prefix("Source path:"))
        .map(|_| output.split_once('\n').map_or("", |(_, rest)| rest))
        .unwrap_or(output)
}

/// Estimate token count for a brief. Uses `chars / 4` as a reasonable
/// approximation for English text (1 token ≈ 4 chars ≈ 0.75 words).
fn estimate_tokens(brief: &HandoffBrief) -> usize {
    let total_chars = brief.task_description.len()
        + brief.goal_summary.len()
        + brief.agent_charter.len()
        + brief
            .context_items
            .iter()
            .map(|c| c.summary.len())
            .sum::<usize>()
        + brief
            .repository_map
            .as_ref()
            .map(|map| format!("{map}").len())
            .unwrap_or_default()
        + brief.constraints.iter().map(|c| c.len()).sum::<usize>()
        + 100; // overhead for markdown formatting

    total_chars.div_ceil(4) // ceiling division
}

/// Estimate how many tokens a naive payload (the old `task_payload` style)
/// would consume if it carried the full conductor context.
///
/// This is the "baseline" for measuring reduction. We model the baseline as:
/// - The goal text (full, not summarized)
/// - All completed task outputs (full, not truncated)
/// - The task description
/// - Overhead for the full transcript framing
pub fn estimate_baseline_tokens(
    goal: &str,
    node: &TaskNode,
    completed_results: &[WorkerResult],
) -> usize {
    let mut total_chars = goal.len() + node.title.len() + node.done_when.len();
    for result in completed_results {
        total_chars += result.output.len();
        total_chars += result.task_id.len() + 20; // framing overhead per result
    }
    total_chars += 200; // transcript framing overhead
    total_chars.div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::coding::repo_map::{
        evaluate_repository_map, RepositoryMapContextObservation, RepositoryMapEvaluationCase,
        RepositoryMapEvaluationObservation, RepositoryMapSource,
    };
    use rightkit_agent_engine_protocol::TaskState;

    fn make_node(id: &str, deps: &[&str], title: &str, done_when: &str) -> TaskNode {
        TaskNode {
            id: id.into(),
            title: title.into(),
            deps: deps.iter().map(|s| s.to_string()).collect(),
            done_when: done_when.into(),
            done_when_predicates: vec![],
            state: TaskState::Open,
            category: "coding".into(),
            estimated_duration: String::new(),
            assigned_role: "coder".into(),
            escalation_path: "engineer,architect".into(),
            open_questions: vec![],
            created_at: "t0".into(),
            updated_at: "t1".into(),
        }
    }

    fn make_result(task_id: &str, output: &str) -> WorkerResult {
        WorkerResult {
            task_id: task_id.into(),
            agent: "coder".into(),
            success: true,
            output: output.into(),
            input_tokens: 0,
            mop_fired: false,
            failure_kind: None,
            agent_version: None,
            rollback_step: None,
            outcome: crate::coding::conductor::WorkerOutcome::Complete,
        }
    }

    fn make_agent(name: &str, charter: &str) -> AgentDescriptor {
        let mut agent = AgentDescriptor::new(name, name, "m");
        agent.system_prompt = charter.into();
        agent
    }

    #[test]
    fn brief_contains_task_description_and_done_when() {
        let node = make_node("t1", &[], "Implement auth module", "tests pass");
        let brief = HandoffBriefBuilder::new("Ship the feature", &node, &[]).build();

        assert!(brief.task_description.contains("Implement auth module"));
        assert!(brief.task_description.contains("Done when: tests pass"));
    }

    #[test]
    fn brief_contains_goal_summary() {
        let node = make_node("t1", &[], "task", "");
        let brief =
            HandoffBriefBuilder::new("Build a secure auth system. Use OAuth2.", &node, &[]).build();

        assert!(brief.goal_summary.contains("Build a secure auth system."));
    }

    #[test]
    fn brief_includes_context_from_dependency_tasks() {
        let node = make_node("t2", &["t1"], "Build API layer", "");
        let results = vec![make_result(
            "t1",
            "Auth module implemented with JWT tokens and refresh flow.",
        )];
        let brief = HandoffBriefBuilder::new("goal", &node, &results).build();

        assert_eq!(brief.context_items.len(), 1);
        assert_eq!(brief.context_items[0].source_task_id, "t1");
        assert!(brief.context_items[0]
            .summary
            .contains("Auth module implemented"));
    }

    #[test]
    fn brief_excludes_context_from_non_dependency_tasks() {
        let node = make_node("t3", &["t1"], "Build UI", "");
        let results = vec![
            make_result("t1", "Auth done."),
            make_result("t2", "API done."), // not a dep of t3
        ];
        let brief = HandoffBriefBuilder::new("goal", &node, &results).build();

        assert_eq!(brief.context_items.len(), 1);
        assert_eq!(brief.context_items[0].source_task_id, "t1");
    }

    #[test]
    fn brief_truncates_large_output() {
        let node = make_node("t2", &["t1"], "task", "");
        let large_output = "x".repeat(2000);
        let results = vec![make_result("t1", &large_output)];
        let brief = HandoffBriefBuilder::new("goal", &node, &results).build();

        assert!(brief.context_items[0].summary.len() <= MAX_CONTEXT_ITEM_CHARS + 20);
        assert!(brief.context_items[0].summary.contains("[truncated]"));
    }

    #[test]
    fn live_handoff_archive_keeps_folded_dependency_recoverable() {
        let temp = tempfile::tempdir().unwrap();
        let node = make_node("t2", &["t1"], "task", "");
        let output = (0..200)
            .map(|index| format!("line-{index:03}"))
            .collect::<Vec<_>>()
            .join("\n");
        let results = vec![make_result("t1", &output)];
        let config = HandoffArchiveConfig::for_workspace(temp.path());

        let brief = HandoffBriefBuilder::new("goal", &node, &results)
            .with_archive(config.clone())
            .build();
        let item = &brief.context_items[0];
        let reference = item.archive_ref.as_ref().expect("archive ref");
        let path = config.directory.join(format!("{reference}.txt"));

        assert_eq!(fs::read_to_string(path).unwrap(), output);
        assert_eq!(item.full_len, output.len());
        assert!(format!("{brief}").contains("[full output:"));
        assert_eq!(
            fs::read_to_string(temp.path().join(".rightkit/.gitignore")).unwrap(),
            "*\n"
        );
    }

    #[test]
    fn handoff_archive_refuses_oversized_dependency_honestly() {
        let temp = tempfile::tempdir().unwrap();
        let node = make_node("t2", &["t1"], "task", "");
        let results = vec![make_result("t1", &"x".repeat(600))];
        let mut config = HandoffArchiveConfig::for_workspace(temp.path());
        config.max_item_bytes = 599;

        let brief = HandoffBriefBuilder::new("goal", &node, &results)
            .with_archive(config)
            .build();
        let item = &brief.context_items[0];

        assert!(item.archive_ref.is_none());
        assert!(item.summary.contains("not archived"));
    }

    #[test]
    fn brief_includes_agent_charter() {
        let node = make_node("t1", &[], "task", "");
        let agent = make_agent("coder", "You are a senior Rust developer.");
        let brief = HandoffBriefBuilder::new("goal", &node, &[])
            .with_agent(&agent)
            .build();

        assert_eq!(brief.agent_charter, "You are a senior Rust developer.");
    }

    #[test]
    fn brief_includes_constraints() {
        let node = make_node("t1", &[], "task", "all tests green");
        let brief = HandoffBriefBuilder::new("goal", &node, &[]).build();

        assert!(brief.constraints.iter().any(|c| c.contains("coder")));
        assert!(brief.constraints.iter().any(|c| c.contains("coding")));
        assert!(brief
            .constraints
            .iter()
            .any(|c| c.contains("engineer,architect")));
    }

    #[test]
    fn brief_token_estimate_is_positive() {
        let node = make_node("t1", &[], "Implement feature X", "tests pass");
        let brief = HandoffBriefBuilder::new("Build the whole app", &node, &[]).build();

        assert!(brief.token_estimate > 0);
    }

    #[test]
    fn brief_renders_to_string_with_sections() {
        let node = make_node("t1", &[], "task", "");
        let brief = HandoffBriefBuilder::new("goal", &node, &[]).build();
        let rendered = format!("{brief}");

        assert!(rendered.contains("## Handoff Brief"));
        assert!(rendered.contains("### Task"));
    }

    #[test]
    fn context_item_limit_is_respected() {
        let node = make_node("t5", &["t1", "t2", "t3", "t4"], "task", "");
        let results = vec![
            make_result("t1", "a"),
            make_result("t2", "b"),
            make_result("t3", "c"),
            make_result("t4", "d"),
        ];
        let brief = HandoffBriefBuilder::new("goal", &node, &results)
            .with_max_context_items(2)
            .build();

        assert_eq!(brief.context_items.len(), 2);
    }

    #[test]
    fn empty_goal_produces_empty_summary() {
        let node = make_node("t1", &[], "task", "");
        let brief = HandoffBriefBuilder::new("", &node, &[]).build();

        assert!(brief.goal_summary.is_empty());
    }

    #[test]
    fn token_reduction_vs_baseline() {
        // Simulate a realistic scenario: conductor has 8 completed task outputs,
        // each ~1000 chars, and a 500-char goal. The brief should be much smaller.
        let goal = "Build a complete authentication system with OAuth2, JWT tokens, \
                     refresh flows, role-based access control, and comprehensive tests. \
                     The system must support multiple providers including Google, GitHub, \
                     and Microsoft. Security audit must pass before deployment.";
        let node = make_node(
            "t9",
            &["t1", "t2", "t3"],
            "Integrate auth with API gateway",
            "all integration tests pass",
        );

        // Baseline: all 8 completed outputs passed in full (simulating full transcript)
        let all_results: Vec<WorkerResult> = (1..=8)
            .map(|i| {
                make_result(
                    &format!("t{i}"),
                    &format!("Output from task {i}: {}", "detail ".repeat(150)),
                )
            })
            .collect();

        let baseline_tokens = estimate_baseline_tokens(goal, &node, &all_results);

        // Brief: only dependency outputs (t1, t2, t3), truncated
        let brief = HandoffBriefBuilder::new(goal, &node, &all_results).build();

        assert!(
            brief.token_estimate < baseline_tokens,
            "brief tokens ({}) should be less than baseline ({})",
            brief.token_estimate,
            baseline_tokens,
        );

        let reduction_pct = (1.0 - brief.token_estimate as f64 / baseline_tokens as f64) * 100.0;
        assert!(
            reduction_pct > 30.0,
            "expected >30% reduction, got {:.1}%",
            reduction_pct
        );
    }

    #[test]
    fn skeletonized_context_uses_signatures_for_oversized_code() {
        // Simulate a large Rust file as task output
        let rust_code = format!(
            r#"fn helper(x: i32) -> i32 {{
    x + 1
}}

fn authenticate(token: &str) -> bool {{
    token == "valid"
}}

fn process(data: Vec<u8>) -> Vec<u8> {{
    data.into_iter().rev().collect()
}}

fn validate(input: &str) -> Result<(), String> {{
    if input.is_empty() {{
        return Err("empty".into());
    }}
    Ok(())
}}
{}
"#,
            "// padding\n".repeat(200)
        );

        let node = make_node("t2", &["t1"], "Fix authentication bug", "auth tests pass");
        let results = vec![make_result("t1", &rust_code)];

        let mut source_paths = std::collections::HashMap::new();
        source_paths.insert("t1".to_string(), "src/auth.rs".to_string());

        let config = SkeletonizeConfig {
            task_text: "Fix authentication bug".to_string(),
            size_threshold: 200,
            task_source_paths: source_paths,
        };

        let brief = HandoffBriefBuilder::new("Build secure auth", &node, &results)
            .with_skeletonization(config)
            .build();

        assert_eq!(brief.context_items.len(), 1);
        let summary = &brief.context_items[0].summary;

        // authenticate body should be kept (task-relevant)
        assert!(
            summary.contains("token == \"valid\""),
            "task-relevant body should be kept: {summary}"
        );

        // helper, process, validate bodies should be stubbed
        assert!(
            !summary.contains("x + 1"),
            "irrelevant body should be stubbed: {summary}"
        );
        assert!(
            !summary.contains("into_iter().rev()"),
            "irrelevant body should be stubbed: {summary}"
        );

        // Signatures should still be present
        assert!(
            summary.contains("fn helper"),
            "signature missing: {summary}"
        );
        assert!(
            summary.contains("fn process"),
            "signature missing: {summary}"
        );
    }

    #[test]
    fn production_policy_skeletonizes_only_survey_with_explicit_relative_source() {
        let survey = make_node("t2", &["t1"], "Map authentication", "return a survey");
        let mut survey = survey;
        survey.category = "survey".into();
        let results = vec![make_result(
            "t1",
            &format!(
                "Source path: src/auth.rs\n{}",
                "fn auth() { true; }\n".repeat(300)
            ),
        )];

        let config = SkeletonizeConfig::for_handoff(&survey, &results).expect("survey policy");
        assert_eq!(config.task_source_paths["t1"], "src/auth.rs");

        for category in ["coding", "implementation", "review", "verification"] {
            let mut exact = survey.clone();
            exact.category = category.into();
            assert!(SkeletonizeConfig::for_handoff(&exact, &results).is_none());
        }
        for title in [
            "Edit the authentication module",
            "Verify the authentication map",
        ] {
            let mut exact = survey.clone();
            exact.title = title.into();
            assert!(SkeletonizeConfig::for_handoff(&exact, &results).is_none());
        }

        let unsafe_result = vec![make_result("t1", "Source path: ../secret.rs\nfn x() {}")];
        assert!(SkeletonizeConfig::for_handoff(&survey, &unsafe_result).is_none());
    }

    #[test]
    fn direct_repository_map_enablement_stays_off_without_proof() {
        let mut node = make_node("t3", &["t1", "t2"], "Map authentication", "return survey");
        node.category = "survey".into();
        let results = vec![
            make_result(
                "t1",
                "Source path: src/cache.rs\nfn cache_value() {\n    let x = 1;\n}\n",
            ),
            make_result(
                "t2",
                "Source path: src/authentication.rs\nfn authenticate() {\n    let x = 1;\n}\n",
            ),
        ];
        let config = SkeletonizeConfig::for_handoff(&node, &results).expect("survey policy");

        let brief = HandoffBriefBuilder::new("map auth", &node, &results)
            .with_skeletonization(config)
            .with_repository_map()
            .build();
        assert!(brief.repository_map.is_none());
    }

    #[test]
    fn production_handoff_keeps_repository_map_off_until_eval_gate_passes() {
        let mut node = make_node("t2", &["t1"], "Map authentication", "return survey");
        node.category = "survey".into();
        let results = vec![make_result(
            "t1",
            "Source path: src/authentication.rs\nfn authenticate() {}\n",
        )];
        let config = SkeletonizeConfig::for_handoff(&node, &results).expect("survey policy");

        let brief = HandoffBriefBuilder::new("map auth", &node, &results)
            .with_skeletonization(config)
            .build();

        assert!(brief.repository_map.is_none());
    }

    #[test]
    fn proof_gated_repository_map_requires_passed_eval() {
        let mut node = make_node("t2", &["t1"], "Map sessions", "return survey");
        node.category = "survey".into();
        let results = vec![make_result(
            "t1",
            "Source path: src/session.rs\nfn reconnect_session() {}\n",
        )];
        let config = SkeletonizeConfig::for_handoff(&node, &results).expect("survey policy");
        let report = evaluate_repository_map(
            vec![RepositoryMapEvaluationCase {
                task_text: "map session reconnect".into(),
                expected_path: "src/session.rs".into(),
                expected_symbol: "reconnect_session".into(),
                sources: vec![RepositoryMapSource {
                    path: "src/session.rs".into(),
                    content: "fn reconnect_session() {}".into(),
                }],
                observation: RepositoryMapEvaluationObservation {
                    map_enabled: RepositoryMapContextObservation {
                        retrieved_path: Some("src/session.rs".into()),
                        completed: true,
                        lookup_calls: 0,
                    },
                    map_disabled: RepositoryMapContextObservation {
                        retrieved_path: None,
                        completed: false,
                        lookup_calls: 1,
                    },
                },
            }],
            32,
            1,
        );
        assert!(report.proof.is_usable());

        let brief = HandoffBriefBuilder::new("map sessions", &node, &results)
            .with_skeletonization(config)
            .with_repository_map_after(report.proof)
            .build();
        assert!(brief.repository_map.is_some());
    }
}
