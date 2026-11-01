import { SkillProposalMeta, skillProvenanceSeverityClass } from "@rightkit/agent-ui-settings/components/settings/tabs/skills/SkillShared";
import type { SkillProvenanceReviewPanelProps } from "./types";

export function SkillProvenanceReviewPanel({ review }: SkillProvenanceReviewPanelProps) {
  return (
    <section className="grid gap-3 rounded-lg border border-line bg-panel px-3 py-3">
      <div className="grid gap-2 md:grid-cols-4">
        <SkillProposalMeta label="Provenance" value={review.ok ? "passed" : "needs review"} />
        <SkillProposalMeta label="Lifecycle" value={review.recommendedLifecycle} />
        <SkillProposalMeta label="Manifest" value={review.manifestName || "unknown"} />
        <SkillProposalMeta label="Hash" value={review.contentHash} />
      </div>
      <div className="grid gap-1 text-[11px] text-text-3">
        <span className="truncate">{review.normalizedSourceUrl || review.sourceUrl}</span>
        {review.findings.map((finding) => (
          <div
            className="grid grid-cols-[72px_minmax(120px,0.55fr)_minmax(0,1fr)] gap-2 rounded border border-line bg-bg px-2 py-1"
            key={`${finding.severity}:${finding.code}:${finding.message}`}
          >
            <code className={skillProvenanceSeverityClass(finding.severity)}>
              {finding.severity}
            </code>
            <code className="truncate text-text-3">{finding.code}</code>
            <span className="truncate">{finding.message}</span>
          </div>
        ))}
      </div>
    </section>
  );
}
