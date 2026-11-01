import { EmptyState } from "@rightkit/react-shell/react";
import { useState } from "react";
import type { WebhookTrigger, WebhookTriggerDeliveryAuditRow } from "@rightkit/agent-ui-core/lib/types";
import type { WebhookTriggerDraft } from "@rightkit/agent-ui-hooks/hooks/useRuntimeAggregates";
import { ActionDialog, Checkbox, Select } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { hostAppName } from "../../appName";

function deliveryEndpoint(daemonUrl: string, trigger: WebhookTrigger): string {
  const base = daemonUrl.replace(/\/+$/, "");
  return `${base}/triggers/${encodeURIComponent(trigger.id)}/deliver/${trigger.provider}`;
}

function formatAuditTime(value: string): string {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

export interface WebhookTriggersRuntimePanelProps {
  connected: boolean;
  daemonUrl: string;
  triggers: WebhookTrigger[];
  audit: WebhookTriggerDeliveryAuditRow[];
  loading: boolean;
  error: string | null;
  pendingId: string | null;
  draft: WebhookTriggerDraft;
  onRefresh: () => void;
  onDraft: (patch: Partial<WebhookTriggerDraft>) => void;
  onEdit: (trigger: WebhookTrigger) => void;
  onResetDraft: () => void;
  onSave: () => void;
  onEnabled: (id: string, enabled: boolean) => void;
  onDelete: (id: string) => void;
}

export default function WebhookTriggersRuntimePanel({
  connected,
  daemonUrl,
  triggers,
  audit,
  loading,
  error,
  pendingId,
  draft,
  onRefresh,
  onDraft,
  onEdit,
  onResetDraft,
  onSave,
  onEnabled,
  onDelete,
}: WebhookTriggersRuntimePanelProps) {
  const [pendingDelete, setPendingDelete] = useState<string | null>(null);
  const [copiedId, setCopiedId] = useState<string | null>(null);
  const saving = pendingId === "create" || pendingId === `edit:${draft.editingId}`;
  const canSave =
    connected &&
    draft.workspaceRoot.trim().length > 0 &&
    draft.prompt.trim().length > 0 &&
    (Boolean(draft.editingId) || draft.secretEnv.trim().length > 0) &&
    !saving;

  const copyEndpoint = async (trigger: WebhookTrigger) => {
    try {
      await navigator.clipboard.writeText(deliveryEndpoint(daemonUrl, trigger));
      setCopiedId(trigger.id);
      window.setTimeout(() => setCopiedId(null), 1600);
    } catch {
      setCopiedId(null);
    }
  };

  return (
    <div className="flex-1 min-h-0 overflow-y-auto">
      <div className="border-b border-line bg-panel px-5 py-4">
        <div className="mb-4 flex flex-wrap items-start justify-between gap-3">
          <div>
            <h3 className="text-[13px] font-semibold text-text">Inbound webhook trigger</h3>
            <p className="mt-1 max-w-2xl text-[12px] leading-relaxed text-text-3">
              Signed events start an agent session in Ask mode. Enter an environment-variable name;
              {hostAppName()} never stores or returns its secret value.
            </p>
          </div>
          <button
            type="button"
            className="inline-flex h-8 items-center gap-2 rounded-md border border-line bg-panel-2 px-3 text-[12px] text-text-2 hover:border-line-2 hover:text-text disabled:cursor-not-allowed disabled:opacity-50"
            disabled={!connected || loading}
            onClick={onRefresh}
          >
            <Icon name="refresh" size={14} />
            {loading ? "Refreshing" : "Refresh"}
          </button>
        </div>

        {!connected ? (
          <div
            role="status"
            className="mb-4 rounded-md border border-warning/40 bg-warning/10 px-3 py-2 text-[12px] text-warning"
          >
            Connect the daemon to manage webhook triggers.
          </div>
        ) : null}
        {error ? (
          <div
            role="alert"
            className="mb-4 flex items-center justify-between gap-3 rounded-md border border-danger/40 bg-danger/10 px-3 py-2 text-[12px] text-danger"
          >
            <span>{error}</span>
            <button
              type="button"
              className="shrink-0 rounded border border-danger/40 px-2 py-1 text-[11px] hover:bg-danger/10"
              onClick={onRefresh}
            >
              Try again
            </button>
          </div>
        ) : null}

        <div className="grid grid-cols-1 gap-3 lg:grid-cols-[minmax(120px,0.65fr)_minmax(120px,0.75fr)_minmax(220px,1.5fr)]">
          <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
            Id
            <input
              aria-label="Trigger id"
              className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none focus:border-accent disabled:opacity-50"
              value={draft.id}
              disabled={Boolean(draft.editingId) || saving}
              placeholder="generated when blank"
              onChange={(event) => onDraft({ id: event.target.value })}
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
            Provider
            <Select
              label="Trigger provider"
              value={draft.provider}
              disabled={saving}
              onChange={(provider) => onDraft({ provider: provider === "generic" ? "generic" : "github" })}
              options={[
                { value: "github", label: "GitHub" },
                { value: "generic", label: "Generic HMAC" },
              ]}
            />
          </label>

          <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
            Title
            <input
              aria-label="Trigger title"
              className="h-8 rounded-md border border-line bg-panel-2 px-2 text-[12px] normal-case tracking-normal text-text outline-none focus:border-accent disabled:opacity-50"
              value={draft.title}
              disabled={saving}
              placeholder="Webhook: repository review"
              onChange={(event) => onDraft({ title: event.target.value })}
            />
          </label>
        </div>

        <label className="mt-3 flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Workspace
          <input
            aria-label="Trigger workspace"
            className="h-8 rounded-md border border-line bg-panel-2 px-2 font-mono text-[12px] normal-case tracking-normal text-text outline-none focus:border-accent disabled:opacity-50"
            value={draft.workspaceRoot}
            disabled={saving}
            onChange={(event) => onDraft({ workspaceRoot: event.target.value })}
          />
        </label>

        <label className="mt-3 flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
          Agent instruction
          <textarea
            aria-label="Trigger agent instruction"
            className="min-h-[78px] resize-y rounded-md border border-line bg-panel-2 px-2 py-2 text-[12px] normal-case tracking-normal text-text outline-none focus:border-accent disabled:opacity-50"
            value={draft.prompt}
            disabled={saving}
            onChange={(event) => onDraft({ prompt: event.target.value })}
          />
        </label>

        <div className="mt-3 grid grid-cols-1 items-end gap-3 lg:grid-cols-[minmax(240px,1fr)_auto_auto]">
          <label className="flex flex-col gap-1 text-[11px] uppercase tracking-wide text-text-3">
            Secret environment variable
            <input
              aria-label="Secret environment variable"
              className="h-8 rounded-md border border-line bg-panel-2 px-2 font-mono text-[12px] normal-case tracking-normal text-text outline-none focus:border-accent disabled:opacity-50"
              value={draft.secretEnv}
              disabled={saving}
              placeholder={
                draft.editingId
                  ? "Leave blank to keep the current binding"
                  : "RIGHTKIT_WEBHOOK_SECRET"
              }
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => onDraft({ secretEnv: event.target.value })}
            />
          </label>

          <Checkbox
            label="Enabled after save"
            checked={draft.enabled}
            disabled={saving}
            onChange={(enabled) => onDraft({ enabled })}
          />

          <div className="flex items-center justify-end gap-2">
            <button
              type="button"
              className="inline-flex h-8 items-center justify-center rounded-md border border-line bg-panel-2 px-3 text-[12px] text-text-2 hover:text-text disabled:opacity-50"
              disabled={saving}
              onClick={onResetDraft}
            >
              Reset
            </button>
            <button
              type="button"
              className="inline-flex h-8 items-center justify-center gap-2 rounded-md border border-accent/60 bg-accent/15 px-3 text-[12px] text-accent hover:bg-accent/20 disabled:cursor-not-allowed disabled:opacity-50"
              disabled={!canSave}
              onClick={onSave}
            >
              <Icon name="check" size={14} />
              {saving ? "Saving" : draft.editingId ? "Update trigger" : "Create trigger"}
            </button>
          </div>
        </div>
      </div>

      {loading && triggers.length === 0 ? (
        <div role="status" className="px-5 py-10 text-center text-[12px] text-text-3">
          Loading webhook triggers…
        </div>
      ) : triggers.length === 0 ? (
        <EmptyState icon={<Icon name="branch" size={18} />} title="No webhook triggers">{"Register a GitHub or generic HMAC endpoint above. The first valid signed delivery will create an Ask-mode agent session."}</EmptyState>
      ) : (
        <div className="flex flex-col divide-y divide-line">
          {triggers.map((trigger) => {
            const endpoint = deliveryEndpoint(daemonUrl, trigger);
            const toggling = pendingId === `enabled:${trigger.id}`;
            return (
              <article className="px-5 py-4" key={trigger.id}>
                <div className="flex flex-wrap items-start gap-3">
                  <div className="min-w-[240px] flex-1">
                    <div className="flex flex-wrap items-center gap-2">
                      <strong className="text-[13px] font-semibold text-text">
                        {trigger.title}
                      </strong>
                      <span
                        className={`rounded border px-1.5 py-0.5 text-[10px] uppercase tracking-wide ${trigger.enabled ? "border-success/40 text-success" : "border-warning/40 text-warning"}`}
                      >
                        {trigger.enabled ? "Enabled" : "Disabled"}
                      </span>
                      <span
                        className={`inline-flex items-center gap-1 rounded border px-1.5 py-0.5 text-[10px] uppercase tracking-wide ${trigger.secretConfigured ? "border-success/40 text-success" : "border-danger/40 text-danger"}`}
                      >
                        <Icon name={trigger.secretConfigured ? "check" : "key"} size={11} />
                        {trigger.secretConfigured ? "Env secret ready" : "Env secret missing"}
                      </span>
                      <span className="rounded border border-line px-1.5 py-0.5 text-[10px] uppercase tracking-wide text-text-3">
                        Ask mode
                      </span>
                    </div>
                    <p className="mt-1 truncate text-[12px] text-text-2" title={trigger.prompt}>
                      {trigger.prompt}
                    </p>
                    <p
                      className="mt-1 truncate font-mono text-[11px] text-text-3"
                      title={trigger.workspaceRoot}
                    >
                      {trigger.workspaceRoot}
                    </p>
                    <div className="mt-2 flex min-w-0 items-center gap-2 rounded-md border border-line bg-panel px-2 py-1.5">
                      <code
                        className="min-w-0 flex-1 truncate text-[11px] text-accent-2"
                        title={endpoint}
                      >
                        {endpoint}
                      </code>
                      <button
                        type="button"
                        aria-label={`Copy ${trigger.id} delivery endpoint`}
                        className="inline-flex h-6 shrink-0 items-center gap-1 rounded px-1.5 text-[11px] text-text-3 hover:bg-panel-2 hover:text-text"
                        onClick={() => void copyEndpoint(trigger)}
                      >
                        <Icon name={copiedId === trigger.id ? "check" : "copy"} size={12} />
                        {copiedId === trigger.id ? "Copied" : "Copy"}
                      </button>
                    </div>
                  </div>

                  <div className="flex items-center gap-1">
                    <button
                      type="button"
                      className="inline-flex h-8 items-center rounded-lg border border-line bg-panel px-2.5 text-[12px] text-text-2 hover:border-accent hover:text-accent disabled:opacity-50"
                      disabled={toggling}
                      onClick={() => onEnabled(trigger.id, !trigger.enabled)}
                    >
                      {toggling ? "Saving" : trigger.enabled ? "Disable" : "Enable"}
                    </button>
                    <button
                      type="button"
                      aria-label={`Edit ${trigger.id}`}
                      className="inline-flex h-8 w-8 items-center justify-center rounded-lg border border-line bg-panel text-text-2 hover:border-accent hover:text-accent disabled:opacity-50"
                      disabled={pendingId === `edit:${trigger.id}`}
                      onClick={() => onEdit(trigger)}
                    >
                      <Icon name="edit" size={14} />
                    </button>
                    <button
                      type="button"
                      aria-label={`Delete ${trigger.id}`}
                      className="inline-flex h-8 w-8 items-center justify-center rounded-lg border border-line bg-panel text-text-2 hover:border-danger hover:text-danger disabled:opacity-50"
                      disabled={pendingId === `delete:${trigger.id}`}
                      onClick={() => setPendingDelete(trigger.id)}
                    >
                      <Icon name="trash" size={14} />
                    </button>
                  </div>
                </div>
              </article>
            );
          })}
        </div>
      )}

      <section
        className="border-t border-line px-5 py-4"
        aria-labelledby="webhook-delivery-audit-heading"
      >
        <div className="mb-3 flex items-center justify-between gap-3">
          <h3
            id="webhook-delivery-audit-heading"
            className="text-[12px] font-semibold uppercase tracking-wide text-text-2"
          >
            Delivery audit
          </h3>
          <span className="text-[11px] text-text-3">Payloads and secrets are redacted</span>
        </div>
        {audit.length === 0 ? (
          <p className="text-[12px] text-text-3">No deliveries recorded.</p>
        ) : (
          <div className="overflow-x-auto">
            <div className="grid min-w-[720px] grid-cols-[150px_120px_110px_minmax(120px,1fr)_minmax(120px,1fr)] gap-x-3 gap-y-2 text-[11px] text-text-3">
              {audit
                .slice(-8)
                .reverse()
                .map((row, index) => (
                  <div
                    className="contents"
                    key={`${row.createdAt}-${row.triggerId}-${row.deliveryId ?? index}`}
                  >
                    <span className="truncate" title={row.createdAt}>
                      {formatAuditTime(row.createdAt)}
                    </span>
                    <code className="truncate font-mono text-accent-2">{row.triggerId}</code>
                    <span className={row.outcome === "accepted" ? "text-success" : "text-danger"}>
                      {row.outcome}
                    </span>
                    <span className="truncate" title={row.reason}>
                      {row.reason}
                    </span>
                    <code
                      className="truncate font-mono"
                      title={row.sessionId || row.deliveryId || ""}
                    >
                      {row.sessionId || row.deliveryId || "—"}
                    </code>
                  </div>
                ))}
            </div>
          </div>
        )}
      </section>

      {pendingDelete !== null ? (
        <ActionDialog
          title="Delete webhook trigger?"
          description={`Delete "${pendingDelete ?? ""}"? Its delivery endpoint will stop accepting events.`}
          role="alertdialog"
          dismissible={!(pendingId === `delete:${pendingDelete}`)}
          onClose={() => setPendingDelete(null)}
          actions={[
            {
              id: "cancel",
              label: "Cancel",
              autoFocus: true,
              disabled: pendingId === `delete:${pendingDelete}`,
              onClick: () => setPendingDelete(null),
            },
            {
              id: "confirm",
              label: "Delete trigger",
              variant: "danger",
              disabled: pendingId === `delete:${pendingDelete}`,
              onClick: () => {
                if (pendingDelete) onDelete(pendingDelete);
                setPendingDelete(null);
              },
            },
          ]}
        />
      ) : null}
    </div>
  );
}
