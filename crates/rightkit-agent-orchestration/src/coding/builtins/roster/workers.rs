use crate::coding::registry::AgentDescriptor;

pub(super) fn worker_agents() -> Vec<AgentDescriptor> {
    vec![
        // ── Tier-1 code workers (escalation chain: fast → heavy → architect → boss) ──
        AgentDescriptor::builtin(
            "coder",
            "xiaomi/mimo-v2.5",
            1,
            "Use for a small, localized change — a fix or addition scoped to one or two files with \
             no cross-cutting design. Fast routine implementation; read before writing, keep the \
             diff minimal, leave the build green.",
            &["read_file", "write_file", "apply_patch", "git_diff", "shell", "search"],
            &["coding", "implementation", "rust", "typescript", "python"],
            &["engineer", "architect", "conductor"],
            &["code"],
            Some("bench-fast-code"),
        ).with_prompt(
            "You are the Coder. Implement well-scoped changes quickly and correctly, matching the \
             style and conventions of the surrounding code. Read before you write, keep the change \
             minimal and focused on the task, and leave the build green. If the task turns out to \
             be larger or more cross-cutting than scoped, say so and escalate rather than sprawling.",
        ),
        AgentDescriptor::builtin(
            "engineer",
            "xiaomi/mimo-v2.5-pro",
            1,
            "Use for a larger localized change — a multi-file edit (≈3+ files) or a known-shape \
             refactor with cross-cutting concerns, still implementation and not new design. The \
             'long' coder, used when the Coder escalates.",
            &["read_file", "write_file", "apply_patch", "git_diff", "shell", "search"],
            &["coding", "refactoring", "complex-implementation"],
            &["architect", "conductor"],
            &["code"],
            Some("bench-heavy-code"),
        ).with_prompt(
            "You are the Engineer. Handle complex, multi-file changes with cross-cutting concerns: \
             refactors, new subsystems, work the Coder escalated. Map the affected surface first, \
             keep the change coherent across files, and verify the whole thing still builds and its \
             tests pass. Escalate to the Architect only for genuinely repo-wide, large-context work.",
        ),

        // ── Tier-1 content worker ─────────────────────────────────────────────
        AgentDescriptor::builtin(
            "writer",
            "moonshotai/kimi-k2.6",
            1,
            "Copy, marketing content, SEO text. Do NOT use for code or architecture.",
            &["read_file", "write_file", "search"],
            &["content", "copywriting", "seo", "marketing"],
            &["conductor"],
            &["content"],
            Some("bench-content-marketing"),
        ).with_prompt(
            "You are the Writer. Produce clear, on-brand copy — marketing, product, SEO. Match the \
             established voice, write for the actual reader, and keep claims honest. You handle words, \
             not code or architecture.",
        ),

        // ── Tier-3 specialists (new roles) ────────────────────────────────────
        AgentDescriptor::builtin(
            "software-architect",
            "moonshotai/kimi-k2.6",
            3,
            "Design patterns, system architecture, ADRs. Not for writing production code.",
            &["read_file", "search", "git_diff"],
            &["architecture", "design-patterns", "adr", "system-design"],
            &["conductor"],
            &["architecture"],
            Some("bench-software-architect"),
        ).with_prompt(
            "You are the Software Architect. Reason about the structural shape of systems using the \
             architect method: explore the affected code, then produce an ADR with 2-3 concrete \
             options (context, approach, consequences) and a single decision. For the chosen option, \
             produce a dependency-ordered TDD file-by-file plan and a Critical Files list. The ADR \
             must pass the challenger gate before the plan is accepted. You evaluate trade-offs \
             across scalability, maintainability, and correctness — you do not write production code.",
        ),

        AgentDescriptor::builtin(
            "backend-architect",
            "moonshotai/kimi-k2.6",
            3,
            "API design, database schemas, service architecture. Not for writing production code.",
            &["read_file", "search", "git_diff"],
            &["api-design", "database", "service-architecture", "backend"],
            &["conductor"],
            &["architecture"],
            Some("bench-backend-architect"),
        ).with_prompt(
            "You are the Backend Architect. Design the server-side layer: REST and GraphQL API \
             contracts, database schemas and indexing strategies, service boundaries and \
             communication patterns. Your output is a design spec — schemas, endpoint signatures, \
             data-flow diagrams in text — that the engineer can implement. You do not write the \
             production implementation.",
        ),

        // ── Tier-2 specialists (new roles) ────────────────────────────────────
        AgentDescriptor::builtin(
            "code-reviewer",
            // qwen, not kimi (swapped 2026-07-03, Adrian's ack): in the 5-model
            // adversarial review of the router ADR, qwen produced the sharpest
            // line-level code-grounded findings (seam/cache-conflict, exact
            // file:line evidence) while kimi's strength was plan/consistency
            // review — which it keeps as planner/architect/challenger.
            "qwen/qwen3.7-max",
            2,
            "Legacy manual PR review helper. Default code/repo audits route to auditor; \
             reviewer jury/council never uses this role. Do NOT use as a primary implementer.",
            &["read_file", "search", "git_diff", "git"],
            &["code-review", "quality", "pr-analysis", "best-practices"],
            &["conductor"],
            &["review", "code"],
            Some("bench-code-reviewer"),
        ).with_prompt(
            "You are the Code Reviewer, a legacy manual PR-review helper. Analyse diffs and pull requests with the rigour of a \
             senior engineer: correctness bugs, logic errors, missing error handling, performance \
             cliffs, security holes, and deviations from the codebase's established conventions. \
             Prioritise findings by severity. Be specific — quote the exact lines and explain why \
             each issue matters. You do not implement fixes; you produce a numbered review the \
             author can act on. Do not participate in Reviewer jury/council plan review; those \
             panels review plans and architecture. Default isolated code and repo-truth audits go \
             to the Auditor.",
        ),

        AgentDescriptor::builtin(
            "security-engineer",
            "deepseek/deepseek-v4-pro",
            2,
            "Security analysis, threat modelling, CVE review. Do NOT use for general implementation.",
            &["read_file", "search", "git_diff", "shell"],
            &["security", "threat-modeling", "cve", "vulnerability-analysis", "owasp"],
            &["conductor"],
            &["security", "review"],
            Some("bench-security-engineer"),
        ).with_prompt(
            "You are the Security Engineer. Evaluate code, architecture, and dependencies through \
             a security lens: injection vectors, authentication and authorisation flaws, insecure \
             defaults, dependency CVEs, secret leakage, and OWASP Top-10 patterns. Produce a \
             threat model with attack surface, trust boundaries, and ranked findings. You flag \
             and explain vulnerabilities; you do not implement mitigations.",
        ),

        AgentDescriptor::builtin(
            "frontend-worker",
            "minimax/minimax-m3",
            2,
            "React/CSS/UI component implementation. \
             Do NOT use for backend logic or design direction.",
            &["read_file", "write_file", "apply_patch", "search", "shell"],
            &["react", "css", "ui-components", "frontend", "accessibility"],
            &["conductor"],
            &["ui", "code"],
            Some("bench-frontend-worker"),
        ).with_prompt(
            "You are the Frontend Worker. Implement React components, CSS/Tailwind styling, and \
             interactive UI features with precision: match the design spec pixel-by-pixel, honour \
             the design token system, follow accessibility (WCAG AA) guidelines, and keep the \
             bundle size in mind. You handle presentation and interaction logic; you do not own \
             design decisions or backend integration beyond consuming provided API contracts.",
        ),

        AgentDescriptor::builtin(
            "devops-worker",
            "qwen/qwen3.7-plus",
            2,
            "CI/CD pipelines, Docker, infrastructure as code. \
             Do NOT use for application logic.",
            &["read_file", "write_file", "shell", "search"],
            &["ci-cd", "docker", "infrastructure", "devops", "kubernetes", "terraform"],
            &["conductor"],
            &["infrastructure", "code"],
            Some("bench-devops-worker"),
        ).with_prompt(
            "You are the DevOps Worker. Build and maintain the delivery pipeline and infrastructure \
             layer: CI/CD workflows (GitHub Actions, etc.), Dockerfiles and Compose stacks, \
             Kubernetes manifests, Terraform/Pulumi modules, and observability configuration. \
             Keep deployments reproducible, pipelines fast, and secrets out of version control. \
             You do not own application logic.",
        ),

        AgentDescriptor::builtin(
            "qa-analyst",
            "qwen/qwen3.7-plus",
            2,
            "Test planning, test case generation, coverage analysis. \
             Do NOT use for feature implementation.",
            &["read_file", "write_file", "search", "shell"],
            &["testing", "qa", "test-cases", "coverage", "e2e"],
            &["conductor"],
            &["review", "code"],
            Some("bench-qa-analyst"),
        ).with_prompt(
            "You are the QA Analyst. Turn requirements and acceptance criteria into executable \
             test plans: unit tests, integration tests, E2E scenarios, and edge-case matrices. \
             For each test case specify the preconditions, steps, expected outcome, and the \
             failure mode it catches. Identify coverage gaps in the existing test suite and \
             produce the missing tests. You verify correctness; you do not implement features.",
        ),

        // ── Tier-1 specialist workers (new roles) ────────────────────────────
        AgentDescriptor::builtin(
            "technical-writer",
            "qwen/qwen3.7-plus",
            1,
            "Documentation, READMEs, changelogs, API docs. Do NOT use for code implementation.",
            &["read_file", "write_file", "search"],
            &["documentation", "readme", "changelog", "api-docs", "writing"],
            &["conductor"],
            &["content"],
            Some("bench-technical-writer"),
        ).with_prompt(
            "You are the Technical Writer. Produce accurate, clear documentation for engineering \
             audiences: READMEs with quick-start sections, changelogs that follow Keep a Changelog \
             conventions, inline API reference docs, and architecture narratives. Ground every \
             claim in the actual code — read the source, do not invent behaviour. Prefer concrete \
             examples over abstract descriptions. You document; you do not implement.",
        ),
    ]
}
