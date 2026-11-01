use super::*;

pub(crate) fn plugin_prompt_context(plugin_dir: &FsPath, role: Option<&str>) -> Option<String> {
    const MAX_PLUGIN_PROMPT_BYTES: usize = 12_000;
    let role = role.unwrap_or("primary");
    let mut rows = Vec::new();
    for (manifest, _) in plugin_manifests_with_roots(plugin_dir)
        .into_iter()
        .filter(|(manifest, _)| manifest.enabled)
    {
        for prompt in manifest.prompts {
            if prompt.body.trim().is_empty() {
                continue;
            }
            if !prompt.applies_to_roles.is_empty()
                && !prompt
                    .applies_to_roles
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(role))
            {
                continue;
            }
            let title = if prompt.title.trim().is_empty() {
                prompt.id.as_str()
            } else {
                prompt.title.as_str()
            };
            rows.push(format!(
                "### Plugin `{}` / `{}`\n{}",
                manifest.id,
                title,
                prompt.body.trim()
            ));
        }
    }
    if rows.is_empty() {
        return None;
    }
    let mut block = String::from(
        "Enabled plugin prompt contributions - apply these instructions when relevant:\n\n",
    );
    block.push_str(&rows.join("\n\n"));
    if block.len() > MAX_PLUGIN_PROMPT_BYTES {
        block.truncate(MAX_PLUGIN_PROMPT_BYTES);
        block.push_str("\n\n[plugin prompt context truncated]");
    }
    Some(block)
}

/// Fable max-effort directive. Not a jailbreak — the lever is "specificity
/// through constraint" (the 2026 Agentic Coding Trends finding that harness +
/// prompt scaffolding swings results more than any single clever prompt). Append
/// it to the system prompt when fable mode is on to drive the model at full depth.
pub(crate) const FABLE_DIRECTIVE: &str = "\
<fable_mode>
You are operating in FABLE MODE — maximum reasoning depth, thoroughness, and rigor.
Do not optimize for speed or brevity of effort; optimize for being correct and complete.

- Decompose before acting. Map the full problem — inputs, edge cases, failure modes,
  and second-order effects — before writing the first line.
- No shortcuts, no \"good enough\", no silently-assumed facts. If something is unknown,
  find it (read the file, run the command) rather than guessing.
- Back every claim with concrete evidence: file:line, command output, test result.
- Prefer the complete correct solution over the fast partial one. Half-implementations
  and TODO-laden stubs are failures here.
- Verify your own work before declaring done: re-run the check, read the diff, look for
  the case you didn't handle. Adversarially critique your output as if reviewing someone
  else's, then fix what you find.
- Exhaust the task. When you think you're finished, ask \"what did I miss?\" once more.
</fable_mode>";

/// Force high effort and append [`FABLE_DIRECTIVE`] when `fable` is set.
/// Returns `(effort, system_prompt)` unchanged when off. Pure — unit-tested.
pub(crate) fn apply_fable_mode(
    fable: bool,
    effort: Option<String>,
    system_prompt: Option<String>,
) -> (Option<String>, Option<String>) {
    if !fable {
        return (effort, system_prompt);
    }
    let prompt = match system_prompt {
        Some(p) if !p.trim().is_empty() => Some(format!("{p}\n\n{FABLE_DIRECTIVE}")),
        _ => Some(FABLE_DIRECTIVE.to_string()),
    };
    (Some("high".to_string()), prompt)
}

/// Concise-output directive injected into every session and sub-agent prompt
/// when the `prompt.brief-default` config flag is on (default: true). Separate
/// from [`DEFAULT_CODING_DIRECTIVE`] — that governs *how* to code; this governs
/// *how to talk*. Enforced in prompt assembly, not an opt-in skill.
pub(crate) const BRIEF_DEFAULT_DIRECTIVE: &str = "\
<brief>
TERSE OUTPUT: write less, not more. Full grammar, real words, no filler.
Lead with the answer. No preamble, no restatement of the question, no
closing sign-off. One idea per sentence. Prose over bullets unless
structure genuinely earns it. Code blocks stay complete — trim commentary,
not code.
</brief>";

/// Append [`BRIEF_DEFAULT_DIRECTIVE`] when `brief` is on (default off in the
/// struct; the caller reads the config default). Returns `system_prompt`
/// unchanged when off. Pure — unit-tested.
pub(crate) fn apply_brief_default(brief: bool, system_prompt: Option<String>) -> Option<String> {
    if !brief {
        return system_prompt;
    }
    match system_prompt {
        Some(p) if !p.trim().is_empty() => Some(format!("{p}\n\n{BRIEF_DEFAULT_DIRECTIVE}")),
        _ => Some(BRIEF_DEFAULT_DIRECTIVE.to_string()),
    }
}

/// Always-on operating posture injected into EVERY session's system prompt at
/// the daemon level — the inbuilt equivalent of an always-active ponytail + brief
/// hook, not an opt-in skill file. Minimalism (write the least code that works)
/// and brevity (lead with the answer) are CR's defaults for every chat.
/// ponytail: unconditional by design; a kill-switch is a 1-line DaemonSettings
/// flag if a user ever needs to opt out.
pub(crate) const DEFAULT_CODING_DIRECTIVE: &str = "\
<coding_defaults>
MINIMALISM: write the least code that solves the problem. Stdlib before a dependency,
a native platform feature before custom code, one line before fifty. No speculative
abstractions, no interface with one implementation, no config nobody sets, no scaffolding
\"for later\". Deletion over addition; the first simple solution that works is the right
one. Leave one runnable check behind for non-trivial logic.
BREVITY: lead with the answer — cut preamble, restatement, and closing filler. One idea
per sentence, full grammar, real words. Prose over bullets unless structure earns it.
Code blocks stay complete.
</coding_defaults>";

/// Identity + tone floor pinned into every session system prompt (top-level
/// AND sub-agent). Without this, the model improvises identity from the
/// strongest nearby noun — the workspace/project folder name — and picks up
/// stray emoji. This block is the fix: RightKit is always RightKit,
/// regardless of what project it's pointed at.
pub(crate) const RIGHTKIT_IDENTITY_BLOCK: &str = "\
You are RightKit, a local-first AI coding agent. The workspace you are \
given is the PROJECT you operate on — it is not who you are. Never adopt \
the workspace or folder name as your own name or product identity; if you \
refer to yourself, the name is RightKit. Tone: professional, direct, and \
concise. Do not use emoji or emoticons unless the user uses them first.";

/// Immutable interpretation rule for append-only Membrane revisions. Keep it
/// in the stable identity block so cacheable prefix defines revision meaning.
pub(crate) const MEMBRANE_REVISION_INTERPRETATION: &str = "the latest membrane_context revision is current; earlier ones are history (evidence may be stale)";

#[allow(clippy::too_many_arguments)]
pub(crate) fn compose_session_system_prompt(
    instruction_context: Option<&str>,
    skill_context: Option<&str>,
    plugin_prompt_context: Option<&str>,
    agent_persona: Option<&str>,
    role_prompt: Option<&str>,
    instinct_block: Option<&str>,
    environment_snapshot: Option<&str>,
    memory_block: Option<&str>,
) -> Option<String> {
    // Always-on: every session leads with identity + tone, then the
    // minimalism + brevity posture.
    let mut parts: Vec<String> = vec![
        RIGHTKIT_IDENTITY_BLOCK.to_string(),
        DEFAULT_CODING_DIRECTIVE.to_string(),
        MEMBRANE_REVISION_INTERPRETATION.to_string(),
    ];
    if let Some(instructions) = instruction_context.filter(|s| !s.trim().is_empty()) {
        parts.push(instructions.to_string());
    }
    if let Some(skills) = skill_context.filter(|s| !s.trim().is_empty()) {
        parts.push(skills.to_string());
    }
    if let Some(plugin_prompts) = plugin_prompt_context.filter(|s| !s.trim().is_empty()) {
        parts.push(plugin_prompts.to_string());
    }
    if let Some(persona) = agent_persona.filter(|s| !s.trim().is_empty()) {
        parts.push(persona.to_string());
    }
    if let Some(role_p) = role_prompt.filter(|s| !s.trim().is_empty()) {
        parts.push(role_p.to_string());
    }
    if let Some(block) = instinct_block {
        parts.push(block.to_string());
    }
    if let Some(snapshot) = environment_snapshot.filter(|s| !s.trim().is_empty()) {
        parts.push(snapshot.to_string());
    }
    if let Some(memories) = memory_block.filter(|s| !s.trim().is_empty()) {
        parts.push(memories.to_string());
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n---\n\n"))
    }
}

#[cfg(test)]
mod identity_block_tests {
    use super::*;

    #[test]
    fn composed_prompt_starts_with_identity_block() {
        let prompt = compose_session_system_prompt(
            Some("some instructions"),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .expect("expected a composed prompt");
        assert!(
            prompt.starts_with(RIGHTKIT_IDENTITY_BLOCK),
            "identity block must be the first joined segment"
        );
        assert!(prompt.contains("You are RightKit"));
        assert!(prompt.contains("Do not use emoji"));
    }

    #[test]
    fn identity_block_present_when_any_optional_segment_is_some() {
        let cases: Vec<Option<String>> = vec![
            compose_session_system_prompt(Some("instr"), None, None, None, None, None, None, None),
            compose_session_system_prompt(None, Some("skills"), None, None, None, None, None, None),
            compose_session_system_prompt(
                None,
                None,
                Some("plugins"),
                None,
                None,
                None,
                None,
                None,
            ),
            compose_session_system_prompt(
                None,
                None,
                None,
                Some("persona"),
                None,
                None,
                None,
                None,
            ),
            compose_session_system_prompt(None, None, None, None, Some("role"), None, None, None),
            compose_session_system_prompt(
                None,
                None,
                None,
                None,
                None,
                Some("instinct"),
                None,
                None,
            ),
            compose_session_system_prompt(None, None, None, None, None, None, Some("env"), None),
            compose_session_system_prompt(None, None, None, None, None, None, None, Some("memory")),
        ];
        for case in cases {
            let prompt = case.expect("expected a composed prompt for this case");
            assert!(
                prompt.contains(RIGHTKIT_IDENTITY_BLOCK),
                "identity block must be present regardless of which optional segment is Some"
            );
        }
    }

    #[test]
    fn function_never_fabricates_emptiness_where_none_existed() {
        // The always-on DEFAULT_CODING_DIRECTIVE + identity block mean this
        // function is never actually None in practice (matches existing
        // behavior in `compose_session_system_prompt_always_injects_coding_defaults`
        // in api_tests_part_18.rs) — adding identity must not change that.
        let prompt = compose_session_system_prompt(None, None, None, None, None, None, None, None)
            .expect("all-None inputs must still produce the always-on identity + coding defaults");
        assert!(prompt.starts_with(RIGHTKIT_IDENTITY_BLOCK));
        assert!(prompt.contains("<coding_defaults>"));
    }
}
