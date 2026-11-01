import { useState } from "react";
import { ConfirmDialog } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { PersistentPermissionRuleRow } from "@rightkit/agent-ui-core/lib/types";

export default function PersistentPermissionRuleList({
  title,
  rules,
  pendingId,
  onRemove,
}: {
  title: string;
  rules: PersistentPermissionRuleRow[];
  pendingId: string | null;
  onRemove: (rule: PersistentPermissionRuleRow) => void;
}) {
  const [pendingRemove, setPendingRemove] = useState<PersistentPermissionRuleRow | null>(null);
  return (
    <div className="flex flex-col border border-line rounded-xl overflow-hidden min-w-0">
      <div className="px-4 py-2 border-b border-line bg-panel">
        <h5 className="text-[12px] font-semibold text-text">{title}</h5>
      </div>
      {rules.length > 0 ? (
        <div className="flex flex-col divide-y divide-line">
          {rules.map((rule) => {
            const key = `${rule.scope}:${rule.workspaceRoot ?? ""}:${rule.index}`;
            return (
              <article
                className="grid grid-cols-[36px_minmax(0,1fr)_70px_auto] items-center gap-3 px-4 py-3"
                key={key}
              >
                <code className="text-[11px] font-mono text-text-3">#{rule.index}</code>
                <div className="flex flex-col gap-0.5 min-w-0">
                  <strong className="text-[13px] font-semibold text-text truncate">
                    {rule.pattern}
                  </strong>
                  {rule.workspaceRoot ? (
                    <span className="text-[11px] text-text-3 truncate">{rule.workspaceRoot}</span>
                  ) : null}
                </div>
                <code className="text-[11px] font-mono text-accent-2">{rule.effect}</code>
                <button
                  type="button"
                  className="inline-flex items-center justify-center h-8 w-8 rounded-lg border border-line bg-transparent text-text-2 cursor-pointer hover:bg-panel hover:text-danger disabled:opacity-40"
                  disabled={pendingId === key}
                  onClick={() => setPendingRemove(rule)}
                  title="Remove durable rule"
                >
                  <Icon name="trash" size={14} />
                </button>
              </article>
            );
          })}
        </div>
      ) : (
        <div className="px-4 py-3 text-[12px] text-text-3">
          No inherited {title.toLowerCase()} rules.
        </div>
      )}
      {pendingRemove !== null ? (
        <ConfirmDialog
          title="Remove durable rule?"
          description={`Remove rule "${pendingRemove?.pattern ?? ""}" (${pendingRemove?.effect ?? ""})? Future matching actions fall back to standard prompts.`}
          confirmLabel="Remove"
          danger
          onCancel={() => setPendingRemove(null)}
          onConfirm={() => {
            if (pendingRemove) onRemove(pendingRemove);
            setPendingRemove(null);
          }}
        />
      ) : null}
    </div>
  );
}
