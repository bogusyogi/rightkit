import { useMemo, useState, type ReactNode } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import DiffView from "@rightkit/agent-ui-hooks/components/workbench/DiffView";
import EditedFilesSummary, { type EditedFileEntry } from "./EditedFilesSummary";
import { resolveToolApproval } from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, ProtocolEvent, TranscriptDensity } from "@rightkit/agent-ui-core/lib/types";
import { normalizeTranscriptDensity } from "@rightkit/agent-ui-core/lib/transcriptGroups";

const FILE_EDIT_TOOL_NAMES = new Set(["edit_file", "multi_edit", "write_file", "apply_patch"]);
const DIFF_SHAPE_PATTERN = /^@@|\n@@|\n[+-]|\n```diff/m;

function looksLikeDiff(text: string): boolean {
  return DIFF_SHAPE_PATTERN.test(text);
}

function parseEditedFilePath(argumentsJson: string): string | null {
  try {
    const parsed = JSON.parse(argumentsJson) as Record<string, unknown>;
    const candidate = parsed.filePath ?? parsed.path ?? parsed.file_path ?? parsed.notebook_path;
    return typeof candidate === "string" ? candidate : null;
  } catch {
    return null;
  }
}

function countDiffStats(diffText: string): { additions: number; deletions: number } {
  let additions = 0;
  let deletions = 0;
  for (const line of diffText.split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---")) continue;
    if (line.startsWith("+")) additions += 1;
    else if (line.startsWith("-")) deletions += 1;
  }
  return { additions, deletions };
}

const STDOUT_TRUNCATE_LIMIT = 2000;
const STDOUT_PREVIEW_LIMIT = 320;

function parseArgs(argumentsJson: string): Array<{ key: string; value: string }> {
  const trimmed = argumentsJson.trim();
  if (!trimmed) return [];
  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch {
    return [{ key: "raw", value: trimmed }];
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) {
    return [{ key: "value", value: typeof parsed === "string" ? parsed : JSON.stringify(parsed) }];
  }
  const entries: Array<{ key: string; value: string }> = [];
  for (const [key, raw] of Object.entries(parsed as Record<string, unknown>)) {
    if (raw == null) {
      entries.push({ key, value: String(raw) });
      continue;
    }
    if (typeof raw === "string") {
      entries.push({ key, value: raw });
    } else if (typeof raw === "number" || typeof raw === "boolean") {
      entries.push({ key, value: String(raw) });
    } else {
      entries.push({ key, value: JSON.stringify(raw) });
    }
  }
  return entries;
}

function extractStdout(resultText: string): string {
  const fence = /```(?:[a-zA-Z0-9_-]*)\n([\s\S]*?)\n```/.exec(resultText);
  if (fence?.[1]) return fence[1];
  return resultText;
}

function extractExitCode(resultText: string): number | null {
  const explicit = /(?:exit(?:ed)?[_ -]?code|exit)[:=]\s*(-?\d+)/i.exec(resultText);
  if (explicit) return Number(explicit[1]);
  return null;
}

function fmtCost(costUsd: number): string {
  if (costUsd < 0.0001) return `$${costUsd.toFixed(6)}`;
  if (costUsd < 1) return `$${costUsd.toFixed(4)}`;
  return `$${costUsd.toFixed(2)}`;
}

export type StreamDensity = TranscriptDensity | "compact" | "comfortable";
function resolveStreamDensity(density?: StreamDensity | null): TranscriptDensity {
  return normalizeTranscriptDensity(density as never);
}
const STREAM_DENSITY: Record<
  TranscriptDensity,
  { gap: string; cardPadding: string; headerPadding: string; font: string; monoFont: string }
> = {
  summary: {
    gap: "gap-1",
    cardPadding: "p-2",
    headerPadding: "px-2.5 py-1.5",
    font: "text-[11px]",
    monoFont: "text-[11px]",
  },
  normal: {
    gap: "gap-2",
    cardPadding: "p-3",
    headerPadding: "px-3 py-2",
    font: "text-[12px]",
    monoFont: "text-[12px]",
  },
  verbose: {
    gap: "gap-3",
    cardPadding: "p-3.5",
    headerPadding: "px-3 py-2.5",
    font: "text-[13px]",
    monoFont: "text-[13px]",
  },
};

/** Tool-call card — fence-style header + collapsible <details> with stdout. */
export function ToolCallCard({
  toolName,
  args,
  summary,
  open = true,
  density,
}: {
  toolName: string;
  args?: string;
  summary?: string;
  open?: boolean;
  density?: StreamDensity | null;
}) {
  const parsedArgs = useMemo(() => parseArgs(args ?? ""), [args]);
  const stdout = useMemo(() => extractStdout(summary ?? ""), [summary]);
  const exitCode = useMemo(() => extractExitCode(summary ?? ""), [summary]);
  const stdoutTruncated = stdout.length > STDOUT_TRUNCATE_LIMIT;
  const stdoutShown = stdoutTruncated
    ? `${stdout.slice(0, STDOUT_TRUNCATE_LIMIT)}\n… (truncated)`
    : stdout;
  const [expanded, setExpanded] = useState(open);
  const preview = stdout.slice(0, STDOUT_PREVIEW_LIMIT);
  const d = resolveStreamDensity(density);
  const dens = STREAM_DENSITY[d];

  return (
    <article
      className="self-start flex flex-col max-w-[720px] rounded-xl border border-line bg-panel overflow-hidden"
      data-density={d}
      aria-label={`Tool ${toolName}`}
    >
      <button
        type="button"
        aria-expanded={expanded}
        aria-label={`${expanded ? "Collapse" : "Expand"} tool ${toolName}`}
        className={`flex items-center gap-2 w-full ${dens.headerPadding} text-left cursor-pointer border-0 bg-panel-2 hover:bg-panel text-text-2`}
        onClick={() => setExpanded((v) => !v)}
      >
        <Icon name="terminal" size={16} />
        <span className={`font-mono ${dens.monoFont} font-semibold text-text`}>{toolName}</span>
        {parsedArgs.length > 0 ? (
          <span
            className={`flex-1 min-w-0 truncate font-mono ${d === "summary" ? "text-[10px]" : "text-[11px]"} text-text-3`}
          >
            {parsedArgs
              .slice(0, d === "summary" ? 1 : d === "verbose" ? 5 : 3)
              .map((p) => `${p.key}=${p.value}`)
              .join("  ")}
            {parsedArgs.length > (d === "summary" ? 1 : d === "verbose" ? 5 : 3)
              ? ` +${parsedArgs.length - (d === "summary" ? 1 : d === "verbose" ? 5 : 3)}`
              : ""}
          </span>
        ) : (
          <span className="flex-1" />
        )}
        {exitCode !== null ? (
          <span
            className={`inline-flex h-5 items-center rounded-md border px-1.5 font-mono text-[10px] font-semibold tabular-nums ${
              exitCode === 0
                ? "border-accent-2/40 bg-accent-2/10 text-accent-2"
                : "border-danger/40 bg-danger/10 text-danger"
            }`}
          >
            Exit: {exitCode}
          </span>
        ) : null}
        <span className={`transition-transform duration-150 ${expanded ? "rotate-180" : ""}`}>
          <Icon name="chevron" size={12} />
        </span>
      </button>
      {expanded ? (
        <details open className="border-t border-line bg-bg">
          <summary className="cursor-pointer list-none px-3 py-1.5 text-[11px] uppercase tracking-wider text-text-3 font-semibold hover:text-text-2">
            Result · {stdout.length} chars
          </summary>
          <pre className="m-0 max-h-[18rem] overflow-auto px-3 pb-3 text-[12px] leading-[1.55] font-mono text-text-2 whitespace-pre-wrap break-words">
            {stdoutShown || "(no output)"}
          </pre>
        </details>
      ) : (
        <div className="border-t border-line bg-bg px-3 py-1.5 text-[11px] text-text-3 font-mono truncate">
          {preview}
          {stdout.length > STDOUT_PREVIEW_LIMIT ? "…" : ""}
        </div>
      )}
    </article>
  );
}

/** Inline approval card with Approve/Reject buttons wired to the daemon IPC. */
export function ApprovalCard({
  toolName,
  riskLabel,
  proposedChange,
  config,
  sessionId,
  toolCallId,
  onApprove,
  onDeny,
  onStatus,
  density,
}: {
  toolName: string;
  riskLabel: string;
  proposedChange?: string;
  config: DaemonConfig | null;
  sessionId: string | null;
  toolCallId: string;
  onApprove?: () => void;
  onDeny?: () => void;
  onStatus?: (status: string) => void;
  density?: StreamDensity | null;
}) {
  const [pending, setPending] = useState<"approve" | "deny" | null>(null);

  const decide = async (approved: boolean) => {
    if (!config || !sessionId) return;
    setPending(approved ? "approve" : "deny");
    try {
      await resolveToolApproval(
        config,
        sessionId,
        toolCallId,
        approved,
        approved ? "approved in stream" : "denied in stream",
      );
      onStatus?.(approved ? "Approved" : "Denied");
      if (approved) onApprove?.();
      else onDeny?.();
    } catch (err) {
      onStatus?.(`Approval failed: ${err instanceof Error ? err.message : String(err)}`);
    } finally {
      setPending(null);
    }
  };

  const isDiff = proposedChange ? /^@@|\n@@|\n[+-]|\n```diff/m.test(proposedChange) : false;
  const d = resolveStreamDensity(density);
  const dens = STREAM_DENSITY[d];

  return (
    <article
      className={`self-start flex flex-col ${dens.gap} max-w-[720px] ${dens.cardPadding} rounded-xl border border-warning/45 bg-[color-mix(in_srgb,var(--color-warning)_7%,transparent)]`}
      data-density={d}
    >
      <div className="flex items-center gap-2">
        <Icon name="lock" size={16} />
        <span className="text-[10px] uppercase tracking-widest font-semibold text-warning">
          Permission needed
        </span>
      </div>
      <div className="flex flex-wrap items-baseline gap-2">
        <span className="font-mono text-[13px] font-semibold text-text">Tool: {toolName}</span>
        {riskLabel ? (
          <span className="font-mono text-[11px] text-text-3">· {riskLabel}</span>
        ) : null}
      </div>
      {proposedChange ? (
        isDiff ? (
          <DiffView text={proposedChange} />
        ) : (
          <pre className="m-0 max-h-[12rem] overflow-auto px-2 py-1.5 rounded-lg border border-line bg-bg font-mono text-[11px] leading-[1.55] text-text-2 whitespace-pre-wrap break-words">
            {proposedChange}
          </pre>
        )
      ) : null}
      <div className="flex items-center gap-2">
        <button
          type="button"
          className="inline-flex items-center gap-1 h-7 px-3 rounded-md border-0 bg-accent text-on-accent text-[12px] font-semibold cursor-pointer hover:opacity-90 disabled:cursor-not-allowed disabled:opacity-50"
          onClick={() => void decide(true)}
          disabled={pending !== null}
        >
          <Icon name="check" size={12} />
          Approve
        </button>
        <button
          type="button"
          className="inline-flex items-center gap-1 h-7 px-3 rounded-md border border-line bg-transparent text-text-2 text-[12px] font-semibold cursor-pointer hover:bg-panel-2 hover:text-text disabled:cursor-not-allowed disabled:opacity-50"
          onClick={() => void decide(false)}
          disabled={pending !== null}
        >
          <Icon name="close" size={12} />
          Reject
        </button>
        <span className="ml-auto text-[10px] uppercase tracking-wider text-text-3">
          y = approve · n = reject
        </span>
      </div>
    </article>
  );
}

/** File-change card with diff body and +/- chip. */
export function DiffCard({
  filePath,
  additions,
  deletions,
  diff,
  density,
}: {
  filePath: string;
  additions?: number;
  deletions?: number;
  diff: string;
  density?: StreamDensity | null;
}) {
  const [expanded, setExpanded] = useState(true);
  const trimmedPath = filePath.replace(/^(a|b)\//, "");
  const d = resolveStreamDensity(density);
  const dens = STREAM_DENSITY[d];

  return (
    <article
      className="self-start flex flex-col max-w-[720px] rounded-xl border border-line bg-panel overflow-hidden"
      data-density={d}
      aria-label={`Diff ${trimmedPath}`}
    >
      <button
        type="button"
        aria-expanded={expanded}
        aria-label={`${expanded ? "Collapse" : "Expand"} diff ${trimmedPath}`}
        className={`flex items-center gap-2 w-full ${dens.headerPadding} text-left cursor-pointer border-0 bg-panel-2 hover:bg-panel text-text-2`}
        onClick={() => setExpanded((v) => !v)}
      >
        <Icon name="diff" size={16} />
        <span className={`font-mono ${dens.monoFont} font-semibold text-text truncate`}>
          {trimmedPath}
        </span>
        <span className="ml-auto flex items-center gap-1.5 font-mono text-[10px] font-semibold tabular-nums">
          {typeof additions === "number" ? (
            <span className="inline-flex h-5 items-center rounded-md border border-accent-2/40 bg-accent-2/10 px-1.5 text-accent-2">
              +{additions}
            </span>
          ) : null}
          {typeof deletions === "number" ? (
            <span className="inline-flex h-5 items-center rounded-md border border-danger/40 bg-danger/10 px-1.5 text-danger">
              -{deletions}
            </span>
          ) : null}
        </span>
        <span className={`transition-transform duration-150 ${expanded ? "rotate-180" : ""}`}>
          <Icon name="chevron" size={12} />
        </span>
      </button>
      {expanded ? (
        <div className="border-t border-line bg-bg">
          <DiffView text={diff} />
        </div>
      ) : null}
    </article>
  );
}

/** Small inline routing chip: → model · est. $cost · reason: tier. */
export function RoutingChip({
  modelId,
  cost,
  tier,
  hoverDetail,
  onRetryCapable,
  density,
}: {
  modelId: string;
  cost?: number;
  tier?: string;
  /** Hover tooltip content (tier + price-snapshot hash for a routing decision). */
  hoverDetail?: string;
  /** "Retry with capable model" affordance (R1a-L4). Omit to render no button. */
  onRetryCapable?: () => void;
  density?: StreamDensity | null;
}) {
  const d = resolveStreamDensity(density);
  const _dens = STREAM_DENSITY[d];
  const font = d === "summary" ? "text-[10px]" : d === "verbose" ? "text-[12px]" : "text-[11px]";
  const pad = d === "summary" ? "px-1.5 py-0.5" : d === "verbose" ? "px-2.5 py-1" : "px-2 py-0.5";
  return (
    <span
      className={`inline-flex items-center gap-1.5 ${pad} rounded-md border border-line bg-panel-2 font-mono ${font} text-text-3 tabular-nums`}
      title={hoverDetail}
      data-density={d}
    >
      <span aria-hidden="true" className="text-text-2">
        →
      </span>
      <span className="text-text-2">{modelId}</span>
      {typeof cost === "number" ? <span>· est. {fmtCost(cost)}</span> : null}
      {tier ? <span>· reason: {tier}</span> : null}
      {onRetryCapable ? (
        <button
          type="button"
          onClick={onRetryCapable}
          className="ml-1 rounded border border-line px-1 text-[10px] text-text-3 cursor-pointer hover:border-line-2 hover:text-text"
        >
          retry with capable model
        </button>
      ) : null}
    </span>
  );
}

/** Empty-state when the stream has no events. */
export function TranscriptEmptyState() {
  return (
    <div className="flex min-h-28 items-center justify-center text-[14px] leading-[1.55] text-text-3">
      No messages yet — describe a task to start.
    </div>
  );
}

interface TranscriptStreamProps {
  events: ProtocolEvent[];
  config?: DaemonConfig | null;
  sessionId?: string | null;
  workspaceRoot?: string | null;
  density?: StreamDensity | null;
  onOpenReview?: () => void;
  onApprove?: () => void;
  onDeny?: () => void;
  onStatus?: (status: string) => void;
  /**
   * F1: visible failure propagation. When the SSE catch-up or the
   * transcript load reports a durability symptom (missed
   * append_event_at_seq, stalled catch-up, daemon restart race),
   * render the error visibly above the transcript. The hook keeps
   * `lastError` until the next successful reconnect, so the banner
   * survives transient blips.
   */
  lastError?: string | null;
  /**
   * "Retry with capable model" affordance for a ModelRouteDecision chip
   * (R1a-L4). No sub-agent task-retry engine exists yet, so this deep-links
   * to the roster/agent rail with a note rather than re-running the task —
   * documented deviation, see the lane report. Omit to hide the button.
   */
  onRetryRouteDecision?: (agentRole: string) => void;
  /** Opens the Review pane for a turn's edited-file set. Falls back to
   * `onOpenReview` (the general Review pane opener already wired by the
   * caller) when the summary row's Review action fires. */
  onReviewEditedFiles?: (files: EditedFileEntry[]) => void;
  /**
   * Reverts a turn's edited-file set. No caller currently supplies this —
   * `WorkbenchContent.tsx` (outside this component's scope) needs to wire
   * it, e.g. via `undoRunChange` in `review/runChangesClient.ts` for each
   * file path. The Undo action renders disabled until a handler is passed.
   */
  onUndoEditedFiles?: (files: EditedFileEntry[]) => void;
}

/** Stream mapper: ProtocolEvent[] → sequence of inline cards. */
export default function TranscriptStream({
  events,
  config,
  sessionId,
  density,
  onApprove,
  onDeny,
  onStatus,
  onOpenReview,
  onRetryRouteDecision,
  onReviewEditedFiles,
  onUndoEditedFiles,
  lastError,
}: TranscriptStreamProps) {
  const d = resolveStreamDensity(density);
  const dens = STREAM_DENSITY[d];
  const sorted = useMemo(() => [...events].sort((a, b) => a.seq - b.seq), [events]);
  if (sorted.length === 0) {
    return (
      <>
        <DurabilityErrorBanner lastError={lastError} />
        <TranscriptEmptyState />
      </>
    );
  }

  const handleReviewEditedFiles =
    onReviewEditedFiles ?? (onOpenReview ? () => onOpenReview() : undefined);

  // Pending edited-file entries for the turn currently being built, keyed
  // by the toolCallId that produced each diff so a matching toolResultAdded
  // can complete the entry. Flushed into one EditedFilesSummary row per
  // assistant turn (on assistantMessageCompleted) or at stream end.
  const pendingCallPaths = new Map<string, string>();
  let pendingFiles: EditedFileEntry[] = [];
  const nodes: ReactNode[] = [];

  const flushEditedFiles = (flushKey: string) => {
    if (pendingFiles.length === 0) return;
    const files = pendingFiles;
    pendingFiles = [];
    nodes.push(
      <EditedFilesSummary
        key={`edited-files-${flushKey}`}
        files={files}
        onReview={handleReviewEditedFiles}
        onUndo={onUndoEditedFiles}
      >
        {(() => {
          // The same file can be edited more than once in a turn; key by
          // occurrence per path instead of array position.
          const occurrences = new Map<string, number>();
          return files.map((file) => {
            const occurrence = occurrences.get(file.filePath) ?? 0;
            occurrences.set(file.filePath, occurrence + 1);
            return (
              <DiffCard
                key={`${file.filePath}#${occurrence}`}
                filePath={file.filePath}
                additions={file.additions}
                deletions={file.deletions}
                diff={file.diff}
                density={d}
              />
            );
          });
        })()}
      </EditedFilesSummary>,
    );
  };

  for (const event of sorted) {
    const key = `${event.$kind}-${event.seq}`;
    switch (event.$kind) {
      case "runFailed":
        nodes.push(
          <article
            className="rounded-lg border border-danger/30 bg-danger/5 px-3 py-2 text-[12px]"
            key={key}
            role="alert"
          >
            <strong className="block font-semibold text-danger">{event.error}</strong>
            {event.hint ? <p className="mt-1 text-text-2">{event.hint}</p> : null}
          </article>,
        );
        break;
      case "assistantMessageCompleted":
        flushEditedFiles(key);
        break;
      case "toolCallStarted": {
        if (FILE_EDIT_TOOL_NAMES.has(event.call.name)) {
          const filePath = parseEditedFilePath(event.call.argumentsJson);
          if (filePath) pendingCallPaths.set(event.call.id, filePath);
        }
        nodes.push(
          <ToolCallCard
            key={key}
            toolName={event.call.name}
            args={event.call.argumentsJson}
            density={d}
          />,
        );
        break;
      }
      case "toolResultAdded": {
        const toolCallId = event.result.toolCallId;
        const filePath = toolCallId ? pendingCallPaths.get(toolCallId) : undefined;
        if (filePath && looksLikeDiff(event.result.summary)) {
          pendingCallPaths.delete(toolCallId as string);
          const diff = extractStdout(event.result.summary);
          const stats = countDiffStats(diff);
          pendingFiles.push({
            filePath,
            additions: stats.additions,
            deletions: stats.deletions,
            diff,
          });
          break;
        }
        nodes.push(
          <ToolCallCard
            key={key}
            toolName={event.result.toolCallId ? "tool" : "result"}
            summary={event.result.summary}
            density={d}
          />,
        );
        break;
      }
      default:
        nodes.push(
          renderRoutingEvent(event, key, {
            config,
            sessionId,
            onApprove,
            onDeny,
            onStatus,
            onRetryRouteDecision,
            density: d,
          }),
        );
    }
  }
  flushEditedFiles("stream-end");

  return (
    <div className={`flex flex-col ${dens.gap}`} data-density={d}>
      <DurabilityErrorBanner lastError={lastError} />
      {nodes}
    </div>
  );
}

function renderRoutingEvent(
  event: ProtocolEvent,
  key: string,
  ctx: {
    config?: DaemonConfig | null;
    sessionId?: string | null;
    onApprove?: () => void;
    onDeny?: () => void;
    onStatus?: (status: string) => void;
    onRetryRouteDecision?: (agentRole: string) => void;
    density?: TranscriptDensity | null;
  },
): ReactNode {
  const { config, sessionId, onApprove, onDeny, onStatus, onRetryRouteDecision, density } = ctx;
  switch (event.$kind) {
    case "toolApprovalRequested":
      return (
        <ApprovalCard
          key={key}
          toolName={event.call.name}
          riskLabel={event.riskLabel}
          proposedChange={event.proposedChange}
          config={config ?? null}
          sessionId={sessionId ?? null}
          toolCallId={event.call.id}
          onApprove={onApprove}
          onDeny={onDeny}
          onStatus={onStatus}
          density={density}
        />
      );
    case "toolApprovalResolved":
      return (
        <RoutingChip
          key={key}
          modelId={event.approved ? "approved" : "rejected"}
          tier={event.toolCallId}
          density={density}
        />
      );
    case "providerRouteUpdated":
      return (
        <RoutingChip
          key={key}
          modelId={`${event.provider}/${event.model}`}
          tier={event.state}
          density={density}
        />
      );
    case "dispatchDecided":
      return (
        <RoutingChip key={key} modelId={event.strategy} tier={event.rationale} density={density} />
      );
    case "usageUpdated":
      return (
        <RoutingChip
          key={key}
          modelId="usage"
          cost={event.costUsd}
          tier={`${event.promptTokens}+${event.completionTokens}t`}
          density={density}
        />
      );
    case "modelRouteDecision":
      return (
        <RoutingChip
          key={key}
          modelId={`${event.agentRole} → ${event.modelId}`}
          cost={event.estCostUsd}
          tier={event.reason}
          hoverDetail={`tier: ${event.tier} · snapshot: ${event.priceSnapshot.hash}`}
          onRetryCapable={
            onRetryRouteDecision ? () => onRetryRouteDecision(event.agentRole) : undefined
          }
          density={density}
        />
      );
    default:
      return null;
  }
}

/**
 * F1 visible failure propagation banner. Renders above the transcript
 * (including the empty state) whenever the hook reports a durability
 * symptom — missed append_event_at_seq, stalled SSE catch-up, or a
 * daemon restart race. The banner survives transient blips because the
 * hook keeps `lastError` until the next successful reconnect.
 */
function DurabilityErrorBanner({ lastError }: { lastError?: string | null }) {
  if (!lastError) return null;
  return (
    <div
      className="rounded-md border border-danger/40 bg-danger/5 px-3 py-2 text-[12px] text-danger"
      role="alert"
      data-testid="durability-error-banner"
    >
      <span className="font-semibold">Event stream degraded:</span> <span>{lastError}</span>
    </div>
  );
}
