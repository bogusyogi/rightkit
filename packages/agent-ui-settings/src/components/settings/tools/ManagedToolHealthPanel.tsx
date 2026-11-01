import { useCallback } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  managedToolStatusLabel,
  type ManagedToolHealth,
  type ManagedToolProbe,
} from "@rightkit/agent-ui-core/lib/tools/managedToolHealth";
import { useManagedToolHealth } from "@rightkit/agent-ui-core/lib/tools/useManagedToolHealth";

export interface ManagedToolHealthPanelProps {
  toolId: string;
  label?: string;
  probe: ManagedToolProbe;
  onSelectInstallation?: () => void;
  onRemediate?: () => void;
  installUrl?: string;
  enabled?: boolean;
}

function HealthBadge({ health }: { health: ManagedToolHealth | null }) {
  const installed = health?.installed === true;
  const knownMissing = health?.installed === false;
  return (
    <span className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[11px] ${installed ? "border-accent-2/40 text-accent-2" : knownMissing ? "border-danger/40 text-danger" : "border-line text-text-3"}`}>
      <Icon name={installed ? "check" : knownMissing ? "close" : "alert"} size={11} />
      {managedToolStatusLabel(health)}
    </span>
  );
}

/**
 * Opcode's Topbar version-status projection, adapted for a generic the host app
 * managed tool. Discovery/install effects stay in the injected Tauri connector.
 */
export function ManagedToolHealthPanel({
  toolId,
  label = toolId,
  probe,
  onSelectInstallation,
  onRemediate,
  installUrl,
  enabled = true,
}: ManagedToolHealthPanelProps) {
  const onNotInstalled = useCallback((health: ManagedToolHealth) => {
    if (health.output.includes("No such file or directory")) {
      window.dispatchEvent(new CustomEvent("managed-tool-not-found", { detail: { toolId } }));
    }
  }, [toolId]);
  const { health, checking, error, check } = useManagedToolHealth({ toolId, probe, enabled, onNotInstalled });

  return (
    <section className="flex flex-col gap-3 rounded-xl border border-line bg-panel p-4" aria-labelledby={`${toolId}-health-title`}>
      <div className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <h4 id={`${toolId}-health-title`} className="text-[13px] font-semibold text-text">{label}</h4>
          <p className="mt-1 text-[12px] text-text-3">Managed tool installation and version</p>
        </div>
        <HealthBadge health={health} />
      </div>

      {checking ? (
        <div className="flex items-center gap-2 text-[12px] text-text-3" role="status">
          <Icon name="activity" size={13} /> Checking {label}…
        </div>
      ) : health === null ? (
        <p className="text-[12px] text-text-2">Unable to verify the {label} installation.</p>
      ) : health.installed ? (
        <div className="flex flex-wrap items-center justify-between gap-2 text-[12px] text-text-2">
          <span className="font-mono">{health.version ? `v${health.version}` : "Version unavailable"}</span>
          {health.executable ? <span className="max-w-[55%] truncate text-[11px] text-text-3" title={health.executable}>{health.executable}</span> : null}
        </div>
      ) : (
        <div className="flex flex-col gap-2">
          <p className="text-[12px] text-text-2">{label} was not found. Select an installation or install it before starting a session.</p>
          {health?.output ? <pre className="max-h-28 overflow-auto rounded-md border border-line bg-bg p-2 text-[10px] text-text-3">{health.output}</pre> : null}
          <div className="flex flex-wrap gap-2">
            {onSelectInstallation ? <button type="button" onClick={onSelectInstallation} className="rounded-lg border border-line px-3 py-1.5 text-[12px] text-text-2 hover:bg-panel-2">Select installation</button> : null}
            {onRemediate ? <button type="button" onClick={onRemediate} className="rounded-lg bg-accent px-3 py-1.5 text-[12px] font-semibold text-on-accent">Install or repair</button> : null}
            {installUrl ? <a href={installUrl} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1.5 rounded-lg border border-line px-3 py-1.5 text-[12px] text-text-2 hover:bg-panel-2"><Icon name="link" size={12} /> Installation guide</a> : null}
          </div>
        </div>
      )}

      {error ? <p className="text-[11px] text-danger"><Icon name="alert" size={11} /> {error}</p> : null}
      <button type="button" onClick={() => void check()} disabled={checking} className="inline-flex items-center gap-1.5 self-start rounded-lg border border-line px-3 py-1.5 text-[11px] text-text-2 hover:bg-panel-2 disabled:opacity-40">
        <Icon name="refresh" size={12} /> {checking ? "Checking" : "Recheck"}
      </button>
    </section>
  );
}

export default ManagedToolHealthPanel;
