import { Select } from "@rightkit/react-shell/react";
import type { ConstitutionSource } from "@rightkit/agent-ui-core/lib/types";
import type { SpecConstitutionPanelProps, SpecConstitutionVerdictProps } from "./types.js";

function SpecConstitutionVerdict({ verdict }: SpecConstitutionVerdictProps) {
  const className =
    verdict === "compliant"
      ? "border-accent-2 text-accent-2"
      : verdict === "overridden"
        ? "border-warning text-warning"
        : "border-danger text-danger";
  return (
    <span
      className={`rounded border bg-bg px-2 py-0.5 text-[10px] font-semibold uppercase ${className}`}
    >
      {verdict}
    </span>
  );
}

function SpecConstitutionSources({
  sources,
  onDraft,
}: {
  sources: SpecConstitutionPanelProps["spec"]["constitutionSources"];
  onDraft: SpecConstitutionPanelProps["onDraft"];
}) {
  if (sources.length === 0) {
    return (
      <div className="px-3 py-3 text-[12px] text-text-3">
        No constitution source is attached to this spec.
      </div>
    );
  }
  return (
    <div className="flex flex-col divide-y divide-line">
      {sources.map((source: ConstitutionSource) => (
        <button
          className="min-w-0 px-3 py-2 text-left hover:bg-panel-2"
          key={`${source.path}:${source.bodySha256}`}
          type="button"
          onClick={() =>
            onDraft({
              sourcePath: source.path,
              sourceSha256: source.bodySha256,
            })
          }
        >
          <div className="flex items-center gap-2">
            <span className="rounded border border-line bg-bg px-2 py-0.5 text-[10px] uppercase text-text-3">
              {source.scope}
            </span>
            <code className="min-w-0 flex-1 truncate text-[11px] text-accent-2">{source.path}</code>
          </div>
          <code className="mt-1 block truncate text-[10px] text-text-3">{source.bodySha256}</code>
        </button>
      ))}
    </div>
  );
}

function SpecConstitutionDecisions({
  decisions,
}: {
  decisions: SpecConstitutionPanelProps["decisions"];
}) {
  if (decisions.length === 0) {
    return (
      <div className="px-3 py-3 text-[12px] text-text-3">
        No constitution decisions recorded yet.
      </div>
    );
  }
  return (
    <div className="max-h-64 overflow-y-auto">
      {decisions.map((decision) => (
        <div className="border-b border-line px-3 py-2 last:border-b-0" key={decision.id}>
          <div className="flex items-center gap-2">
            <SpecConstitutionVerdict verdict={decision.verdict} />
            <code className="min-w-0 flex-1 truncate text-[11px] text-accent-2">
              {decision.rule}
            </code>
          </div>
          <p className="mt-1 text-[12px] leading-relaxed text-text-2">
            {decision.rationale || "No rationale recorded."}
          </p>
          <p className="mt-1 truncate text-[10px] text-text-3">
            {decision.sourcePath} · evidence {decision.evidenceIds.join(", ") || "none"}
          </p>
        </div>
      ))}
    </div>
  );
}

function SpecConstitutionDraftForm({
  draft,
  pending,
  onDraft,
  onRecord,
}: {
  draft: SpecConstitutionPanelProps["draft"];
  pending: boolean;
  onDraft: SpecConstitutionPanelProps["onDraft"];
  onRecord: SpecConstitutionPanelProps["onRecord"];
}) {
  return (
    <div className="min-w-0 px-3 py-3">
      <div className="grid grid-cols-2 gap-2">
        <input
          aria-label="Source path"
          className="h-8 rounded-md border border-line bg-bg px-3 text-[12px] text-text outline-none placeholder:text-text-3"
          placeholder="Source path"
          value={draft.sourcePath}
          onChange={(event) => onDraft({ sourcePath: event.target.value })}
        />
        <input
          aria-label="Source sha256"
          className="h-8 rounded-md border border-line bg-bg px-3 text-[12px] text-text outline-none placeholder:text-text-3"
          placeholder="Source sha256"
          value={draft.sourceSha256}
          onChange={(event) => onDraft({ sourceSha256: event.target.value })}
        />
      </div>
      <input
        aria-label="Rule or requirement"
        className="mt-2 h-8 w-full rounded-md border border-line bg-bg px-3 text-[12px] text-text outline-none placeholder:text-text-3"
        placeholder="Rule or requirement"
        value={draft.rule}
        onChange={(event) => onDraft({ rule: event.target.value })}
      />
      <Select value={draft.verdict} options={[{ value: "compliant", label: "Compliant" }, { value: "violation", label: "Violation" }, { value: "overridden", label: "Overridden" }]} onChange={(value) => onDraft({ verdict: value as SpecConstitutionPanelProps["draft"]["verdict"] })} label="Verdict" className="mt-2 h-8 w-full rounded-md border border-line bg-bg px-3 text-[12px] text-text outline-none" />
      <textarea
        aria-label="Rationale"
        className="mt-2 min-h-20 w-full resize-y rounded-md border border-line bg-bg px-3 py-2 text-[12px] leading-relaxed text-text outline-none placeholder:text-text-3"
        placeholder="Rationale"
        value={draft.rationale}
        onChange={(event) => onDraft({ rationale: event.target.value })}
      />
      <input
        aria-label="Evidence IDs"
        className="mt-2 h-8 w-full rounded-md border border-line bg-bg px-3 text-[12px] text-text outline-none placeholder:text-text-3"
        placeholder="Evidence IDs"
        value={draft.evidenceIds}
        onChange={(event) => onDraft({ evidenceIds: event.target.value })}
      />
      <button
        className="mt-2 inline-flex h-8 items-center rounded-md border border-line bg-raised px-3 text-[12px] font-semibold text-text disabled:cursor-not-allowed disabled:opacity-50"
        disabled={pending || !draft.rule.trim()}
        type="button"
        onClick={onRecord}
      >
        {pending ? "Recording" : "Record decision"}
      </button>
    </div>
  );
}

export default function SpecConstitutionPanel({
  spec,
  decisions,
  draft,
  pending,
  onDraft,
  onRecord,
}: SpecConstitutionPanelProps) {
  const violations = decisions.filter((decision) => decision.verdict === "violation").length;
  const overrides = decisions.filter((decision) => decision.verdict === "overridden").length;
  return (
    <div className="mt-3 rounded-md border border-line bg-panel">
      <div className="flex items-center gap-3 border-b border-line px-3 py-2">
        <strong className="text-[12px] text-text">Constitution review</strong>
        <span className="text-[11px] text-text-3">
          {spec.constitutionSources.length} sources · {decisions.length} decisions · {violations}{" "}
          violations · {overrides} overrides
        </span>
      </div>
      <div className="grid grid-cols-[minmax(0,0.52fr)_minmax(260px,0.48fr)] divide-x divide-line">
        <div className="min-w-0">
          <div className="border-b border-line px-3 py-2 text-[10px] font-semibold uppercase text-text-3">
            Sources
          </div>
          <SpecConstitutionSources sources={spec.constitutionSources} onDraft={onDraft} />
          <div className="border-y border-line px-3 py-2 text-[10px] font-semibold uppercase text-text-3">
            Decisions
          </div>
          <SpecConstitutionDecisions decisions={decisions} />
        </div>
        <SpecConstitutionDraftForm
          draft={draft}
          pending={pending}
          onDraft={onDraft}
          onRecord={onRecord}
        />
      </div>
    </div>
  );
}

export { SpecConstitutionVerdict };
