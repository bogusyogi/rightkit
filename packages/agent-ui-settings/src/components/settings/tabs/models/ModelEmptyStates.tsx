import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { LocalModelCatalog, LocalModelProbeResult } from "@rightkit/agent-ui-core/lib/types";
import { DiagnosticsStat } from "../shared.js";
import { hostAppName } from "../../../../appName.js";

export function ModelSettingsEmptyState({ onOpenWorkspace }: { onOpenWorkspace: () => void }) {
  return (
    <div className="cr-panel-reveal rounded-xl border border-line bg-panel">
      <div className="flex flex-wrap items-start gap-3 border-b border-line px-4 py-4">
        <span className="grid h-9 w-9 flex-shrink-0 place-items-center rounded-lg border border-line bg-bg text-text-2">
          <Icon name="activity" size={15} />
        </span>
        <div className="min-w-[240px] flex-1">
          <strong className="block text-[13px] font-semibold text-text">
            Model settings attach to a workspace
          </strong>
          <p className="mt-1 max-w-[620px] text-[13px] leading-[1.5] text-text-2">
            Open a local folder first; {hostAppName()} will load the project model map, role overrides,
            and provider readiness into this panel.
          </p>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          <span className="rounded-full border border-line bg-bg px-2.5 py-1 text-[11px] font-semibold text-text-2">
            Workspace required
          </span>
          <button
            type="button"
            className="cr-control-pill inline-flex h-8 items-center gap-2 rounded-lg border-0 bg-accent px-3 text-[12px] font-semibold text-on-accent hover:opacity-90"
            onClick={onOpenWorkspace}
          >
            <Icon name="folder" size={13} /> Open local folder
          </button>
        </div>
      </div>
      <div className="grid gap-2 p-3">
        <ModelEmptyStep
          label="1"
          title="Choose workspace"
          detail="Open a repo or worktree so settings can attach to the project."
          current
        />
        <ModelEmptyStep
          label="2"
          title="Load roles"
          detail={`${hostAppName()} reads role defaults from the workspace.`}
        />
        <ModelEmptyStep
          label="3"
          title="Save defaults"
          detail="The Save button becomes primary after a real setting changes."
        />
      </div>
    </div>
  );
}

function ModelEmptyStep({
  label,
  title,
  detail,
  current = false,
}: {
  label: string;
  title: string;
  detail: string;
  current?: boolean;
}) {
  return (
    <div
      className={`grid min-w-0 grid-cols-[28px_minmax(0,1fr)] gap-3 rounded-lg border px-3 py-3 transition-[border-color,background-color] duration-150 ${
        current
          ? "border-[color-mix(in_srgb,var(--color-accent)_32%,var(--color-line))] bg-[color-mix(in_srgb,var(--color-accent)_5%,var(--color-bg))]"
          : "border-line bg-bg"
      }`}
    >
      <span
        className={`inline-grid h-6 w-6 place-items-center rounded-md border text-[11px] font-semibold ${
          current
            ? "border-[color-mix(in_srgb,var(--color-accent)_42%,var(--color-line))] bg-[color-mix(in_srgb,var(--color-accent)_10%,transparent)] text-accent"
            : "border-line bg-panel text-text-3"
        }`}
      >
        {label}
      </span>
      <span className="min-w-0">
        <strong className="block text-[13px] font-semibold text-text">{title}</strong>
        <span className="mt-1 block text-[12px] leading-[1.45] text-text-3">{detail}</span>
      </span>
    </div>
  );
}

export function LocalModelCookbookEmpty({
  onRefreshLocalModels,
}: {
  onRefreshLocalModels: () => void;
}) {
  return (
    <div className="px-4 py-4">
      <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto] md:items-center">
        <div className="min-w-0">
          <strong className="block text-[13px] font-semibold text-text">
            Inspect local model fit
          </strong>
          <p className="mt-1 max-w-[720px] text-[12px] leading-[1.5] text-text-2">
            Refresh checks CPU, memory, GPU/VRAM, local runtimes, and endpoint compatibility before
            you save a local default.
          </p>
        </div>
        <button
          type="button"
          className="inline-flex h-8 items-center gap-2 rounded-lg border border-line bg-transparent px-3 text-[12px] text-text-2 cursor-pointer hover:bg-panel-2 hover:text-text"
          onClick={onRefreshLocalModels}
        >
          <Icon name="refresh" size={13} /> Inspect fit
        </button>
      </div>
      <div className="mt-4 grid gap-3 [grid-template-columns:repeat(auto-fit,minmax(190px,1fr))]">
        <DiagnosticsStat label="Hardware" value="CPU / RAM / GPU" detail="readiness snapshot" />
        <DiagnosticsStat label="Runtime" value="Ollama / LM Studio" detail="detected locally" />
        <DiagnosticsStat label="Route" value="Endpoint probe" detail="probe before save" />
      </div>
    </div>
  );
}

export function localModelFitLabel(fit: LocalModelCatalog["recommendations"][number]["fit"]) {
  switch (fit) {
    case "good":
      return "good fit";
    case "caution":
      return "caution";
    case "tooLarge":
      return "too large";
    default:
      return "unknown";
  }
}

export function localGpuMemoryLabel(gpus: LocalModelCatalog["hardware"]["gpuDevices"]) {
  const maxMemory = Math.max(0, ...gpus.map((gpu) => gpu.memoryMb ?? 0));
  return maxMemory > 0 ? `${maxMemory} MB max VRAM` : "VRAM unknown";
}

export function localModelVramLabel(model: LocalModelCatalog["recommendations"][number]) {
  const target = model.recommendedVramMb ?? model.minVramMb;
  const detected = model.detectedVramMb;
  if (target && detected) {
    return `VRAM ${detected}/${target} MB`;
  }
  if (target) {
    return `VRAM target ${target} MB`;
  }
  if (detected) {
    return `VRAM detected ${detected} MB`;
  }
  return "VRAM manual check";
}

export function localModelSmokeCandidate(
  probe: LocalModelProbeResult | null,
  catalog: LocalModelCatalog | null,
) {
  return (
    probe?.models.find((model) => model.trim()) ??
    catalog?.recommendations.find((model) => model.modelSlug.trim())?.modelSlug
  );
}

export function localModelFitClass(fit: LocalModelCatalog["recommendations"][number]["fit"]) {
  switch (fit) {
    case "good":
      return "border-accent-2/40 text-accent-2";
    case "caution":
      return "border-accent/40 text-accent";
    case "tooLarge":
      return "border-danger/40 text-danger";
    default:
      return "border-line text-text-3";
  }
}
