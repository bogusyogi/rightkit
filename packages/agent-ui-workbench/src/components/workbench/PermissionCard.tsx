// apps/coderight-tauri/src/components/workbench/PermissionCard.tsx
// Phase 5 — extracted the inline permission card from WorkbenchContent.tsx
// (lines 244-251) and tightened the a11y semantics. The original <article>
// became <section role="region" aria-label="Permission request"> so screen
// readers announce it as a labeled region instead of an unlabeled article.
//
// Visual pass: converged onto the shared "ask card" pattern used by
// PlanPrompt / QuestionPrompt — neutral card chrome, a single state-colored
// badge in the header (the one accent-family touch per brand.md), primary
// action first, disclosure content above the action row.

import type { ReactElement } from "react";
import type { PendingApproval as ApprovalState } from "@rightkit/agent-ui-hooks/hooks/useApprovals";
import type { SessionRow } from "@rightkit/agent-ui-core/lib/types";
import type { DiffHunk, DiffHunkDecision } from "@rightkit/agent-ui-hooks/components/workbench/DiffView";
import { approvalActionLabel } from "./ApprovalDetails.js";

type PermissionCardApproval = Pick<ApprovalState, "id"> &
  Partial<Omit<ApprovalState, "id">> & {
    summary?: string;
    hunks?: DiffHunk[];
  };

type PermissionCardProps = {
  pendingApproval: PermissionCardApproval;
  selected: SessionRow | null;
  onApprove: () => void;
  onDeny: () => void;
  onStageHunk: (hunk: DiffHunk, decision: DiffHunkDecision) => Promise<void> | void;
};

export default function PermissionCard({
  pendingApproval,
  selected,
  onApprove,
  onDeny,
  onStageHunk,
}: PermissionCardProps): ReactElement {
  const firstHunk = pendingApproval.hunks?.[0];
  const title = pendingApproval.name
    ? approvalActionLabel(pendingApproval.name)
    : "Permission needed";

  return (
    <section
      aria-label="Permission request"
      // A permission request blocks the run until answered, but role="region"
      // is never announced when it appears — a screen-reader user simply sat
      // there while the agent waited. `alertdialog` announces on arrival and
      // tells assistive tech this needs a decision, without the focus-stealing
      // of a true modal.
      role="alertdialog"
      aria-live="assertive"
      className="cr-msg-in self-start flex max-w-[80%] flex-col gap-4 rounded-xl border border-line bg-panel p-4"
    >
      <header className="flex items-center gap-2.5">
        <span
          aria-hidden="true"
          className="grid h-6 w-6 shrink-0 place-items-center rounded-full border border-warning/40 bg-warning/10 text-[13px] font-semibold text-warning"
        >
          !
        </span>
        <div className="min-w-0 flex-1">
          <h3 className="text-[13px] font-semibold text-text">{title}</h3>
          {pendingApproval.name ? (
            <span className="block truncate text-[11px] text-text-3">{pendingApproval.name}</span>
          ) : null}
        </div>
      </header>
      {(pendingApproval.summary ?? pendingApproval.detail) ? (
        <p className="text-[13px] leading-relaxed text-text">
          {pendingApproval.summary ?? pendingApproval.detail}
        </p>
      ) : null}
      {selected ? (
        <p className="text-[11px] text-text-3">
          Session: <span className="font-mono">{selected.title}</span>
        </p>
      ) : null}
      {pendingApproval.hunks && pendingApproval.hunks.length > 0 ? (
        <details className="text-[11px] text-text-3">
          <summary className="w-fit cursor-pointer select-none hover:text-text-2">
            Show {pendingApproval.hunks.length} change hunks
          </summary>
          <ul className="mt-2 grid gap-1.5">
            {pendingApproval.hunks.map((hunk) => (
              <li
                key={hunk.id}
                className="rounded-md border border-line p-2 font-mono text-[10px] leading-relaxed text-text-2"
              >
                {hunk.patch || hunk.filePath || hunk.header}
              </li>
            ))}
          </ul>
          <div className="mt-2 flex items-center gap-2">
            <button
              type="button"
              className="cr-control-pill inline-flex h-6 items-center gap-1 rounded-md border border-line bg-transparent px-2 text-[11px] text-text-2 hover:border-line-2 hover:text-text"
              disabled={!firstHunk}
              onClick={() => firstHunk && onStageHunk(firstHunk, "approve")}
            >
              Allow this hunk
            </button>
          </div>
        </details>
      ) : null}
      <div className="flex flex-wrap items-center gap-2 pt-1">
        <button
          type="button"
          className="cr-control-pill inline-flex h-8 items-center gap-1.5 rounded-lg border-0 bg-accent px-4 text-[13px] font-semibold text-on-accent hover:opacity-90"
          onClick={onApprove}
          data-decision="allow"
        >
          Allow once
        </button>
        <button
          type="button"
          className="cr-control-pill inline-flex h-8 items-center gap-1.5 rounded-lg border border-line bg-transparent px-3 text-[13px] text-text-2 hover:bg-panel-2 hover:text-text"
          onClick={onDeny}
          data-decision="deny"
        >
          Deny
        </button>
        <kbd className="ml-auto rounded border border-line px-1.5 py-0.5 text-[10px] text-text-3">
          Y / N
        </kbd>
      </div>
    </section>
  );
}
