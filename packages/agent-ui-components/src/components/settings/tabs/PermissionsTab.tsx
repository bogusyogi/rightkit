import { useState } from "react";
import { ConfirmDialog, Select } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import PersistentPermissionRuleList from "@rightkit/agent-ui-settings/components/settings/tabs/permissions/PersistentPermissionRuleList";
import PermissionPresetGrid from "./permissions/PermissionPresetGrid.js";
import { permissionLabel } from "@rightkit/agent-ui-core/lib/utils";
import { OfflinePanel } from "@rightkit/agent-ui-settings/components/settings/tabs/shared";
import { formatIsoTime } from "@rightkit/agent-ui-settings/components/settings/helpers";

import type { PermissionMode } from "@rightkit/agent-ui-core/lib/daemon";
import type {
  PermissionRuleEffect,
  PermissionRuleRow,
  PersistentPermissionRuleScope,
} from "@rightkit/agent-ui-core/lib/types";
import type { PermissionsTabProps } from "./PermissionsTabProps.js";

const PERMISSION_MODE_DESCRIPTIONS: Record<PermissionMode, string> = {
  plan: "Read and plan only; all mutations are denied.",
  ask: "Ask before every mutating tool call.",
  "accept-edits": "Apply file edits; ask for shell, network, and destructive actions.",
  "auto-safe": "Allow safe operations; ask for destructive, browser, or process control.",
  bypass: "Allow every tool call. Use only in a trusted disposable workspace.",
};

const EFFECT_OPTIONS: ReadonlyArray<{ value: PermissionRuleEffect; label: string }> = [
  { value: "deny", label: "Deny" },
  { value: "ask", label: "Ask" },
  { value: "allow", label: "Allow" },
];

function permissionModeDescription(mode: PermissionMode): string {
  return PERMISSION_MODE_DESCRIPTIONS[mode];
}

export default function PermissionsTab({
  selectedSession,
  permissionMode,
  rules,
  draft,
  pendingId,
  persistentDraft,
  persistentPendingId,
  persistentStorePath,
  persistentUserRules,
  persistentProjectRules,
  persistentError,
  jsonDraft,
  jsonPending,
  denials,
  denialStorePath,
  denialsLoading,
  denialsError,
  onDraft,
  onAdd,
  onRemove,
  onPersistentDraft,
  onPersistentAdd,
  onPersistentRemove,
  onJsonDraft,
  onJsonExport,
  onJsonImport,
  onRefresh,
  onCycleMode,
}: PermissionsTabProps) {
  const [pendingRemove, setPendingRemove] = useState<PermissionRuleRow | null>(null);
  return (
    <section className="flex flex-col gap-5 p-6" aria-labelledby="permissions-tab-title">
      <div className="flex items-start gap-3">
        <div className="flex flex-col gap-1 flex-1 min-w-0">
          <h3 id="permissions-tab-title" className="text-[16px] font-bold text-text">
            Permissions
          </h3>
          <p className="text-[13px] text-text-2 truncate">
            {selectedSession
              ? `${selectedSession.title} · ${selectedSession.id}`
              : "Select a session to edit live rules."}
          </p>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
          disabled={!selectedSession}
          onClick={onRefresh}
        >
          <Icon name="refresh" /> Refresh
        </button>
      </div>

      <div className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-3 max-w-[640px]">
        <div className="flex items-start gap-4 px-4 py-3 rounded-xl border border-line bg-panel min-w-0">
          <span className="text-[12px] text-text-3 w-24 flex-shrink-0">Active mode</span>
          <div className="flex flex-col gap-1 min-w-0">
            <strong className="text-[13px] font-medium text-text truncate">
              {permissionLabel(permissionMode)}
            </strong>
            <p className="text-[12px] leading-4 text-text-3">
              {permissionModeDescription(permissionMode)}
            </p>
          </div>
        </div>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-10 px-4 rounded-lg border border-line bg-transparent text-text-2 text-[13px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
          aria-label="Change permission mode"
          title="Cycle through the permission modes for this session"
          disabled={!selectedSession}
          onClick={onCycleMode}
        >
          <Icon name="lock" /> Change
        </button>
      </div>
      <p className="max-w-[640px] text-[12px] leading-5 text-text-3">
        Permission mode is scoped to the active session. The daemon enforces the selected policy;
        rules below add narrower tool-specific decisions.
      </p>

      <PermissionPresetGrid
        disabled={!selectedSession}
        onSelect={(preset) => onDraft({ pattern: preset.pattern, effect: preset.effect })}
      />

      <div className="grid grid-cols-[minmax(0,1fr)_120px_auto] gap-3 max-w-[640px] items-end">
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Tool pattern
          <input
            className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none disabled:opacity-40"
            value={draft.pattern}
            disabled={!selectedSession}
            onChange={(e) => onDraft({ ...draft, pattern: e.target.value })}
            placeholder="shell or read_*"
          />
        </label>
        <label className="flex flex-col gap-1 text-[13px] text-text-2">
          Effect
          <Select<PermissionRuleEffect>
            label="Effect"
            value={draft.effect}
            disabled={!selectedSession}
            onChange={(effect) => onDraft({ ...draft, effect })}
            options={EFFECT_OPTIONS}
          />
        </label>
        <button
          type="button"
          className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
          disabled={!selectedSession || pendingId === "add"}
          onClick={onAdd}
        >
          <Icon name="plus" /> Add
        </button>
      </div>

      {selectedSession ? (
        rules.length > 0 ? (
          <div className="flex flex-col divide-y divide-line border border-line rounded-xl overflow-hidden">
            {rules.map((rule) => (
              <article
                className="grid grid-cols-[48px_minmax(0,1fr)_90px_90px_auto] items-center gap-4 px-4 py-3"
                key={`${rule.index}:${rule.source}:${rule.pattern}`}
              >
                <code className="text-[11px] font-mono text-text-3">#{rule.index}</code>
                <strong className="text-[13px] font-semibold text-text truncate">
                  {rule.pattern}
                </strong>
                <code className="text-[11px] font-mono text-accent-2">{rule.effect}</code>
                <code className="text-[11px] font-mono text-text-3">{rule.source}</code>
                <button
                  type="button"
                  className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
                  disabled={pendingId === String(rule.index)}
                  onClick={() => setPendingRemove(rule)}
                >
                  <Icon name="trash" /> Remove
                </button>
              </article>
            ))}
          </div>
        ) : (
          <OfflinePanel
            title="No session permission rules."
            action="Add a tool pattern to refine this session."
          />
        )
      ) : (
        <OfflinePanel
          title="No active session."
          action="Open a session before editing permission rules."
        />
      )}

      <div className="flex flex-col gap-3">
        <div className="flex flex-col gap-1 min-w-0">
          <h4 className="text-[14px] font-bold text-text">Inherited rules</h4>
          <p className="text-[12px] text-text-3 truncate">
            {persistentStorePath || "Durable rule store will appear after the engine connects."}
          </p>
        </div>

        {persistentError ? (
          <OfflinePanel title="Durable rules unavailable." action={persistentError} />
        ) : null}

        <PermissionPresetGrid
          keyPrefix="persistent:"
          onSelect={(preset) =>
            onPersistentDraft({
              ...persistentDraft,
              pattern: preset.pattern,
              effect: preset.effect,
            })
          }
        />

        <div className="grid grid-cols-[120px_minmax(0,1fr)_120px_auto] gap-3 max-w-[760px] items-end">
          <label className="flex flex-col gap-1 text-[13px] text-text-2">
            Scope
            <Select<PersistentPermissionRuleScope>
              label="Rule scope"
              value={persistentDraft.scope}
              onChange={(scope) => onPersistentDraft({ ...persistentDraft, scope })}
              options={[
                { value: "user", label: "User" },
                { value: "project", label: "Project" },
              ]}
            />
          </label>
          <label className="flex flex-col gap-1 text-[13px] text-text-2">
            Pattern
            <input
              className="h-9 px-3 rounded-lg border border-line bg-panel text-text text-[13px] outline-none"
              value={persistentDraft.pattern}
              onChange={(e) =>
                onPersistentDraft({
                  ...persistentDraft,
                  pattern: e.target.value,
                })
              }
              placeholder="shell(git push*)"
            />
          </label>
          <label className="flex flex-col gap-1 text-[13px] text-text-2">
            Effect
            <Select<PermissionRuleEffect>
              label="Rule effect"
              value={persistentDraft.effect}
              onChange={(effect) => onPersistentDraft({ ...persistentDraft, effect })}
              options={EFFECT_OPTIONS}
            />
          </label>
          <button
            type="button"
            className="inline-flex items-center gap-2 h-9 px-4 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[13px] cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
            disabled={persistentPendingId === "add"}
            onClick={onPersistentAdd}
          >
            <Icon name="plus" /> Add
          </button>
        </div>

        <div className="grid grid-cols-2 gap-3">
          <PersistentPermissionRuleList
            title="User"
            rules={persistentUserRules}
            pendingId={persistentPendingId}
            onRemove={onPersistentRemove}
          />
          <PersistentPermissionRuleList
            title="Project"
            rules={persistentProjectRules}
            pendingId={persistentPendingId}
            onRemove={onPersistentRemove}
          />
        </div>

        <div className="flex flex-col gap-3 border border-line rounded-xl p-4">
          <div className="flex items-start justify-between gap-3">
            <div className="flex flex-col gap-1 min-w-0">
              <h5 className="text-[13px] font-bold text-text">Rule JSON</h5>
              <p className="text-[12px] text-text-3 truncate">
                Full durable user/project permission-rule store
              </p>
            </div>
            <div className="flex items-center gap-2 flex-shrink-0">
              <button
                type="button"
                className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel hover:text-text disabled:opacity-40"
                disabled={Boolean(jsonPending)}
                onClick={onJsonExport}
              >
                <Icon name="download" size={13} /> Export
              </button>
              <button
                type="button"
                className="inline-flex items-center gap-2 h-8 px-3 rounded-lg border-0 bg-accent text-on-accent font-semibold text-[12px] cursor-pointer hover:opacity-90 disabled:border disabled:border-line disabled:bg-panel-2 disabled:text-text-3"
                disabled={Boolean(jsonPending) || !jsonDraft.trim()}
                onClick={onJsonImport}
              >
                <Icon name="upload" size={13} /> Import
              </button>
            </div>
          </div>
          <textarea
            aria-label="Permissions JSON"
            className="min-h-[160px] px-3 py-2 rounded-lg border border-line bg-panel text-text text-[12px] leading-5 outline-none font-mono resize-y"
            spellCheck={false}
            value={jsonDraft}
            onChange={(event) => onJsonDraft(event.target.value)}
            placeholder='{"user":[],"projects":{}}'
          />
        </div>
      </div>

      <div className="flex flex-col gap-3">
        <div className="flex items-start gap-3">
          <div className="flex flex-col gap-1 flex-1 min-w-0">
            <h4 className="text-[14px] font-bold text-text">Recent denials</h4>
            <p className="text-[12px] text-text-3 truncate">
              {denialStorePath || "Permission-denial ledger will appear after the engine connects."}
            </p>
          </div>
          {denialsLoading ? <span className="text-[12px] text-text-3">Loading...</span> : null}
        </div>

        {denialsError ? (
          <OfflinePanel title="Denial history unavailable." action={denialsError} />
        ) : selectedSession ? (
          denials.length > 0 ? (
            <div className="flex flex-col divide-y divide-line border border-line rounded-xl overflow-hidden">
              {denials.map((row) => (
                <article
                  className="grid grid-cols-[150px_88px_minmax(0,1fr)] items-start gap-4 px-4 py-3"
                  key={row.id}
                >
                  <time className="text-[11px] text-text-3 tabular-nums" dateTime={row.createdAt}>
                    {formatIsoTime(row.createdAt)}
                  </time>
                  <code
                    className="text-[11px] font-mono text-accent-2 truncate"
                    title={row.toolCallId}
                  >
                    {row.toolCallId}
                  </code>
                  <div className="flex flex-col gap-1 min-w-0">
                    <p className="text-[12px] leading-5 text-text-2 break-words" title={row.reason}>
                      {row.reason}
                    </p>
                    <code
                      className="text-[11px] font-mono text-text-3 truncate"
                      title={row.sessionId}
                    >
                      {row.sessionId}
                    </code>
                  </div>
                </article>
              ))}
            </div>
          ) : (
            <OfflinePanel
              title="No recent permission denials."
              action="Denied tool results for this session will appear here."
            />
          )
        ) : (
          <OfflinePanel
            title="No active session."
            action="Open a session to inspect recent permission denials."
          />
        )}
      </div>
      {pendingRemove !== null ? (
        <ConfirmDialog
          title="Remove permission rule?"
          description={`Remove rule "${pendingRemove?.pattern ?? ""}" (${pendingRemove?.effect ?? ""}) from this session?`}
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
