import type { SpecExecutionActionsProps } from "./types";

function ActionButton({
  disabled,
  pendingLabel,
  onClick,
}: {
  disabled: boolean;
  pendingLabel: string;
  onClick: () => void;
}) {
  return (
    <button
      className="inline-flex h-8 items-center rounded-md border border-line bg-panel px-3 text-[12px] font-semibold text-text-2 disabled:cursor-not-allowed disabled:opacity-50"
      disabled={disabled}
      type="button"
      onClick={onClick}
    >
      {pendingLabel}
    </button>
  );
}

function PrimaryActionButton({
  disabled,
  pendingLabel,
  onClick,
}: {
  disabled: boolean;
  pendingLabel: string;
  onClick: () => void;
}) {
  return (
    <button
      className="inline-flex h-8 items-center rounded-md border border-line bg-raised px-3 text-[12px] font-semibold text-text disabled:cursor-not-allowed disabled:opacity-50"
      disabled={disabled}
      type="button"
      onClick={onClick}
    >
      {pendingLabel}
    </button>
  );
}

export default function SpecExecutionActions({
  specId,
  pending,
  selectedIsBound,
  hasSessionBinding,
  onClarify,
  onPlan,
  onChecklist,
  onAnalyze,
  onImplement,
  onBindSessionSpec,
  onClearSessionSpec,
}: SpecExecutionActionsProps) {
  const isPending = (op: string) => pending === `${op}:${specId}`;
  return (
    <>
      {selectedIsBound ? (
        <span className="inline-flex h-8 items-center rounded-md border border-accent-2 bg-panel px-3 text-[12px] font-semibold text-accent-2">
          Bound
        </span>
      ) : null}
      <button
        className="inline-flex h-8 items-center rounded-md border border-line bg-raised px-3 text-[12px] font-semibold text-text disabled:cursor-not-allowed disabled:opacity-50"
        disabled={selectedIsBound}
        type="button"
        onClick={onBindSessionSpec}
      >
        {selectedIsBound ? "Bound to session" : "Bind session"}
      </button>
      {hasSessionBinding ? (
        <button
          className="inline-flex h-8 items-center rounded-md border border-line bg-panel px-3 text-[12px] font-semibold text-text-2 disabled:cursor-not-allowed disabled:opacity-50"
          type="button"
          onClick={onClearSessionSpec}
        >
          Clear binding
        </button>
      ) : null}
      <ActionButton
        disabled={isPending("clarify")}
        pendingLabel={isPending("clarify") ? "Checking" : "Clarify"}
        onClick={onClarify}
      />
      <ActionButton
        disabled={isPending("plan")}
        pendingLabel={isPending("plan") ? "Planning" : "Plan"}
        onClick={onPlan}
      />
      <ActionButton
        disabled={isPending("checklist")}
        pendingLabel={isPending("checklist") ? "Building" : "Checklist"}
        onClick={onChecklist}
      />
      <PrimaryActionButton
        disabled={isPending("analyze")}
        pendingLabel={isPending("analyze") ? "Analyzing" : "Analyze"}
        onClick={onAnalyze}
      />
      <PrimaryActionButton
        disabled={isPending("implement")}
        pendingLabel={isPending("implement") ? "Starting" : "Implement"}
        onClick={onImplement}
      />
    </>
  );
}
