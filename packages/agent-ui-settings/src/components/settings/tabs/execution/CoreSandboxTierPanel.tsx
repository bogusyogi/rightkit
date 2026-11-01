import { Select } from "@rightkit/react-shell/react";
import { useEffect, useState } from "react";
import type {
  CoreSandboxSettings,
  CoreSandboxTier,
  DiagnosticsReport,
} from "@rightkit/agent-ui-core/lib/types";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { diagnosticsLabel, diagnosticsStatusClass } from "../shared";
import { RemoteStatusCard } from "./RemoteStatusCard";

interface CoreSandboxTierPanelProps {
  report: DiagnosticsReport | null;
  settings: CoreSandboxSettings | null;
  pending: boolean;
  onSaveTier: (tier: CoreSandboxTier) => void;
}

const CORE_SANDBOX_TIERS: { value: CoreSandboxTier; label: string; detail: string }[] = [
  {
    value: "landlock-seccomp",
    label: "Landlock + seccomp",
    detail: "Default local shell filesystem and syscall guard where Linux supports it.",
  },
  {
    value: "landlock",
    label: "Landlock",
    detail: "Filesystem guard only; useful when syscall filtering needs to stay off.",
  },
  {
    value: "none",
    label: "Disabled",
    detail: "Trusted workloads only; diagnostics will warn on this setting.",
  },
];

export function CoreSandboxTierPanel({
  report,
  settings,
  pending,
  onSaveTier,
}: CoreSandboxTierPanelProps) {
  const sandboxCheck = report?.checks.find((check) => check.id === "core_sandbox");
  const [draftTier, setDraftTier] = useState<CoreSandboxTier>(
    settings?.workspaceTier ?? settings?.tier ?? "landlock-seccomp",
  );
  const persistedWorkspaceTier = settings?.workspaceTier ?? settings?.tier;
  useEffect(() => {
    if (persistedWorkspaceTier) {
      setDraftTier(persistedWorkspaceTier);
    }
  }, [persistedWorkspaceTier]);
  const selectedTier = CORE_SANDBOX_TIERS.find((option) => option.value === draftTier);
  const savedWorkspaceTier = persistedWorkspaceTier;
  const tierDirty = settings ? draftTier !== savedWorkspaceTier : draftTier !== "landlock-seccomp";
  return (
    <section className="grid grid-cols-1 gap-3 rounded-xl border border-line bg-panel p-4 max-w-[980px]">
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h4 className="text-[13px] font-semibold text-text">Core tool sandbox</h4>
          <p className="mt-1 text-[12px] text-text-3">
            RIGHTKIT_SANDBOX_TIER is the operator minimum; a workspace may only tighten it.
          </p>
        </div>
        {sandboxCheck ? (
          <span
            className={`shrink-0 px-2 py-0.5 rounded-full border text-[11px] ${diagnosticsStatusClass(
              sandboxCheck.status,
            )}`}
          >
            {diagnosticsLabel(sandboxCheck.status)}
          </span>
        ) : (
          <span className="shrink-0 px-2 py-0.5 rounded-full border border-line text-[11px] text-text-3">
            Not loaded
          </span>
        )}
      </div>
      <div className="grid gap-3 md:grid-cols-[minmax(0,1fr)_auto] items-end">
        <label className="flex flex-col gap-1 min-w-0">
          <span className="text-[10px] uppercase tracking-[0.08em] text-text-3">Sandbox tier</span>
          <Select value={draftTier} options={[...CORE_SANDBOX_TIERS.map((option) => ({ value: option.value, label: option.label }))]} onChange={(value) => setDraftTier(value as CoreSandboxTier)} label="Sandbox tier" className="h-9 min-w-0 rounded-lg border border-line bg-panel-2 px-2 text-[13px] text-text outline-none focus:border-accent/60" />
          <span className="text-[11px] text-text-3">
            {selectedTier?.detail ?? "Choose how local shell commands request isolation."}
          </span>
        </label>
        <button
          type="button"
          className="inline-flex items-center justify-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40 whitespace-nowrap"
          disabled={pending || !tierDirty}
          onClick={() => onSaveTier(draftTier)}
        >
          <Icon name="check" />
          {pending ? "Saving tier" : "Save tier"}
        </button>
      </div>
      <div className="grid gap-2 md:grid-cols-4">
        <RemoteStatusCard
          title="Saved tier"
          value={sandboxTierLabel(savedWorkspaceTier)}
          status={sandboxCheck?.status ?? "warn"}
          detail={
            settings
              ? `effective ${sandboxTierLabel(settings.tier)} · ${settings.source} · ${settings.storePath}`
              : "Load execution settings to inspect the saved workspace tier."
          }
        />
        <RemoteStatusCard
          title="Mode"
          value="Local OS"
          status={sandboxCheck?.status ?? "warn"}
          detail="Linux hosts use Landlock/seccomp where available; non-Linux hosts report an honest warning."
        />
        <RemoteStatusCard
          title="Current"
          value={sandboxCheck ? diagnosticsLabel(sandboxCheck.status) : "Not loaded"}
          status={sandboxCheck?.status ?? "warn"}
          detail={sandboxCheck?.detail ?? "Refresh diagnostics to inspect core sandbox posture."}
        />
        <RemoteStatusCard
          title="Fix"
          value={sandboxCheck?.fix ? "Action available" : "No action"}
          status={sandboxCheck?.status ?? "pass"}
          detail={sandboxCheck?.fix || "Sandbox posture is explicit for this daemon."}
        />
      </div>
    </section>
  );
}

function sandboxTierLabel(tier: string | null | undefined) {
  return CORE_SANDBOX_TIERS.find((option) => option.value === tier)?.label ?? "Not saved";
}
