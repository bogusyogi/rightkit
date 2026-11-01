import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { MiddlewareRegistrySnapshot } from "@rightkit/agent-ui-core/lib/types";
import { OfflinePanel } from "./shared";

function formatMiddlewarePhase(phase: string) {
  return phase.replace(/([A-Z])/g, " $1").replace(/^./, (value) => value.toUpperCase());
}

function RuntimeMiddlewareStat({ label, value }: { label: string; value: number }) {
  return (
    <div className="rounded-xl border border-line bg-panel p-3">
      <span className="block text-[11px] uppercase text-text-3">{label}</span>
      <strong className="mt-1 block text-[18px] text-text">{value}</strong>
    </div>
  );
}

export default function RuntimeMiddlewareTab({
  middlewareSnapshot,
  pendingId,
  error,
  onRefresh,
  onToggle,
}: {
  middlewareSnapshot: MiddlewareRegistrySnapshot;
  pendingId: string | null;
  error: string | null;
  onRefresh: () => void;
  onToggle: (id: string, enabled: boolean) => void;
}) {
  return (
    <section className="flex flex-col gap-5 p-6">
      <div className="flex items-start gap-3">
        <div className="flex flex-col gap-1 flex-1 min-w-0">
          <h3 className="text-[16px] font-bold text-text">Runtime middleware</h3>
          <p className="text-[13px] text-text-2 truncate">
            Inspect and enable local middleware that observes or gates runtime execution.
          </p>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
          disabled={pendingId === "refresh"}
          onClick={onRefresh}
        >
          <Icon name="refresh" /> {pendingId === "refresh" ? "Refreshing" : "Refresh"}
        </button>
      </div>

      {error ? <OfflinePanel title="Runtime middleware unavailable." action={error} /> : null}

      <div className="grid grid-cols-3 gap-3">
        <RuntimeMiddlewareStat label="Middleware" value={middlewareSnapshot.middleware.length} />
        <RuntimeMiddlewareStat label="Spans" value={middlewareSnapshot.spans.length} />
        <RuntimeMiddlewareStat
          label="Observations"
          value={middlewareSnapshot.observations.length}
        />
      </div>

      <div className="flex flex-col divide-y divide-line rounded-xl border border-line overflow-hidden">
        {middlewareSnapshot.middleware.length ? (
          middlewareSnapshot.middleware.map((definition) => (
            <article
              className="grid grid-cols-[minmax(0,1fr)_auto] gap-4 bg-panel px-4 py-3"
              key={definition.id}
            >
              <div className="min-w-0">
                <div className="flex items-center gap-2">
                  <strong
                    className="min-w-0 truncate text-[13px] font-semibold text-text"
                    title={definition.title}
                  >
                    {definition.title}
                  </strong>
                  <span className="shrink-0 rounded border border-line bg-bg px-1.5 py-0.5 text-[10px] text-text-3">
                    {definition.enabled ? "enabled" : "disabled"}
                  </span>
                </div>
                <p
                  className="mt-1 line-clamp-2 text-[12px] leading-snug text-text-2"
                  title={definition.description}
                >
                  {definition.description}
                </p>
                <div className="mt-2 flex flex-wrap gap-2 text-[11px] text-text-3">
                  <code className="rounded border border-line bg-bg px-1.5 py-0.5">
                    phase {formatMiddlewarePhase(definition.phase)}
                  </code>
                  <code className="rounded border border-line bg-bg px-1.5 py-0.5">
                    effect {definition.effect}
                  </code>
                  <code className="rounded border border-line bg-bg px-1.5 py-0.5">
                    source {definition.source}
                  </code>
                  {definition.tool ? (
                    <code
                      className="min-w-0 max-w-full truncate rounded border border-line bg-bg px-1.5 py-0.5"
                      title={`tool ${definition.tool}`}
                    >
                      tool {definition.tool}
                    </code>
                  ) : null}
                  {definition.requiresToolset ? (
                    <code
                      className="min-w-0 max-w-full truncate rounded border border-line bg-bg px-1.5 py-0.5"
                      title={`toolset ${definition.requiresToolset}`}
                    >
                      toolset {definition.requiresToolset}
                    </code>
                  ) : null}
                </div>
              </div>
              <button
                type="button"
                className="inline-flex items-center justify-center h-8 min-w-[86px] px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-bg hover:text-text disabled:opacity-40"
                disabled={pendingId === definition.id || pendingId === "refresh"}
                onClick={() => onToggle(definition.id, !definition.enabled)}
              >
                {pendingId === definition.id ? "Saving" : definition.enabled ? "Disable" : "Enable"}
              </button>
            </article>
          ))
        ) : (
          <OfflinePanel
            title="No middleware registered."
            action="Refresh after the runtime starts to inspect built-in and plugin middleware."
          />
        )}
      </div>
    </section>
  );
}
