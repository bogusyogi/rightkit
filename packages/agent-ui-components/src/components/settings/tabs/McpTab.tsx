import { Checkbox, ConfirmDialog, EmptyState, Select } from "@rightkit/react-shell/react";
import { useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type {
  McpProbeResult,
  McpServerModeStatus,
  McpServerRow,
  McpServerScope,
  McpServerType,
} from "@rightkit/agent-ui-core/lib/types";
import type { McpDraft } from "../types.js";
import { diagnosticsCardStatusClass, OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";

function defaultMcpTrustPreset(type: McpServerType): string {
  return type === "stdio" ? "local_process" : "local_dev";
}

function mcpServerKey(server: Pick<McpServerRow, "scope" | "name">): string {
  return `${server.scope}:${server.name}`;
}

function describeMcpServer(server: McpServerRow): string {
  if (server.config.type === "http" || server.config.type === "sse") {
    return server.config.url;
  }
  const args = server.config.args?.join(" ") ?? "";
  return `${server.config.command}${args ? ` ${args}` : ""}`;
}

function mcpTrustLabel(preset: string | undefined): string {
  switch (preset) {
    case "local_process":
      return "local process";
    case "local_dev":
      return "local dev";
    case "remote_read_only":
      return "remote read-only";
    case "remote_full":
      return "remote full";
    case "remote_unreviewed":
      return "remote unreviewed";
    default:
      return preset || "unknown";
  }
}

function RemoteStatusCard({
  title,
  value,
  status,
  detail,
}: {
  title: string;
  value: string;
  status: "pass" | "warn" | "block";
  detail?: string;
}) {
  return (
    <div
      className={`flex flex-col gap-1 rounded-lg border px-3 py-2 ${diagnosticsCardStatusClass(status)}`}
    >
      <span className="text-[11px] uppercase text-text-3">{title}</span>
      <strong className="text-[14px] font-semibold text-text">{value}</strong>
      {detail ? <span className="text-[12px] text-text-3 break-all">{detail}</span> : null}
    </div>
  );
}

function McpSaveButton({ pending, onSave }: { pending: boolean; onSave: () => void }) {
  return (
    <button
      type="button"
      className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
      disabled={pending}
      onClick={onSave}
    >
      <Icon name="plus" /> Save
    </button>
  );
}
export default function McpTab({
  servers,
  probeResults,
  serverModeStatus,
  userConfigPath,
  workspaceConfigPath,
  draft,
  pendingId,
  error,
  workspaceRoot,
  onDraft,
  onSave,
  onRemove,
  onProbe,
  onRefresh,
}: {
  servers: McpServerRow[];
  probeResults: Record<string, McpProbeResult>;
  serverModeStatus: McpServerModeStatus | null;
  userConfigPath: string;
  workspaceConfigPath: string | null;
  draft: McpDraft;
  pendingId: string | null;
  error: string | null;
  workspaceRoot: string;
  onDraft: (draft: McpDraft) => void;
  onSave: () => void;
  onRemove: (server: McpServerRow) => void;
  onProbe: (server: McpServerRow) => void;
  onRefresh: () => void;
}) {
  const [pendingRemove, setPendingRemove] = useState<McpServerRow | null>(null);
  const targetPath =
    draft.scope === "workspace"
      ? workspaceConfigPath || "Select a workspace to write .mcp.json"
      : userConfigPath || "User MCP config path unavailable";

  return (
    <section className="@container flex flex-col gap-5 p-6">
      <div className="flex items-start gap-3">
        <div className="flex flex-col gap-1 flex-1 min-w-0">
          <h3 className="text-[16px] font-bold text-text">MCP</h3>
          <p className="text-[13px] text-text-2 truncate" title={targetPath}>
            {targetPath}
          </p>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text"
          onClick={onRefresh}
        >
          <Icon name="refresh" /> Refresh
        </button>
      </div>

      {error ? <OfflinePanel title="MCP servers unavailable." action={error} /> : null}

      {serverModeStatus ? (
        <div className="grid grid-cols-1 xl:grid-cols-3 gap-3 max-w-[900px]">
          <RemoteStatusCard
            title="Server default"
            value={
              serverModeStatus.defaultMode === "read_only"
                ? "Read-only"
                : serverModeStatus.defaultMode
            }
            status="pass"
            detail={serverModeStatus.readOnlyCommand}
          />
          <RemoteStatusCard
            title="Read-only tools"
            value={`${serverModeStatus.readOnlyToolCount} tools`}
            status={serverModeStatus.readOnlyToolCount > 0 ? "pass" : "warn"}
            detail={serverModeStatus.readOnlyTools.slice(0, 5).join(", ")}
          />
          <RemoteStatusCard
            title="Mutation opt-in"
            value={`${serverModeStatus.mutatingToolCount} tools`}
            status={serverModeStatus.mutatingRequiresOptIn ? "pass" : "block"}
            detail={serverModeStatus.mutatingCommand}
          />
        </div>
      ) : null}

      <div className="grid grid-cols-2 gap-3 max-w-[860px] @2xl:grid-cols-[minmax(0,1fr)_140px_120px_160px]">
        <label className="flex min-w-0 flex-col gap-1 text-[13px] text-text-2">
          Server name
          <input
            className="h-9 w-full min-w-0 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
            value={draft.name}
            onChange={(e) => onDraft({ ...draft, name: e.target.value })}
            placeholder="github"
          />
        </label>
        <div className="flex min-w-0 flex-col gap-1 text-[13px] text-text-2">
          <span>Scope</span>
          <Select<McpServerScope>
            label="Scope"
            value={draft.scope}
            onChange={(scope) => onDraft({ ...draft, scope })}
            options={[
              { value: "workspace", label: "Workspace" },
              { value: "user", label: "User" },
            ]}
          />
        </div>
        <div className="flex min-w-0 flex-col gap-1 text-[13px] text-text-2">
          <span>Transport</span>
          <Select<McpServerType>
            label="Transport"
            value={draft.type}
            onChange={(type) =>
              onDraft({
                ...draft,
                type,
                trustPreset: defaultMcpTrustPreset(type),
              })
            }
            options={[
              { value: "stdio", label: "stdio" },
              { value: "http", label: "HTTP" },
              { value: "sse", label: "SSE" },
            ]}
          />
        </div>
        <div className="flex min-w-0 flex-col gap-1 text-[13px] text-text-2">
          <span>Trust</span>
          <Select
            label="Trust"
            value={draft.trustPreset}
            onChange={(trustPreset) => onDraft({ ...draft, trustPreset })}
            options={[
              { value: "local_process", label: "Local process" },
              { value: "local_dev", label: "Local dev" },
              { value: "remote_read_only", label: "Remote read-only" },
              { value: "remote_full", label: "Remote full" },
              { value: "remote_unreviewed", label: "Remote unreviewed" },
            ]}
          />
        </div>
      </div>

      <div className="grid grid-cols-[minmax(0,1fr)_180px] gap-3 max-w-[860px]">
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Allowed channels
          <input
            className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
            value={draft.channelAllowlist}
            onChange={(e) => onDraft({ ...draft, channelAllowlist: e.target.value })}
            placeholder="desktop, cli, telegram"
          />
        </label>
        <Checkbox checked={draft.allowMutating} onChange={(checked) => onDraft({ ...draft, allowMutating: checked })} label="Allow mutating tools" className="flex items-center gap-2 h-9 self-end text-[13px] text-text-2" />
      </div>

      {draft.type === "stdio" ? (
        <div className="grid grid-cols-[minmax(0,1fr)_minmax(0,1fr)_auto] gap-3 max-w-[680px] items-end">
          <label className="flex min-w-0 flex-col gap-1 text-[13px] text-text-2">
            Command
            <input
              className="h-9 w-full min-w-0 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={draft.command}
              onChange={(e) => onDraft({ ...draft, command: e.target.value })}
              placeholder="npx"
            />
          </label>
          <label className="flex min-w-0 flex-col gap-1 text-[13px] text-text-2">
            Args
            <input
              className="h-9 w-full min-w-0 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={draft.args}
              onChange={(e) => onDraft({ ...draft, args: e.target.value })}
              placeholder="-y @modelcontextprotocol/server-github"
            />
          </label>
          <McpSaveButton pending={pendingId === draft.name} onSave={onSave} />
        </div>
      ) : (
        <div className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 max-w-[680px] items-end">
          <label className="flex min-w-0 flex-col gap-1 text-[13px] text-text-2">
            URL
            <input
              className="h-9 w-full min-w-0 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={draft.url}
              onChange={(e) => onDraft({ ...draft, url: e.target.value })}
              placeholder={
                draft.type === "http" ? "http://127.0.0.1:9105/mcp" : "http://127.0.0.1:9100/sse"
              }
            />
          </label>
          <McpSaveButton pending={pendingId === draft.name} onSave={onSave} />
        </div>
      )}

      {!workspaceRoot.trim() && draft.scope === "workspace" ? (
        <p className="text-[12px] text-danger">
          Workspace-scoped servers need an active workspace.
        </p>
      ) : null}

      {servers.length > 0 ? (
        <div className="flex flex-col divide-y divide-line border border-line rounded-xl overflow-hidden">
          {servers.map((server) => {
            const key = mcpServerKey(server);
            const probe = probeResults[key];
            return (
              <article
                className="grid grid-cols-[minmax(0,1fr)_auto_auto_auto] items-center gap-4 px-4 py-3"
                key={key}
              >
                <div className="flex flex-col gap-1 min-w-0">
                  <div className="flex items-center gap-2 min-w-0">
                    <strong className="text-[13px] font-semibold text-text truncate">
                      {server.name}
                    </strong>
                    <code className="text-[11px] font-mono text-text-3">{server.scope}</code>
                  </div>
                  <span
                    className="text-[12px] text-text-3 truncate"
                    title={describeMcpServer(server)}
                  >
                    {describeMcpServer(server)}
                  </span>
                  <span
                    className="text-[11px] text-text-3 truncate"
                    title={`trust: ${mcpTrustLabel(server.trust?.preset)}${server.trust?.inferred ? " inferred" : ""}`}
                  >
                    trust: {mcpTrustLabel(server.trust?.preset)}
                    {server.trust?.inferred ? " inferred" : ""}
                  </span>
                  <span
                    className="text-[11px] text-text-3 truncate"
                    title={`channels: ${
                      server.channelPermissions?.allowedChannels.length
                        ? server.channelPermissions.allowedChannels.join(", ")
                        : "not set"
                    }${server.channelPermissions?.allowMutating ? " · mutating allowed" : " · read-only"}`}
                  >
                    channels:{" "}
                    {server.channelPermissions?.allowedChannels.length
                      ? server.channelPermissions.allowedChannels.join(", ")
                      : "not set"}
                    {server.channelPermissions?.allowMutating
                      ? " · mutating allowed"
                      : " · read-only"}
                  </span>
                  {server.policyDiagnostics?.length ? (
                    <div className="flex flex-col gap-0.5">
                      {server.policyDiagnostics.map((diagnostic) => (
                        <span
                          className={`text-[11px] truncate ${
                            diagnostic.severity === "warning"
                              ? "text-warning"
                              : diagnostic.severity === "error"
                                ? "text-danger"
                                : "text-text-3"
                          }`}
                          key={diagnostic.code}
                        >
                          {diagnostic.message}
                        </span>
                      ))}
                    </div>
                  ) : null}
                  {probe ? (
                    <span
                      className={`text-[11px] truncate ${probe.ok ? "text-accent-2" : "text-danger"}`}
                    >
                      {probe.ok
                        ? `${probe.toolCount} tools · ${probe.latencyMs}ms`
                        : probe.error || "Probe failed"}
                    </span>
                  ) : null}
                </div>
                <code className="text-[11px] font-mono text-text-3">{server.type}</code>
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
                  disabled={pendingId === `probe:${key}`}
                  onClick={() => onProbe(server)}
                >
                  <Icon name="activity" /> Test
                </button>
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
                  disabled={pendingId === key}
                  onClick={() => setPendingRemove(server)}
                >
                  <Icon name="trash" /> Remove
                </button>
              </article>
            );
          })}
        </div>
      ) : (
        <EmptyState icon={<Icon name="server" size={18} />} title="No MCP servers configured.">{"Add a workspace or user server above to expose MCP tools to sessions."}</EmptyState>
      )}
      {pendingRemove !== null ? (
        <ConfirmDialog
          title="Remove MCP server?"
          description={`Remove server "${pendingRemove?.name ?? ""}"? Tools it provides become unavailable to sessions.`}
          confirmLabel="Remove"
          danger
          onCancel={() => setPendingRemove(null)}
          onConfirm={() => {
            if (pendingRemove) onRemove(pendingRemove);
            setPendingRemove(null);
          }}
        />
      ) : null}
    </section>
  );
}
