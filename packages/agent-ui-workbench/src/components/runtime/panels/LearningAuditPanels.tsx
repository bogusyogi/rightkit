import type { AuditLogView, LearningProposal, TasteProposal } from "@rightkit/agent-ui-core/lib/types";
import { RuntimeEmpty, ScoreBar, panelWrap } from "./shared";

export function LearningPanel({
  rows,
  pendingId,
  onDecision,
}: {
  rows: LearningProposal[];
  pendingId: string | null;
  onDecision: (id: string, decision: "approve" | "reject") => void;
}) {
  if (!rows.length)
    return (
      <RuntimeEmpty
        title="No learning proposals"
        body="Approved learning proposals become durable rules, memories, or skill bundles once enough evidence accumulates."
      />
    );
  return (
    <div className={panelWrap}>
      {rows.map((p) => {
        const locked = pendingId === p.id || p.status === "Promoted" || p.status === "Rejected";
        return (
          <div
            key={p.id}
            className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-3"
          >
            <div className="flex items-center gap-2">
              <strong className="text-[13px] font-semibold text-text truncate">{p.title}</strong>
              <span className="text-[10px] uppercase tracking-wider text-text-2 border border-line bg-panel-2 px-2 py-0.5 rounded-md">
                {p.kind}
              </span>
              <span className="flex-1" />
              <span className="text-[11px] font-mono text-text-3">
                {p.status} · {p.confidence.toFixed(2)}
              </span>
            </div>
            <p className="text-[12px] text-text-2 leading-relaxed">{p.body}</p>
            <ScoreBar value={p.confidence} />
            <div className="flex items-center gap-2">
              <button
                type="button"
                className="inline-flex items-center justify-center h-7 px-3 rounded-md border border-accent/40 bg-accent/10 text-accent text-[12px] font-semibold cursor-pointer hover:bg-accent/15 disabled:opacity-40 disabled:cursor-not-allowed"
                disabled={locked}
                onClick={() => onDecision(p.id, "approve")}
              >
                Approve
              </button>
              <button
                type="button"
                className="inline-flex items-center justify-center h-7 px-3 rounded-md border border-line bg-transparent text-text-2 text-[12px] cursor-pointer hover:bg-panel-2 hover:text-text disabled:opacity-40 disabled:cursor-not-allowed"
                disabled={locked}
                onClick={() => onDecision(p.id, "reject")}
              >
                Reject
              </button>
              {pendingId === p.id ? <span className="text-[11px] text-text-3">Saving…</span> : null}
            </div>
          </div>
        );
      })}
    </div>
  );
}

export function ProposalsPanel({ rows }: { rows: TasteProposal[] }) {
  if (!rows.length)
    return (
      <RuntimeEmpty
        title="No proposals"
        body="The taste engine proposes confidence-gated improvements — model swaps, prompt tweaks, escalation changes — once a role accumulates enough signal. Proposals are advisory until approved."
      />
    );
  return (
    <div className={panelWrap}>
      {rows.map((p) => (
        <div
          key={`${p.role}:${p.kind}:${p.rationale}`}
          className="rounded-xl border border-line bg-panel p-4 flex flex-col gap-2"
        >
          <div className="flex items-center gap-2">
            <strong className="text-[13px] font-semibold text-text truncate">{p.role}</strong>
            <span className="text-[10px] uppercase tracking-wider text-text-2 border border-line bg-panel-2 px-2 py-0.5 rounded-md">
              {p.kind}
            </span>
            <span className="flex-1" />
            <span className="text-[11px] font-mono text-text-3">
              conf {p.confidence.toFixed(2)}
            </span>
          </div>
          <p className="text-[12px] text-text-2 leading-relaxed">{p.rationale}</p>
          <ScoreBar value={p.confidence} />
        </div>
      ))}
    </div>
  );
}

// ── audit log ──────────────────────────────────────────────────────────────

export function AuditPanel({ view }: { view: AuditLogView }) {
  const { verified, entries } = view;
  if (!entries.length)
    return (
      <RuntimeEmpty
        title="Audit log empty"
        body="Governance-relevant events — goal completions, model promotions, safety escalations — are appended here in a hash-chained, tamper-evident log."
      />
    );
  return (
    <div className="flex-1 min-h-0 overflow-y-auto flex flex-col">
      <div
        className={`flex items-center gap-2 px-5 py-2.5 text-[12px] font-semibold sticky top-0 ${
          verified ? "text-accent-2 bg-accent-2/10" : "text-danger bg-danger/10"
        }`}
      >
        <span className={`w-2 h-2 rounded-full ${verified ? "bg-accent-2" : "bg-danger"}`} />
        {verified ? "Chain verified — no tampering detected" : "Chain BROKEN — tamper detected"}
        <span className="flex-1" />
        <span className="text-text-3 font-normal">{entries.length} entries</span>
      </div>
      <div className="flex flex-col divide-y divide-line">
        {entries
          .slice()
          .reverse()
          .map((e) => (
            <div
              key={e.seq}
              className="grid grid-cols-[auto_minmax(0,1fr)_auto] items-start gap-3 px-5 py-3"
            >
              <span className="text-[11px] font-mono text-text-3 mt-0.5">#{e.seq}</span>
              <div className="min-w-0 flex flex-col gap-0.5">
                <div className="flex items-center gap-2">
                  <span className="text-[10px] uppercase tracking-wider text-text-3">{e.kind}</span>
                  <strong className="text-[12px] font-semibold text-text truncate">
                    {e.subject}
                  </strong>
                </div>
                <span className="text-[12px] text-text-2 truncate">{e.summary}</span>
              </div>
              <code className="text-[10px] font-mono text-text-3 mt-0.5" title={e.hash}>
                {e.hash.slice(0, 8)}
              </code>
            </div>
          ))}
      </div>
    </div>
  );
}
