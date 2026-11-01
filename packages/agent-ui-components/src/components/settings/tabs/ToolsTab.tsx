import { useState } from "react";
import { Checkbox, ConfirmDialog } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { ToolsetDraft } from "../types";
import { OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";

import type {
  SettingsToolsetsResponse,
  ToolGatewaySnapshot,
  ToolGatewayStatus,
  ToolsetProfile,
} from "@rightkit/agent-ui-core/lib/types";

function toolsetDraftFromProfile(toolset: ToolsetProfile): ToolsetDraft {
  return {
    id: toolset.id,
    name: toolset.name,
    description: toolset.description,
    allowedTools: toolset.allowedTools.join(", "),
    disallowedTools: toolset.disallowedTools.join(", "),
    readOnly: toolset.readOnly,
    defaultBehavioralSkills: toolset.defaultBehavioralSkills.join(", "),
  };
}

function ToolsetList({
  title,
  toolsets,
  empty,
  pendingId,
  onSelect,
  onDelete,
}: {
  title: string;
  toolsets: ToolsetProfile[];
  empty: string;
  pendingId: string | null;
  onSelect: (toolset: ToolsetProfile) => void;
  onDelete?: (id: string) => void;
}) {
  return (
    <div className="flex flex-col border border-line rounded-xl overflow-hidden min-h-[260px]">
      <div className="px-4 py-3 border-b border-line bg-panel">
        <h4 className="text-[13px] font-semibold text-text">{title}</h4>
      </div>
      <div className="flex flex-col divide-y divide-line">
        {toolsets.length ? (
          toolsets.map((toolset) => (
            <div key={toolset.id} className="grid grid-cols-[minmax(0,1fr)_auto] gap-3 px-4 py-3">
              <button
                type="button"
                className="min-w-0 border-0 bg-transparent p-0 text-left cursor-pointer"
                onClick={() => onSelect(toolset)}
              >
                <strong className="block truncate text-[13px] text-text">
                  {toolset.name || toolset.id}
                </strong>
                <span className="block truncate text-[11px] font-mono text-accent-2">
                  {toolset.id}
                </span>
                <span className="block truncate text-[12px] text-text-3">
                  allow {toolset.allowedTools.join(", ") || "default"} · deny{" "}
                  {toolset.disallowedTools.join(", ") || "none"}
                </span>
              </button>
              {onDelete ? (
                <button
                  type="button"
                  className="inline-grid h-8 w-8 place-items-center rounded-lg border border-danger/50 bg-transparent text-danger cursor-pointer hover:bg-danger/10 disabled:opacity-40"
                  disabled={pendingId === `delete:${toolset.id}`}
                  onClick={() => onDelete(toolset.id)}
                  title={`Delete ${toolset.id}`}
                >
                  <Icon name="trash" size={13} />
                </button>
              ) : null}
            </div>
          ))
        ) : (
          <OfflinePanel title={empty} action="Save a profile below to add one." />
        )}
      </div>
    </div>
  );
}

function GatewayProfilesPanel({ gateways }: { gateways: ToolGatewayStatus[] }) {
  return (
    <div className="flex flex-col gap-3 border border-line rounded-xl p-4">
      <div className="flex flex-col gap-1">
        <h4 className="text-[13px] font-semibold text-text">Gateway profiles</h4>
        <p className="text-[12px] text-text-3">
          Redacted provider-backed tool bundles available to named toolsets.
        </p>
      </div>
      <div className="grid grid-cols-3 gap-3">
        {gateways.length ? (
          gateways.map((gateway) => (
            <article
              key={gateway.id}
              className="flex flex-col gap-2 rounded-lg border border-line bg-panel p-3"
            >
              <div className="flex items-center justify-between gap-2">
                <strong className="min-w-0 truncate text-[13px] text-text">{gateway.name}</strong>
                <code className="shrink-0 text-[11px] font-mono text-accent-2">
                  {gateway.status}
                </code>
              </div>
              <p className="line-clamp-2 text-[12px] leading-snug text-text-3">
                {gateway.description}
              </p>
              <div className="grid gap-1 text-[11px] text-text-3">
                <span className="truncate">toolset {gateway.toolset}</span>
                <span className="truncate">provider {gateway.provider}</span>
                <span className="truncate">secrets {gateway.secretStatus}</span>
              </div>
              <p className="truncate text-[11px] font-mono text-text-3">
                {gateway.tools
                  .map((tool) => `${tool.id}:${tool.available ? "ready" : "missing"}`)
                  .join(", ")}
              </p>
            </article>
          ))
        ) : (
          <OfflinePanel
            title="No gateway profiles loaded."
            action="Refresh toolsets to inspect provider-backed gateways."
          />
        )}
      </div>
    </div>
  );
}

export default function ToolsTab({
  snapshot,
  gatewaySnapshot,
  draft,
  pendingId,
  error,
  onDraft,
  onRefresh,
  onSave,
  onDelete,
}: {
  snapshot: SettingsToolsetsResponse | null;
  gatewaySnapshot: ToolGatewaySnapshot | null;
  draft: ToolsetDraft;
  pendingId: string | null;
  error: string | null;
  onDraft: (draft: ToolsetDraft) => void;
  onRefresh: () => void;
  onSave: () => void;
  onDelete: (id: string) => void;
}) {
  const [pendingDelete, setPendingDelete] = useState<ToolsetProfile | null>(null);
  return (
    <section className="flex flex-col gap-5 p-6">
      <div className="flex items-start gap-3">
        <div className="flex flex-col gap-1 flex-1 min-w-0">
          <h3 className="text-[16px] font-bold text-text">Tools</h3>
          <p className="text-[13px] text-text-2 truncate">
            {snapshot?.path || "Workspace toolset config will appear after the engine connects."}
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

      {error ? <OfflinePanel title="Toolsets unavailable." action={error} /> : null}

      <div className="grid grid-cols-[minmax(280px,0.9fr)_minmax(0,1.1fr)] gap-4">
        <ToolsetList
          title="Configured toolsets"
          toolsets={snapshot?.configured ?? []}
          empty="No workspace toolsets configured yet."
          pendingId={pendingId}
          onSelect={(toolset) => onDraft(toolsetDraftFromProfile(toolset))}
          onDelete={(id) => {
            const toolset = (snapshot?.configured ?? []).find((t) => t.id === id);
            if (toolset) setPendingDelete(toolset);
          }}
        />
        <ToolsetList
          title="Effective toolsets"
          toolsets={snapshot?.effective ?? []}
          empty="Effective toolsets will appear after refresh."
          pendingId={pendingId}
          onSelect={(toolset) => onDraft(toolsetDraftFromProfile(toolset))}
        />
      </div>
      {pendingDelete !== null ? (
        <ConfirmDialog
          title="Delete toolset?"
          description={`Delete toolset "${pendingDelete?.name || pendingDelete?.id || ""}"? Sessions referencing it fall back to the default toolset.`}
          confirmLabel="Delete"
          danger
          onCancel={() => setPendingDelete(null)}
          onConfirm={() => {
            if (pendingDelete) onDelete(pendingDelete.id);
            setPendingDelete(null);
          }}
        />
      ) : null}

      <GatewayProfilesPanel gateways={gatewaySnapshot?.gateways ?? []} />

      <div className="flex flex-col gap-3 border border-line rounded-xl p-4">
        <div className="grid grid-cols-[minmax(140px,0.7fr)_minmax(160px,0.9fr)_minmax(0,1.4fr)_auto] gap-3 items-end">
          <label className="flex flex-col gap-1 text-[13px] text-text-2">
            Id
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none font-mono"
              value={draft.id}
              onChange={(event) => onDraft({ ...draft, id: event.target.value })}
              placeholder="remote-safe"
            />
          </label>
          <label className="flex flex-col gap-1 text-[13px] text-text-2">
            Name
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={draft.name}
              onChange={(event) => onDraft({ ...draft, name: event.target.value })}
              placeholder="Remote safe"
            />
          </label>
          <label className="flex flex-col gap-1 text-[13px] text-text-2">
            Description
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={draft.description}
              onChange={(event) => onDraft({ ...draft, description: event.target.value })}
              placeholder="Remote-safe profile for chat channels"
            />
          </label>
          <Checkbox
            label="Read-only"
            checked={draft.readOnly}
            onChange={(readOnly) => onDraft({ ...draft, readOnly })}
          />
        </div>
        <div className="grid grid-cols-3 gap-3">
          <label className="flex flex-col gap-1 text-[13px] text-text-2">
            Allowed tools
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none font-mono"
              value={draft.allowedTools}
              onChange={(event) => onDraft({ ...draft, allowedTools: event.target.value })}
              placeholder="read_file, search"
            />
          </label>
          <label className="flex flex-col gap-1 text-[13px] text-text-2">
            Disallowed tools
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none font-mono"
              value={draft.disallowedTools}
              onChange={(event) => onDraft({ ...draft, disallowedTools: event.target.value })}
              placeholder="shell, write_file"
            />
          </label>
          <label className="flex flex-col gap-1 text-[13px] text-text-2">
            Default behavioral skills
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none font-mono"
              value={draft.defaultBehavioralSkills}
              onChange={(event) =>
                onDraft({ ...draft, defaultBehavioralSkills: event.target.value })
              }
              placeholder="cr-baseline-behavior"
            />
          </label>
        </div>
        <div className="flex items-center justify-end gap-2">
          <button
            type="button"
            className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
            disabled={pendingId === "save" || !draft.id.trim()}
            onClick={onSave}
          >
            <Icon name="edit" /> Save toolset
          </button>
        </div>
      </div>
    </section>
  );
}
