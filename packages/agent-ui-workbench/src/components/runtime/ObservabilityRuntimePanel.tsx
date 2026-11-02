import Icon from "@rightkit/agent-ui-core/components/Icon";
import { EmptyState } from "@rightkit/react-shell/react";
import type { MiddlewareRegistrySnapshot, RuntimeObservation, RuntimeSpan } from "@rightkit/agent-ui-core/lib/types";
import RuntimeStat from "./RuntimeStat.js";
import { phaseLabel } from "./runtimeUtils.js";
export default function ObservabilityRuntimePanel({
  middleware,
  spans,
  observations,
}: {
  middleware: MiddlewareRegistrySnapshot;
  spans: RuntimeSpan[];
  observations: RuntimeObservation[];
}) {
  const observationCountBySpan = observations.reduce<Record<string, number>>((acc, observation) => {
    acc[observation.spanId] = (acc[observation.spanId] ?? 0) + 1;
    return acc;
  }, {});

  return (
    <div className="flex-1 min-h-0 overflow-y-auto">
      <div className="grid grid-cols-3 gap-3 border-b border-line px-5 py-3">
        <RuntimeStat label="Middleware" value={middleware.middleware.length} />
        <RuntimeStat label="Spans" value={spans.length} />
        <RuntimeStat label="Observations" value={observations.length} />
      </div>
      <div className="grid grid-cols-[minmax(0,1.2fr)_minmax(220px,0.8fr)] divide-x divide-line">
        <div className="min-w-0">
          <div className="border-b border-line px-5 py-2 text-[11px] font-semibold uppercase text-text-3">
            Trace tree
          </div>
          {spans.length === 0 ? (
            <EmptyState icon={<Icon name="activity" size={18} />} title="No spans yet">{"Runtime middleware spans appear here after sessions, tools, schedules, or adapters emit local trace data."}</EmptyState>
          ) : (
            <div className="flex flex-col divide-y divide-line">
              {spans.map((span) => (
                <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 px-5 py-3" key={span.id}>
                  <div className="min-w-0">
                    <div className="flex items-center gap-2">
                      <strong className="truncate text-[13px] font-semibold text-text">
                        {phaseLabel(span.phase)}
                      </strong>
                      <code className="rounded border border-line bg-panel px-1.5 py-0.5 text-[10px] text-text-3">
                        {span.status}
                      </code>
                    </div>
                    <p className="mt-1 truncate text-[11px] text-text-3">
                      {span.parentId ? `parentId ${span.parentId}` : "root span"} · session{" "}
                      {span.sessionId}
                    </p>
                    <p className="mt-1 truncate font-mono text-[11px] text-text-3">
                      {JSON.stringify(span.attributes)}
                    </p>
                  </div>
                  <div className="min-w-[120px] text-right">
                    <code className="block truncate text-[11px] font-mono text-text-2">
                      {span.id}
                    </code>
                    <span className="block text-[11px] text-text-3">
                      {observationCountBySpan[span.id] ?? 0} observation
                      {(observationCountBySpan[span.id] ?? 0) === 1 ? "" : "s"}
                    </span>
                    <span className="block truncate text-[11px] text-text-3">{span.startedAt}</span>
                  </div>
                </div>
              ))}
            </div>
          )}
        </div>
        <div className="min-w-0">
          <div className="border-b border-line px-5 py-2 text-[11px] font-semibold uppercase text-text-3">
            Middleware and observations
          </div>
          <div className="flex flex-col divide-y divide-line">
            {middleware.middleware.length === 0 && observations.length === 0 ? (
              <EmptyState icon={<Icon name="server" size={18} />} title="No middleware or observations">{"Registered middleware and the observations it emits during runs show up here."}</EmptyState>
            ) : null}
            {middleware.middleware.map((definition) => (
              <div className="px-5 py-3" key={definition.id}>
                <div className="flex items-center gap-2">
                  <strong className="min-w-0 flex-1 truncate text-[12px] text-text">
                    {definition.title}
                  </strong>
                  <span className="rounded border border-line bg-panel px-1.5 py-0.5 text-[10px] text-text-3">
                    {definition.enabled ? "enabled" : "disabled"}
                  </span>
                </div>
                <p className="mt-1 text-[11px] leading-snug text-text-3">
                  {phaseLabel(definition.phase)} · {definition.effect}
                  {definition.tool ? ` · ${definition.tool}` : ""}
                </p>
              </div>
            ))}
            {observations.map((observation) => (
              <div className="px-5 py-3" key={observation.id}>
                <div className="flex items-center gap-2">
                  <strong className="min-w-0 flex-1 truncate text-[12px] text-text">
                    {observation.message}
                  </strong>
                  <code className="rounded border border-line bg-panel px-1.5 py-0.5 text-[10px] text-text-3">
                    {observation.severity}
                  </code>
                </div>
                <p className="mt-1 truncate text-[11px] text-text-3">
                  span {observation.spanId} · {observation.createdAt}
                </p>
              </div>
            ))}
          </div>
        </div>
      </div>
    </div>
  );
}
