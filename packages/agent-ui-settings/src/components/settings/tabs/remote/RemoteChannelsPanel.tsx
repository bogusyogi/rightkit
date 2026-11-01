import { OfflinePanel } from "../shared";
import { diagnosticsStatusClass } from "./RemoteStatusCard";
import type { RemoteChannelsSnapshot } from "@rightkit/agent-ui-core/lib/types";

function RemoteGatewayChannelRow({
  channel,
}: {
  channel: RemoteChannelsSnapshot["channels"][number];
}) {
  const stateStatus =
    channel.status.state === "ready"
      ? "pass"
      : channel.status.state === "notConfigured" || channel.status.state === "blocked"
        ? "block"
        : "warn";
  return (
    <article className="grid grid-cols-[minmax(0,1fr)_minmax(170px,0.45fr)] gap-3 px-4 py-3">
      <div className="min-w-0">
        <div className="flex items-center gap-2 min-w-0">
          <strong className="truncate text-[13px] text-text">{channel.id}</strong>
          <span
            className={`shrink-0 rounded border px-2 py-0.5 text-[10px] ${diagnosticsStatusClass(stateStatus)}`}
          >
            {channel.status.state}
          </span>
          <span className="shrink-0 rounded border border-line bg-panel-2 px-2 py-0.5 text-[10px] text-text-3">
            {channel.kind}
          </span>
        </div>
        <div className="mt-2 grid grid-cols-1 sm:grid-cols-3 gap-2 text-[11px] text-text-3">
          <span>Profile {channel.profile.profileId}</span>
          <span>Toolset {channel.profile.toolset}</span>
          <span>Trust {channel.profile.trust}</span>
          <span>{channel.status.bindingCount} bindings</span>
          <span>{channel.status.activeTargetCount} active targets</span>
          <span>{channel.profile.deliveryPolicy}</span>
        </div>
        {channel.notes.length ? (
          <div className="mt-2 flex flex-col gap-1">
            {channel.notes.slice(0, 2).map((note) => (
              <p className="text-[11px] leading-relaxed text-text-3" key={note}>
                {note}
              </p>
            ))}
          </div>
        ) : null}
        {channel.recentDeliveries.length ? (
          <div className="mt-3 overflow-hidden rounded border border-line bg-panel-2">
            {channel.recentDeliveries.slice(0, 3).map((delivery) => (
              <div
                className="grid grid-cols-[minmax(0,1fr)_auto] gap-2 border-b border-line px-2 py-1.5 text-[10px] text-text-3 last:border-b-0"
                key={delivery.id}
              >
                <span className="min-w-0 truncate">
                  {delivery.deliveryKind} / {delivery.targetRedacted}
                </span>
                <span className="shrink-0 text-text-2">{delivery.status}</span>
              </div>
            ))}
          </div>
        ) : null}
      </div>
      <div className="min-w-0 text-right text-[11px] text-text-3">
        <span className="block truncate">
          {channel.enabled ? "Enabled" : "Disabled"} /{" "}
          {channel.configured ? "configured" : "not configured"}
        </span>
        <span className="mt-1 block truncate">
          Last success {channel.status.lastSuccessAt ?? "none"}
        </span>
        <span className="mt-1 block truncate">
          Last error {channel.status.lastErrorAt ?? "none"}
        </span>
        {channel.status.lastError ? (
          <span className="mt-1 block truncate text-danger">{channel.status.lastError}</span>
        ) : null}
      </div>
    </article>
  );
}

export function RemoteChannelsPanel({ snapshot }: { snapshot: RemoteChannelsSnapshot | null }) {
  return (
    <div className="rounded-xl border border-line bg-panel overflow-hidden">
      <div className="flex items-start justify-between gap-3 px-4 py-3 border-b border-line">
        <div className="min-w-0">
          <h4 className="text-[13px] font-semibold text-text">Remote gateway profiles</h4>
          <p className="text-[12px] text-text-3">
            Generic channel profiles, delivery policy, trust labels, and redacted binding counts
            from the daemon gateway snapshot.
          </p>
        </div>
        <span className="shrink-0 rounded-full border border-line bg-panel-2 px-2 py-0.5 text-[11px] font-semibold text-text-3">
          {snapshot ? `${snapshot.channels.length} channels` : "not loaded"}
        </span>
      </div>
      {snapshot ? (
        <div className="grid grid-cols-1 xl:grid-cols-[minmax(220px,0.45fr)_minmax(0,1fr)] gap-4 p-4">
          <div className="flex flex-col gap-2">
            {snapshot.profiles.length ? (
              snapshot.profiles.map((profile) => (
                <div className="rounded-lg border border-line bg-panel-2 p-3" key={profile.id}>
                  <div className="flex items-center justify-between gap-3">
                    <strong className="text-[12px] text-text">{profile.id}</strong>
                    <span className="rounded border border-line bg-panel px-1.5 py-0.5 text-[10px] text-text-3">
                      {profile.deliveryPolicy}
                    </span>
                  </div>
                  <div className="mt-2 text-[11px] text-text-3">Toolset {profile.toolset}</div>
                  <div className="mt-2 flex flex-wrap gap-1">
                    {profile.channels.map((channel) => (
                      <span
                        className="rounded border border-line bg-panel px-1.5 py-0.5 text-[10px] text-text-3"
                        key={`${profile.id}:${channel}`}
                      >
                        {channel}
                      </span>
                    ))}
                  </div>
                </div>
              ))
            ) : (
              <OfflinePanel
                title="No gateway profiles loaded."
                action="Refresh remote status to read channel profiles."
              />
            )}
          </div>
          <div className="min-w-0 rounded-lg border border-line overflow-hidden">
            {snapshot.channels.length ? (
              <div className="divide-y divide-line">
                {snapshot.channels.map((channel) => (
                  <RemoteGatewayChannelRow channel={channel} key={channel.id} />
                ))}
              </div>
            ) : (
              <div className="px-4 py-3 text-[13px] text-text-3">
                No remote channels are currently registered.
              </div>
            )}
          </div>
        </div>
      ) : (
        <OfflinePanel
          title="Remote gateway snapshot not loaded."
          action="Refresh remote status to inspect generic gateway profiles."
        />
      )}
    </div>
  );
}
