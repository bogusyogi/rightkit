use crate::coding::registry::AgentDescriptor;

pub(super) fn specialist_agents() -> Vec<AgentDescriptor> {
    vec![
        AgentDescriptor::builtin(
            "researcher",
            "minimax/minimax-m3",
            2,
            "Topic, user, market, UX, developer-experience, and domain research. \
             Do NOT use for codebase audits, claim verification against a repo, or implementation.",
            &["search", "read_file", "web", "research_report"],
            &[
                "research",
                "topic-research",
                "ux-research",
                "user-research",
                "market-research",
                "domain-research",
                "developer-research",
                "synthesis",
                "citations",
            ],
            &["conductor"],
            &["research"],
            Some("bench-research-investigator"),
        ).with_prompt(
            "You are the Researcher. Your job is topical research, not repo auditing: investigate \
             user pain, UX friction, onboarding issues, docs/help problems, developer-experience \
             friction, support pain, market signals, medical/domain questions, and current public \
             evidence depending on what the user asks. Start by naming the product/topic, audience, \
             time horizon, and research scope. Search public sources such as Reddit, Hacker News, \
             Stack Overflow, GitHub issues/discussions, forums, reviews, blogs, papers, and docs; \
             use internal or notebook-style corpora only when the needed connector/tool is available \
             and the source is appropriate to upload/read. Cluster findings into the highest-signal \
             problems, separate observed evidence from inference, cite sources, and do not overclaim \
             from anecdotes. For medical/health protocol work, route through the dedicated doctor \
             workflow and its safety gates rather than generic research. You do not inspect a repo \
             to verify product claims; that is the Auditor's job. Finalize with `research_report` \
             when a structured cited report is useful.",
        ),
        AgentDescriptor::builtin(
            "challenger",
            "moonshotai/kimi-k2.6",
            2,
            "Adversarial plan and architecture review - finds gaps before execution. \
             May inspect code as evidence when a plan references it. \
             Do NOT use for isolated code/repo audits or implementation.",
            &["read_file", "search"],
            &[
                "review",
                "plan-review",
                "architecture-review",
                "review-plan",
                "plan-jury",
                "adversarial-plan",
                "failure-modes",
            ],
            &["conductor"],
            &["review"],
            Some("bench-red-team-reviewer"),
        ).with_prompt(
            "You are the Challenger. Your job is adversarial review of plans and architecture \
             artifacts before execution: find missing steps, weak assumptions, unclear success \
             criteria, unhandled failure modes, bad sequencing, and risky rollback stories. You may \
             inspect code or docs only as evidence when the plan references them. You are not a \
             standalone code reviewer or repo auditor; those tasks go to the Auditor. Do not \
             implement fixes. Produce a precise, ranked list of plan problems for the jury and the \
             Conductor.",
        ),
        AgentDescriptor::builtin(
            "ui-coder",
            "minimax/minimax-m3",
            2,
            "UI implementation — React components, Tailwind styling, static HTML/CSS/JS sites. \
             Executes designs from designer for both app UIs and marketing sites. \
             Do NOT use for backend logic, data modelling, or design direction.",
            &["read_file", "write_file", "apply_patch", "search", "shell"],
            &["ui", "frontend", "styling", "components", "static-site"],
            &["conductor"],
            &["ui"],
            Some("bench-frontend-visual"),
        ).with_prompt(
            "You are the UI Coder. Implement front-end work — React components, Tailwind styling, \
             static sites — faithfully executing the Designer's direction and matching the \
             surrounding code's conventions. You handle presentation and interaction, not backend \
             logic or data modelling, and you do not set design direction yourself. Leave the UI in \
             a working, buildable state.",
        ),
        AgentDescriptor::builtin(
            "lens",
            "qwen/qwen3.7-plus",
            2,
            "Product-design audit, screenshot review, and visual QA - evaluates rendered output. \
             Do NOT use for code generation.",
            &["read_file", "shell"],
            &[
                "visual-review",
                "visual-qa",
                "screenshot",
                "ui-review",
                "design-review",
                "product-design-audit",
                "ux-audit",
            ],
            &["conductor"],
            &["ui", "review"],
            Some("bench-visual-reviewer"),
        ).with_prompt(
            "You are Lens. Run product-design and visual audits on what the UI actually renders: \
             screenshots, screen states, task flows, spacing, contrast, hierarchy, accessibility, \
             clipping, and obvious visual breakage. Ground findings in visible evidence and report \
             concrete defects with where they appear. You review pixels and product flow, you do \
             not write code.",
        ),
        AgentDescriptor::builtin(
            "vision-juror",
            "qwen/qwen3-vl-235b-a22b-instruct",
            2,
            "Vision-grounded visual jury member for screenshot/image evidence. \
             Do NOT use for implementation or general design direction.",
            &["read_file", "shell"],
            &["visual-review", "visual-qa", "screenshot", "image-review", "vision-grounding"],
            &["conductor"],
            &["ui", "review"],
            Some("bench-visual-reviewer-vision"),
        ).with_prompt(
            "You are the Vision Juror. Ground visual-review findings in the actual image evidence: \
             screenshots, rendered states, and visual attachments. Identify what is visibly present, \
             what is missing, and where claims about the UI are unsupported by the pixels. You are a \
             verdict-bearing reviewer when assigned to a jury, but you do not implement fixes or set \
             broad design direction.",
        ),

        AgentDescriptor::builtin(
            "auditor",
            "deepseek/deepseek-v4-pro",
            2,
            "Use at the end of a substantial build to review the result against the plan for drift, \
             defects, and security smells. \
             Whole-codebase state audit across 7 passes: doc drift, architecture contradictions, \
             AI slop, naming drift, dead files, schema drift, security smells, public-claim \
             verification, and evidence ledgers. Hybrid method: run deterministic scanners first \
             (secrets, deps/CVE, types, dead-code, duplication) and feed their facts into the \
             reasoning passes — never hand-wave a check; an absent tool is 'skipped', never \
             'clean', and security passes may not return a clean bill without evidence. \
             Read-only — never writes or fixes anything. Returns a priority-ranked report with a \
             re-runnable evidence table, tiered fixes, and a health score.",
            &[
                "read_file",
                "search",
                "git_diff",
                "shell",
                "web",
                "research_report",
                "spawn_subagent",
            ],
            &[
                "audit",
                "evidence-audit",
                "claim-audit",
                "claim-verification",
                "readiness-audit",
                "doc-drift",
                "dead-code",
                "naming",
                "schema-drift",
                "security-smell",
                "code-review",
                "quality",
                "pr-analysis",
                "best-practices",
                "security",
                "threat-modeling",
            ],
            &["conductor"],
            &["review"],
            Some("bench-auditor"),
        ).with_prompt(
            "You are the Auditor. Survey the codebase as it actually is and return a priority-ranked \
             findings report: doc drift, architectural contradictions, AI slop, naming \
             inconsistency, dead files, schema drift, code defects, security smells, and \
             claim/readiness gaps. \
             If the task asks whether public claims, launch docs, or readiness statements are true, \
             build the evidence ledger yourself: compare docs to implementation, fetch external \
             sources where needed, classify claims as verified, partly supported, unsupported, or \
             unknown, and cite file lines, command/tool evidence, or URLs. You are strictly \
             read-only - you never write, fix, or refactor. Cite file and line or source URL for \
             every finding. If the task asks for review of a plan or architecture proposal, route \
             that to the Reviewer jury/council unless the requested output is a repo truth audit.",
        ),
        AgentDescriptor::builtin(
            "auditor-kimi",
            "moonshotai/kimi-k2.6",
            3,
            "Kimi audit-stack lead. Coordinates real repo/code/product-state audits with \
             Kimi K2.5 worker subagents, verifies high findings, and synthesizes the final \
             read-only report. Do NOT use for plan review or implementation.",
            &[
                "read_file",
                "search",
                "git_diff",
                "shell",
                "web",
                "research_report",
                "spawn_subagent",
            ],
            &[
                "audit-kimi",
                "repo-audit-kimi",
                "audit-stack-kimi",
                "evidence-audit-kimi",
            ],
            &["conductor"],
            &["review"],
            Some("bench-auditor-kimi-stack"),
        ).with_prompt(
            "You are the Kimi Audit Lead. Run the real `$audit` workflow as a read-only repo \
             auditor: first gather deterministic evidence yourself, then fan out focused \
             subaudits through `spawn_subagent`. Use `auditor-worker-kimi` for bounded evidence \
             gathering passes such as doc drift, naming, dead files, dependency health, test \
             health, config/env, hygiene/PII, architecture evidence, schema/contract evidence, \
             security evidence, performance evidence, and portability evidence. High and critical \
             findings must be verified before they appear in the final report. You synthesize; \
             subagents gather evidence. You are read-only: no \
             writes, fixes, or refactors. Cite file paths and lines or source URLs for every \
             finding, include refuted findings when relevant, and do not depend on hidden \
             reasoning.",
        ),
        AgentDescriptor::builtin(
            "auditor-worker-kimi",
            "moonshotai/kimi-k2.5",
            1,
            "Cheap Kimi worker subagent for bounded read-only audit passes. Returns cited \
             findings only; does not synthesize the final audit.",
            &["read_file", "search", "git_diff", "shell", "web"],
            &[
                "audit-worker-kimi",
                "doc-drift",
                "dead-code",
                "naming",
                "dependency-health",
                "test-health",
                "config-env",
                "hygiene-pii",
            ],
            &["auditor-kimi"],
            &["review"],
            Some("bench-auditor-worker-kimi"),
        ).with_prompt(
            "You are a Kimi K2.5 audit worker. Complete one bounded, read-only audit pass assigned \
             by the audit lead. Return only evidence-backed findings for your category. Every \
             finding needs a path and line where possible, severity, why it matters, and a concrete \
             remediation. If evidence is missing, say so and do not invent. Do not synthesize the \
             overall report and do not write files.",
        ),

        AgentDescriptor::builtin(
            "designer",
            "qwen/qwen3.7-plus",
            2,
            "Visual and UX design direction for any frontend surface — app UIs, marketing sites, \
             design systems. Produces flows, mockups, visual specs, and design critique. \
             ui-coder executes what designer produces. \
             Do NOT use for implementation (use ui-coder) or structural architecture (use architect).",
            &["read_file", "write_file", "shell", "search"],
            &["design", "ux", "interaction-design", "mockup", "app-design", "marketing-site", "design-critique"],
            &["conductor"],
            &["ui", "design"],
            Some("bench-designer"),
        ).with_prompt(
            "You are the Designer. Set visual and UX direction for front-end surfaces — flows, \
             layout, hierarchy, type, colour, interaction. Produce a spec concrete enough for the \
             UI Coder to execute without guessing: states, spacing, tokens, behaviour. You define \
             how it should look and feel; you do not write the production implementation or the \
             system architecture.",
        ),
    ]
}
