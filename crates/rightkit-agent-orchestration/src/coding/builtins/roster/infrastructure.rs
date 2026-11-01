use crate::coding::registry::AgentDescriptor;

pub(super) fn infrastructure_agents() -> Vec<AgentDescriptor> {
    vec![
        // ── Infrastructure agents (no escalation tier — always run to completion) ──
        AgentDescriptor::builtin(
            "watchdog",
            "deepseek/deepseek-v4-flash",
            1,
            "Writes LLM nudge prompts when the deterministic watchdog fires a stall trigger. \
             Never routes work — only produces nudge text.",
            &["read_file"],
            &["watchdog", "nudge"],
            &[],
            &[],
            None,
        ).with_prompt(
            "You are the Watchdog. When a run appears stalled, read the recent context and write a \
             single, targeted nudge that helps the stuck agent make progress — name the specific \
             blocker and the next concrete step. You only produce nudge text; you never route or \
             implement work.",
        ),
        AgentDescriptor::builtin(
            "memorizer",
            "deepseek/deepseek-v4-flash",
            1,
            "Post-session decision capture — writes to DecisionMemory. \
             Runs after session ends; never invoked mid-session.",
            &["read_file", "write_file"],
            &["history", "memory", "decisions"],
            &[],
            &[],
            None,
        ).with_prompt(
            "You are the Memorizer. After a session ends, distil what was decided and why into \
             durable decision memory: the choices made, the rationale, and what future sessions \
             should know. Capture decisions, not transcripts. Be concise and factual.",
        ),
        AgentDescriptor::builtin(
            "signoff",
            "z-ai/glm-4.7",
            2,
            "Validates that the stated goal is actually complete. \
             Escalates only to primary-orchestrator. Never implements anything.",
            &["read_file", "shell"],
            &["completion", "verification", "arbiter"],
            &["conductor"],
            &["completion"],
            Some("bench-completion-arbiter"),
        ).with_prompt(
            "You are Signoff — the independent verifier. Check whether the stated goal is genuinely \
             complete by inspecting the actual deliverables and the acceptance criteria, end to end. \
             A worker claiming success is not evidence; the artifacts are. Produce a goal_verification \
             judgement that states exactly what you checked, what passed, and what (if anything) \
             still fails. You never implement — you only verify.",
        ),
    ]
}
