import { EmptyState } from "@rightkit/react-shell/react";
import type {
  SpecCreateDraftProps,
  SpecListPaneProps,
  SpecSelectionCallbacks,
  SpecStatusBadgeProps,
} from "./types.js";

function SpecStatusBadge({ status }: SpecStatusBadgeProps) {
  const color =
    status === "done"
      ? "border-accent-2 text-accent-2"
      : status === "blocked"
        ? "border-danger text-danger"
        : status === "implementing"
          ? "border-accent-2 text-accent-2"
          : "border-line text-text-3";
  return (
    <span className={`rounded border bg-panel px-2 py-0.5 text-[10px] font-semibold ${color}`}>
      {status}
    </span>
  );
}

function SpecCreateForm({ draft, pending, onDraft, onCreate }: SpecCreateDraftProps) {
  return (
    <div className="border-b border-line px-5 py-3">
      <input
        aria-label="Title"
        className="mb-2 h-8 w-full rounded-md border border-line bg-panel px-3 text-[12px] text-text outline-none placeholder:text-text-3"
        placeholder="Title"
        value={draft.title}
        onChange={(event) => onDraft({ title: event.target.value })}
      />
      <textarea
        aria-label="Feature description"
        className="min-h-24 w-full resize-y rounded-md border border-line bg-panel px-3 py-2 text-[12px] leading-relaxed text-text outline-none placeholder:text-text-3"
        placeholder="Feature description"
        value={draft.description}
        onChange={(event) => onDraft({ description: event.target.value })}
      />
      <button
        className="mt-2 inline-flex h-8 items-center rounded-md border border-line bg-raised px-3 text-[12px] font-semibold text-text disabled:cursor-not-allowed disabled:opacity-50"
        disabled={pending === "create" || !draft.description.trim()}
        type="button"
        onClick={onCreate}
      >
        {pending === "create" ? "Creating" : "Create spec"}
      </button>
    </div>
  );
}

function SpecListItems({
  specs,
  selectedSpecId,
  onSelect,
}: SpecSelectionCallbacks & {
  specs: SpecListPaneProps["specs"];
  selectedSpecId: SpecListPaneProps["selectedSpecId"];
}) {
  return (
    <div className="flex flex-col divide-y divide-line">
      {specs.map((spec) => (
        <button
          className={`min-w-0 px-5 py-3 text-left ${
            selectedSpecId === spec.id ? "bg-raised" : "bg-transparent hover:bg-panel"
          }`}
          key={spec.id}
          type="button"
          onClick={() => onSelect(spec.id)}
        >
          <div className="flex items-center gap-2">
            <strong className="min-w-0 flex-1 truncate text-[13px] font-semibold text-text">
              {spec.title}
            </strong>
            <SpecStatusBadge status={spec.status} />
          </div>
          <p className="mt-1 truncate text-[11px] text-text-3">
            {spec.functionalRequirements.length} req · {spec.successCriteria.length} criteria
          </p>
          <code className="mt-1 block truncate text-[11px] text-accent-2">{spec.id}</code>
        </button>
      ))}
    </div>
  );
}

export default function SpecListPane({
  specs,
  selectedSpecId,
  draft,
  pending,
  onSelect,
  onDraft,
  onCreate,
}: SpecListPaneProps) {
  return (
    <div className="min-w-0">
      <div className="border-b border-line px-5 py-2 text-[11px] font-semibold uppercase text-text-3">
        Specs
      </div>
      <SpecCreateForm draft={draft} pending={pending} onDraft={onDraft} onCreate={onCreate} />
      {specs.length === 0 ? (
        <EmptyState title="No specs">{"Spec-bound feature work appears here after an operator creates a feature spec."}</EmptyState>
      ) : (
        <SpecListItems specs={specs} selectedSpecId={selectedSpecId} onSelect={onSelect} />
      )}
    </div>
  );
}

export { SpecStatusBadge };
