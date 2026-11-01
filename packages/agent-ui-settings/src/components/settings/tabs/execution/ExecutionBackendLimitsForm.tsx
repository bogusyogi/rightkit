import type { ResourceLimitsDraft, ResourceLimitsChange, ExecutionBackendConfig } from "./types.js";
import { ResourceLimitInput } from "./TextSettingInput.js";
import Icon from "@rightkit/agent-ui-core/components/Icon";

interface ExecutionBackendLimitsFormProps {
  backend: ExecutionBackendConfig;
  draft: ResourceLimitsDraft;
  pending: boolean;
  onDraftChange: (update: (draft: ResourceLimitsDraft) => ResourceLimitsDraft) => void;
  onSave: ResourceLimitsChange;
}

export function ExecutionBackendLimitsForm({
  backend,
  draft,
  pending,
  onDraftChange,
  onSave,
}: ExecutionBackendLimitsFormProps) {
  const baseline = resourceLimitsDraft(backend);
  const dirty =
    draft.cpu !== baseline.cpu ||
    draft.memoryMb !== baseline.memoryMb ||
    draft.diskMb !== baseline.diskMb;
  const valid = resourceDraftValid(draft);
  const formatted = formatResourceLimits(backend);
  return (
    <div className="flex flex-col gap-2 min-w-0">
      <span className="text-[11px] uppercase tracking-[0.08em] text-text-3">Capabilities</span>
      <CapabilitiesList backend={backend} />
      <span className="text-[12px] text-text-3">Resources: {formatted}</span>
      <div className="grid grid-cols-3 gap-2">
        <ResourceLimitInput
          label="CPU"
          value={draft.cpu}
          onChange={(cpu) => onDraftChange((current) => ({ ...current, cpu }))}
        />
        <ResourceLimitInput
          label="RAM MB"
          value={draft.memoryMb}
          onChange={(memoryMb) => onDraftChange((current) => ({ ...current, memoryMb }))}
        />
        <ResourceLimitInput
          label="Disk MB"
          value={draft.diskMb}
          onChange={(diskMb) => onDraftChange((current) => ({ ...current, diskMb }))}
        />
      </div>
      <button
        type="button"
        className="inline-flex items-center justify-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40 whitespace-nowrap"
        disabled={!dirty || pending || !valid}
        onClick={() => onSave(parseResourceLimitsDraft(draft))}
      >
        <Icon name="check" />
        {pending ? "Saving caps" : "Apply caps"}
      </button>
    </div>
  );
}

function CapabilitiesList({ backend }: { backend: ExecutionBackendConfig }) {
  const labels = Object.entries(backend.supports)
    .filter(([, enabled]) => enabled)
    .map(([name]) => capabilityLabel(name));
  if (labels.length === 0) {
    return <span className="text-[12px] text-text-3">No live controls advertised</span>;
  }
  return (
    <div className="flex flex-wrap gap-1.5">
      {labels.map((label) => (
        <span
          key={label}
          className="px-2 py-0.5 rounded-full border border-line bg-panel text-[11px] text-text-2"
        >
          {label}
        </span>
      ))}
    </div>
  );
}

function capabilityLabel(value: string) {
  return value.replace(/([A-Z])/g, " $1").replace(/^./, (match) => match.toUpperCase());
}

function resourceLimitsDraft(backend: ExecutionBackendConfig): ResourceLimitsDraft {
  return {
    cpu: formatLimitInput(backend.resourceLimits.cpu),
    memoryMb: formatLimitInput(backend.resourceLimits.memoryMb),
    diskMb: formatLimitInput(backend.resourceLimits.diskMb),
  };
}

function formatLimitInput(value?: number | null) {
  return value ? String(value) : "";
}

function resourceDraftValid(draft: ResourceLimitsDraft) {
  return [draft.cpu, draft.memoryMb, draft.diskMb].every((value) => {
    if (!value.trim()) return true;
    const parsed = Number(value);
    return Number.isInteger(parsed) && parsed > 0;
  });
}

function parseResourceLimitsDraft(draft: ResourceLimitsDraft) {
  return {
    cpu: parseOptionalPositiveInt(draft.cpu),
    memoryMb: parseOptionalPositiveInt(draft.memoryMb),
    diskMb: parseOptionalPositiveInt(draft.diskMb),
  };
}

function parseOptionalPositiveInt(value: string) {
  const trimmed = value.trim();
  return trimmed ? Number(trimmed) : null;
}

function formatResourceLimits(backend: ExecutionBackendConfig) {
  const limits = backend.resourceLimits;
  const parts = [
    limits.cpu ? `${limits.cpu} CPU` : null,
    limits.memoryMb ? `${limits.memoryMb} MB RAM` : null,
    limits.diskMb ? `${limits.diskMb} MB disk` : null,
  ].filter(Boolean);
  return parts.length ? parts.join(" · ") : "not capped";
}
