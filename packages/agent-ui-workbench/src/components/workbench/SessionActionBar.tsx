import { Select } from "@rightkit/react-shell/react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import { ExecutionBadge, ForkBadge, SafeModeBadge, ToolsetBadge, SpecBadge } from "./ContextBadges";
import { DispatchBadge } from "./DispatchBadge";
import type { DispatchDecision, SessionRow } from "@rightkit/agent-ui-core/lib/types";

export default function SessionActionBar({
  selected,
  dispatchDecision,
  showConductorModelSelect,
  conductorModelOptions,
  conductorModelPending,
  conductorModelValue,
  messagesLength,
  awaySummaryPending,
  sessionExportPending,
  onConductorModelChange,
  onRefreshAwaySummary,
  onExportSessionMarkdown,
  onExportSessionJson,
}: {
  selected: SessionRow;
  dispatchDecision: DispatchDecision | null;
  showConductorModelSelect: boolean;
  conductorModelOptions: Array<{ id: string; label: string }>;
  conductorModelPending: boolean;
  conductorModelValue: string;
  messagesLength: number;
  awaySummaryPending: boolean;
  sessionExportPending: boolean;
  onConductorModelChange: (modelId: string) => void;
  onRefreshAwaySummary: () => void;
  onExportSessionMarkdown: () => void;
  onExportSessionJson: () => void;
}) {
  return (
    <div className="mb-2 flex flex-wrap items-center justify-end gap-2 border-b border-line bg-bg px-1 pb-2">
      <span className="sr-only">Session actions</span>
      <div className="mr-auto flex min-w-0 flex-wrap items-center gap-1.5">
        <SafeModeBadge safeMode={selected.safeMode} />
        <ForkBadge forkedFromSessionId={selected.forkedFromSessionId} />
        <ExecutionBadge
          executionBackend={selected.executionBackend}
          executionWorkspaceId={selected.executionWorkspaceId}
        />
        <ToolsetBadge toolset={selected.toolset} />
        <SpecBadge specId={selected.specId} />
        <DispatchBadge decision={dispatchDecision} />
      </div>
      {showConductorModelSelect ? (
        <label className="inline-flex items-center gap-2 text-[11px] text-text-3">
          <span className="hidden sm:inline">Conductor</span>
          <Select
            label="Conductor model for this session"
            disabled={conductorModelPending}
            value={conductorModelValue}
            onChange={(modelId) => {
              if (modelId) onConductorModelChange(modelId);
            }}
            options={[
              { value: "", label: "Choose model", disabled: true },
              ...conductorModelOptions.map((option) => ({ value: option.id, label: option.label })),
            ]}
          />
        </label>
      ) : null}
      <button
        type="button"
        className="inline-flex h-7 items-center gap-1.5 rounded-lg border border-line bg-panel px-2.5 text-[12px] text-text-2 transition-colors hover:border-line-2 hover:text-text disabled:cursor-wait disabled:opacity-50"
        disabled={awaySummaryPending || messagesLength === 0}
        onClick={onRefreshAwaySummary}
        title="Summarize recent activity since the last return digest"
      >
        <Icon name="spark" size={13} />
        Digest
      </button>
      <button
        type="button"
        className="inline-flex h-7 items-center gap-1.5 rounded-lg border border-line bg-panel px-2.5 text-[12px] text-text-2 transition-colors hover:border-line-2 hover:text-text disabled:cursor-wait disabled:opacity-50"
        disabled={sessionExportPending || messagesLength === 0}
        onClick={onExportSessionMarkdown}
        title="Export structured session as Markdown"
      >
        <Icon name="download" size={13} />
        MD
      </button>
      <button
        type="button"
        className="inline-flex h-7 items-center gap-1.5 rounded-lg border border-line bg-panel px-2.5 text-[12px] text-text-2 transition-colors hover:border-line-2 hover:text-text disabled:cursor-wait disabled:opacity-50"
        disabled={sessionExportPending || messagesLength === 0}
        onClick={onExportSessionJson}
        title="Export structured session as JSON"
      >
        <Icon name="download" size={13} />
        JSON
      </button>
    </div>
  );
}
