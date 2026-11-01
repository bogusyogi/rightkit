import Icon, { type IconName } from "@rightkit/agent-ui-core/components/Icon";
import type { SkillOptimizationStatus } from "@rightkit/agent-ui-core/lib/types";

export function SkillProposalStat({ label, value }: { label: string; value: number }) {
  return (
    <div className="rounded-lg border border-line bg-bg p-2">
      <span className="block text-[10px] uppercase text-text-3">{label}</span>
      <strong className="mt-1 block text-[16px] text-text">{value}</strong>
    </div>
  );
}

export function SkillProposalMeta({ label, value }: { label: string; value: string }) {
  return (
    <div className="min-w-0 rounded-lg border border-line bg-panel p-3">
      <span className="block text-[10px] uppercase text-text-3">{label}</span>
      <strong className="mt-1 block truncate text-[12px] text-text">{value}</strong>
    </div>
  );
}

export function SkillBodyPreview({ title, body }: { title: string; body: string }) {
  return (
    <div className="flex min-w-0 flex-col overflow-hidden">
      <div className="border-b border-line bg-panel px-3 py-2 text-[11px] font-semibold text-text-2">
        {title}
      </div>
      <pre className="min-h-0 flex-1 overflow-auto whitespace-pre-wrap break-words p-3 text-[11px] leading-5 text-text-2">
        {body}
      </pre>
    </div>
  );
}

export function skillTrainingStatusClass(status: SkillOptimizationStatus) {
  if (status === "accepted" || status === "running") return "text-accent-2";
  if (status === "rejected" || status === "cancelled") return "text-danger";
  return "text-text-3";
}

export function skillProvenanceSeverityClass(severity: "info" | "warning" | "error") {
  if (severity === "error") return "text-danger";
  if (severity === "warning") return "text-warning";
  return "text-accent-2";
}

export function SkillIcon({ name, size }: { name: IconName; size?: number }) {
  return <Icon name={name} size={size} />;
}
