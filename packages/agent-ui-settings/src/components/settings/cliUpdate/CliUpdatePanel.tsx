import type { ManagedCliUpdate } from "@rightkit/agent-ui-core/lib/cliUpdate/updateProjection";
import { hostAppName } from "../../../appName.js";

export interface CliUpdatePanelProps {
  updates: ManagedCliUpdate[];
  loading?: boolean;
  error?: string | null;
  onRefresh?: () => void | Promise<void>;
}

function stateLabel(state: ManagedCliUpdate["state"]): string {
  switch (state) {
    case "current":
      return "Current";
    case "update-available":
      return "Update available";
    case "not-installed":
      return "Not installed";
    case "unavailable":
      return "Status unavailable";
  }
}

/** Settings projection for managed CLI versions. Installation stays daemon-owned. */
export default function CliUpdatePanel({
  updates,
  loading = false,
  error = null,
  onRefresh,
}: CliUpdatePanelProps) {
  return (
    <section className="rounded-xl border border-line bg-panel p-4" aria-label="CLI updates">
      <div className="flex items-start justify-between gap-3">
        <div>
          <h3 className="text-[14px] font-semibold text-text">CLI updates</h3>
          <p className="mt-1 text-[12px] text-text-3">
            Managed CLI versions come from the daemon. {hostAppName()} never updates a vendor binary directly.
          </p>
        </div>
        {onRefresh ? (
          <button
            type="button"
            className="h-8 rounded-lg border border-line px-3 text-[12px] text-text-2 hover:bg-panel-2 disabled:opacity-40"
            disabled={loading}
            onClick={() => void onRefresh()}
          >
            {loading ? "Refreshing" : "Refresh"}
          </button>
        ) : null}
      </div>
      {error ? <p className="mt-3 text-[12px] text-danger">{error}</p> : null}
      <div className="mt-4 flex flex-col gap-2">
        {updates.length === 0 ? <p className="text-[12px] text-text-3">No managed CLIs detected.</p> : null}
        {updates.map((update) => (
          <article key={update.providerId} className="flex items-center gap-3 rounded-lg border border-line/70 px-3 py-2">
            <div className="min-w-0 flex-1">
              <strong className="block truncate text-[12px] text-text">{update.displayName}</strong>
              <span className="block truncate text-[11px] text-text-3">Installed: {update.installedVersion ?? "—"}</span>
            </div>
            <div className="shrink-0 text-right">
              <span className="block text-[12px] text-text-2">{stateLabel(update.state)}</span>
              <span className="block max-w-[220px] text-[10px] text-text-3">{update.reason}</span>
            </div>
          </article>
        ))}
      </div>
    </section>
  );
}
