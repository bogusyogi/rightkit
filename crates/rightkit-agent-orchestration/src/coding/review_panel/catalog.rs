use super::{PanelAuthority, PanelKind, PanelMember, ReviewPanel};

pub fn jury_for_category(category: &str) -> Option<ReviewPanel> {
    match review_family(category)?.as_str() {
        "review" => Some(reviewer_jury()),
        "visual-review" => Some(visual_review_jury()),
        _ => None,
    }
}

pub fn council_for_category(category: &str) -> Option<ReviewPanel> {
    match review_family(category)?.as_str() {
        "review" => Some(reviewer_council()),
        "visual-review" => Some(visual_review_council()),
        _ => None,
    }
}

pub(super) fn review_family(category: &str) -> Option<String> {
    let category = category.trim().to_ascii_lowercase();
    match category.as_str() {
        "review"
        | "plan-review"
        | "architecture-review"
        | "plan-jury"
        | "review-plan"
        | "adversarial-plan"
        | "failure-modes" => Some("review".into()),
        "visual-review"
        | "visual-qa"
        | "screenshot"
        | "ui-review"
        | "design-review"
        | "product-design-audit"
        | "ux-audit" => Some("visual-review".into()),
        _ => None,
    }
}

pub(super) fn member(
    id: &str,
    agent_role: Option<&str>,
    model_id: Option<&str>,
    mandate: &str,
) -> PanelMember {
    PanelMember {
        id: id.into(),
        agent_role: agent_role.map(|r| r.into()),
        model_id: model_id.map(|m| m.into()),
        mandate: mandate.into(),
    }
}

pub(super) fn reviewer_jury() -> ReviewPanel {
    ReviewPanel {
        id: "reviewer-jury".into(),
        kind: PanelKind::Jury,
        authority: PanelAuthority::Verdict,
        quorum: 2,
        purpose: "Adversarial plan and architecture review before execution.".into(),
        members: vec![
            member(
                "plan-gap-finder",
                Some("challenger"),
                Some("qwen/qwen3.7-max"),
                "Find missing steps, weak assumptions, unhandled failure modes, and unclear success criteria in the plan.",
            ),
            member(
                "architecture-risk",
                Some("architect"),
                Some("moonshotai/kimi-k2.6"),
                "Judge architectural fit, system consequences, reversibility, and whether the plan should proceed.",
            ),
            member(
                "execution-skeptic",
                Some("conductor"),
                Some("z-ai/glm-5.2"),
                "Stress-test sequencing, delegation, verification gates, rollback story, and operational risk.",
            ),
        ],
    }
}

pub(super) fn reviewer_council() -> ReviewPanel {
    ReviewPanel {
        id: "reviewer-council".into(),
        kind: PanelKind::Council,
        authority: PanelAuthority::Advisory,
        quorum: 0,
        purpose: "Internal plan preflight that refines architecture artifacts before independent review.".into(),
        members: vec![
            member(
                "scope-and-evidence-lens",
                Some("architect"),
                Some("moonshotai/kimi-k2.6"),
                "Check that the plan states scope, evidence, non-goals, and assumptions clearly.",
            ),
            member(
                "plan-risk-lens",
                Some("challenger"),
                Some("qwen/qwen3.7-max"),
                "Flag gaps, contradictions, missing verification, and likely execution traps before jury spend.",
            ),
            member(
                "operator-readiness-lens",
                Some("conductor"),
                Some("z-ai/glm-5.2"),
                "Improve handoff clarity, sequencing, rollback notes, and decision framing.",
            ),
        ],
    }
}

pub(super) fn visual_review_jury() -> ReviewPanel {
    ReviewPanel {
        id: "visual-review-jury".into(),
        kind: PanelKind::Jury,
        authority: PanelAuthority::Verdict,
        quorum: 2,
        purpose: "Rendered-surface and screenshot-based visual verdicts.".into(),
        members: vec![
            member(
                "visual-specificity",
                Some("lens"),
                Some("qwen/qwen3.7-plus"),
                "Find layout, hierarchy, spacing, and visual polish defects.",
            ),
            member(
                "severity-and-user-impact",
                Some("ui-coder"),
                Some("minimax/minimax-m3"),
                "Rank visual issues by usability and ship risk.",
            ),
            member(
                "vision-grounding",
                Some("vision-juror"),
                Some("qwen/qwen3-vl-235b-a22b-instruct"),
                "Ground findings in actual screenshot/image evidence.",
            ),
        ],
    }
}

pub(super) fn visual_review_council() -> ReviewPanel {
    ReviewPanel {
        id: "visual-review-council".into(),
        kind: PanelKind::Council,
        authority: PanelAuthority::Advisory,
        quorum: 0,
        purpose: "Internal visual preflight before verdict-bearing screenshot review.".into(),
        members: vec![
            member(
                "pixel-fit-lens",
                Some("lens"),
                Some("qwen/qwen3.7-plus"),
                "Check spacing, alignment, density, contrast, and clipping.",
            ),
            member(
                "ux-flow-lens",
                Some("designer"),
                Some("qwen/qwen3.7-plus"),
                "Check task flow, empty states, loading states, and affordances.",
            ),
            member(
                "accessibility-lens",
                Some("ui-coder"),
                Some("minimax/minimax-m3"),
                "Check keyboard, contrast, target size, and readable hierarchy.",
            ),
        ],
    }
}
