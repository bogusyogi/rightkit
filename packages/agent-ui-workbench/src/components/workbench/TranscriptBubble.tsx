import { memo, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import ReasoningBlock from "./ReasoningBlock";
import RenderedMessageContent from "./RenderedMessageContent";
import TrustBadge from "./TrustBadge";
import DiffView, { looksLikeDiff } from "@rightkit/agent-ui-hooks/components/workbench/DiffView";
import ReferenceChipStrip from "./references/ReferenceChipStrip";
import type { TranscriptDensity, TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import { normalizeTranscriptDensity, visibleToolCallNames } from "@rightkit/agent-ui-core/lib/transcriptGroups";
import type { TranscriptGroup } from "@rightkit/agent-ui-core/lib/transcriptGroups";
import { performReferenceAction } from "./references/referenceActions";
import {
  parseExternalizedOutput,
  formatBytes,
  stripAgentBookkeeping,
} from "@rightkit/agent-ui-core/lib/messageNotices";

export type DensityProp = TranscriptDensity | "compact" | "comfortable";

function resolveDensity(density?: DensityProp | null): TranscriptDensity {
  return normalizeTranscriptDensity(density as never);
}

const BUBBLE_DENSITY: Record<
  TranscriptDensity,
  {
    bubblePadding: string;
    bubbleFont: string;
    bubbleLeading: string;
    gap: string;
    headerGap: string;
  }
> = {
  summary: {
    bubblePadding: "px-3 py-2",
    bubbleFont: "text-[13px]",
    bubbleLeading: "leading-[1.45]",
    gap: "gap-2",
    headerGap: "gap-1",
  },
  normal: {
    bubblePadding: "px-4 py-3",
    bubbleFont: "text-[14px]",
    bubbleLeading: "leading-[1.55]",
    gap: "gap-4",
    headerGap: "gap-1.5",
  },
  verbose: {
    bubblePadding: "px-4 py-3.5",
    bubbleFont: "text-[15px]",
    bubbleLeading: "leading-[1.65]",
    gap: "gap-4",
    headerGap: "gap-2",
  },
};

/**
 * Render a tool result. Ordinary output is a plain `<pre>`; a spilled-output
 * stub (the engine's `[Large tool output externalized]` header + ref id + byte
 * count, written for the model) collapses to a compact "N KB output" line that
 * expands to the head/tail preview, instead of dumping a 64-char hash and an
 * internal path on the user (UI-ISSUES §2).
 */
function ToolResultText({ text, isError }: { text: string; isError?: boolean }) {
  const [expanded, setExpanded] = useState(false);
  const external = parseExternalizedOutput(text);
  const preClass = `m-0 text-[12px] font-mono whitespace-pre-wrap break-words max-h-[18rem] overflow-y-auto ${
    isError ? "text-danger" : "text-text-3"
  }`;
  if (!external) {
    // Drop the agent's own bookkeeping (todo checklist, EXIT sentinels) — the
    // checklist is rendered as live state in the todo dock instead.
    const cleaned = stripAgentBookkeeping(text);
    if (!cleaned) return null;
    return <pre className={preClass}>{cleaned}</pre>;
  }
  return (
    <div className="flex flex-col gap-1.5">
      <button
        type="button"
        className="flex items-center gap-1.5 self-start border-0 bg-transparent p-0 text-[12px] text-text-3 cursor-pointer hover:text-text-2"
        onClick={() => setExpanded((v) => !v)}
      >
        <Icon name="file" size={12} />
        <span>{formatBytes(external.bytes)} output</span>
        {external.preview ? (
          <span className={`opacity-60 transition-transform ${expanded ? "rotate-180" : ""}`}>
            <Icon name="chevron" size={12} />
          </span>
        ) : null}
      </button>
      {expanded && external.preview ? <pre className={preClass}>{external.preview}</pre> : null}
    </div>
  );
}

/** Detect the `[image-block]` prefix produced by the `view_image` tool. */
function isImageBlock(summary: string): boolean {
  return summary.startsWith("[image-block]");
}

/** Parse an image-block summary and render it as an inline image. */
function ImageBlockInline({ summary }: { summary: string }) {
  const lines = summary.split("\n");
  const metadata: Record<string, string> = {};
  for (const line of lines) {
    const colonIndex = line.indexOf(":");
    if (colonIndex > 0) {
      metadata[line.slice(0, colonIndex).trim()] = line.slice(colonIndex + 1).trim();
    }
  }
  const dataUrl = metadata.data_url || metadata.data_url;
  const path = metadata.path || "image";
  const dimensions = metadata.dimensions || "";
  const size = metadata.size || "";

  if (!dataUrl) {
    // Fallback: render as plain text if data_url is missing.
    return (
      <pre className="m-0 text-[12px] text-text-2 font-mono whitespace-pre-wrap break-words">
        {summary}
      </pre>
    );
  }

  return (
    <div className="flex flex-col gap-1.5">
      <img
        src={dataUrl}
        alt={path}
        className="max-w-full max-h-[24rem] rounded-lg border border-line object-contain"
        loading="lazy"
      />
      <div className="flex items-center gap-2 text-[11px] text-text-3">
        <span className="font-mono truncate">{path}</span>
        {dimensions ? <span>{dimensions}</span> : null}
        {size ? <span>{size}</span> : null}
      </div>
    </div>
  );
}

function ProcessingTranscriptGroup({
  group,
  density,
}: {
  group: Extract<TranscriptGroup, { kind: "assistant:processing" }>;
  density?: DensityProp | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const d = resolveDensity(density);
  const dens = BUBBLE_DENSITY[d];
  const visibleNames = visibleToolCallNames(group.toolCalls, d);
  const errorCount = group.toolResults.filter((result) => result.isError).length;
  const toolCount = Math.max(group.toolCalls.length, group.toolResults.length);
  const settled = toolCount > 0 && group.toolResults.length >= group.toolCalls.length;
  const cap = d === "summary" ? 1 : d === "verbose" ? 5 : 3;
  const names = visibleNames.length
    ? ` · ${visibleNames.slice(0, cap).join(", ")}`
    : settled
      ? ""
      : " · Working";
  const overflow = visibleNames.length > cap ? ` +${visibleNames.length - cap}` : "";

  const gapClass = dens.gap === "gap-2" ? "gap-1" : dens.gap === "gap-4" ? "gap-2" : "gap-1.5";
  const textSize =
    d === "summary" ? "text-[11px]" : d === "verbose" ? "text-[13px]" : "text-[12px]";

  return (
    <div className="self-start w-full max-w-[var(--cr-measure)]" data-density={d}>
      <button
        type="button"
        className={`group/run flex items-center ${dens.headerGap} text-left cursor-pointer border-0 bg-transparent p-0 text-text-3 hover:text-text-2`}
        onClick={() => setExpanded((value) => !value)}
      >
        <Icon name="terminal" size={12} />
        <span className={`${textSize} font-medium`}>
          {settled ? "Ran" : "Processing"} · {toolCount} step
          {toolCount === 1 ? "" : "s"}
          {names}
          {overflow}
        </span>
        {errorCount > 0 ? (
          <span className="text-[11px] font-semibold text-danger">
            · {errorCount} error{errorCount === 1 ? "" : "s"}
          </span>
        ) : null}
        <div
          className={`opacity-60 transition-transform duration-150 ${expanded ? "rotate-180" : ""}`}
        >
          <Icon name="chevron" size={12} />
        </div>
      </button>
      {expanded ? (
        <div className={`mt-1.5 ml-[5px] flex flex-col ${gapClass} border-l border-line pl-3`}>
          {group.assistant?.content.trim() ? (
            <div className={`${textSize} text-text-2`}>
              <RenderedMessageContent content={group.assistant.content} />
            </div>
          ) : null}
          {group.toolCalls.map((call, index) => (
            <div
              key={call.id || `${call.name}-${index}`}
              className="flex items-center gap-2 text-text-3"
            >
              <span className="w-1 h-1 rounded-full bg-text-3 flex-shrink-0" />
              <span className={`${textSize} font-mono truncate`}>
                {visibleToolCallNames([call], d)[0] ?? "internal step"}
              </span>
            </div>
          ))}
          {group.toolResults.map((result) => (
            <div key={result.id} className="flex flex-col">
              {isImageBlock(result.summary) ? (
                <ImageBlockInline summary={result.summary} />
              ) : looksLikeDiff(result.summary) ? (
                <DiffView text={result.summary} />
              ) : (
                <ToolResultText text={result.summary} isError={result.isError} />
              )}
            </div>
          ))}
        </div>
      ) : null}
    </div>
  );
}

function ClarificationTranscriptGroup({
  group,
  density,
}: {
  group: Extract<TranscriptGroup, { kind: "assistant:clarification" }>;
  density?: DensityProp | null;
}) {
  const d = resolveDensity(density);
  const dens = BUBBLE_DENSITY[d];
  const pad = d === "summary" ? "p-2" : d === "verbose" ? "p-4" : "p-3";
  const font = d === "summary" ? "text-[12px]" : d === "verbose" ? "text-[14px]" : "text-[13px]";
  return (
    <article
      className={`self-start w-full max-w-[var(--cr-measure)] rounded-xl rounded-bl-sm border border-accent-2 bg-[color-mix(in_srgb,var(--color-accent-2)_5%,transparent)] ${pad}`}
      data-density={d}
    >
      <div className="flex items-center gap-2 mb-2">
        <span className="w-2 h-2 rounded-full bg-accent-2" />
        <span className="text-[11px] font-semibold text-accent-2 uppercase tracking-wide">
          Clarification
        </span>
      </div>
      <div className={`flex flex-col ${dens.gap}`}>
        {group.questions.map((question, index) => (
          <p
            key={`${group.questionId}-${question.header || index}`}
            className={`m-0 ${font} text-text leading-relaxed`}
          >
            {question.question}
          </p>
        ))}
      </div>
    </article>
  );
}

export function TranscriptBubbleInner({
  message,
  onEdit,
  density,
}: {
  message: TranscriptMessage;
  onEdit?: () => void;
  density?: DensityProp | null;
}) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const [copyFailed, setCopyFailed] = useState(false);
  const d = resolveDensity(density);
  const dens = BUBBLE_DENSITY[d];

  function copyToClipboard() {
    const content = "content" in message ? (message.content as string) : "";
    navigator.clipboard
      .writeText(content ?? "")
      .then(() => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => {
        setCopyFailed(true);
        setTimeout(() => setCopyFailed(false), 2000);
      });
  }

  if (message.$kind === "system") return null;

  if (message.$kind === "tool") {
    // Image blocks render inline by default (no expand needed).
    if (isImageBlock(message.summary)) {
      return (
        <article
          className="self-start w-full max-w-[var(--cr-measure)] rounded-xl border overflow-hidden border-line"
          data-density={d}
        >
          <div className={`${dens.bubblePadding} bg-panel`}>
            <ImageBlockInline summary={message.summary} />
          </div>
        </article>
      );
    }
    const toolFont =
      d === "summary" ? "text-[11px]" : d === "verbose" ? "text-[13px]" : "text-[12px]";
    const toolGap = d === "summary" ? "gap-1" : d === "verbose" ? "gap-2" : "gap-1.5";
    return (
      <div className="self-start w-full max-w-[var(--cr-measure)]" data-density={d}>
        <button
          type="button"
          className={`flex items-center ${toolGap} text-left cursor-pointer border-0 bg-transparent p-0 ${message.isError ? "text-danger" : "text-text-3 hover:text-text-2"}`}
          onClick={() => setExpanded((v) => !v)}
        >
          <Icon name="terminal" size={12} />
          <span className={`${toolFont} font-mono truncate`}>{message.summary}</span>
          <TrustBadge trust={message.trust} />
          <div
            className={`opacity-60 transition-transform duration-150 ${expanded ? "rotate-180" : ""}`}
          >
            <Icon name="chevron" size={12} />
          </div>
        </button>
        {expanded ? (
          <div className="mt-1.5 ml-[5px] border-l border-line pl-3">
            <ToolResultText text={message.summary} isError={message.isError} />
          </div>
        ) : null}
      </div>
    );
  }

  if (message.$kind === "compact") {
    return (
      <div className="flex items-center gap-3 my-2 text-text-3">
        <span className="flex-1 border-t border-line" />
        <span className="text-[11px] font-medium shrink-0" title={message.summary || undefined}>
          Auto-compacted earlier context
        </span>
        <span className="flex-1 border-t border-line" />
      </div>
    );
  }

  if (message.$kind === "user") {
    const attachments = message.attachments ?? [];
    return (
      <div
        className="cr-msg-in group flex flex-col items-end self-end max-w-[var(--cr-measure)]"
        data-density={d}
      >
        <article
          className={`flex flex-col rounded-lg border border-transparent bg-panel-2 ${dens.bubblePadding}`}
        >
          <div
            className={`max-w-[var(--cr-measure)] ${dens.bubbleFont} ${dens.bubbleLeading} text-text-2`}
          >
            <RenderedMessageContent content={message.content || "(empty message)"} />
          </div>
          {attachments.length > 0 ? (
            <div className="mt-2">
              <ReferenceChipStrip
                references={attachments}
                compact={false}
                onAction={(id, action) => {
                  const reference = attachments.find((item) => item.id === id);
                  if (reference) void performReferenceAction(reference, action);
                }}
              />
            </div>
          ) : null}
        </article>
        <div className={MESSAGE_ACTION_ROW}>
          <button
            type="button"
            className={MESSAGE_ACTION_BUTTON}
            onClick={copyToClipboard}
            title={copyFailed ? "Copy failed" : copied ? "Copied" : "Copy message"}
            aria-label={copyFailed ? "Copy failed" : copied ? "Copied message" : "Copy message"}
          >
            <Icon name={copyFailed ? "flag" : copied ? "check" : "copy"} size={13} />
          </button>
          <span className="sr-only" role="status" aria-live="polite">
            {copyFailed ? "Copy failed" : copied ? "Copied message" : ""}
          </span>
          {onEdit ? (
            <button
              type="button"
              className={MESSAGE_ACTION_BUTTON}
              onClick={onEdit}
              title="Rollback to this message"
              aria-label="Rollback to this message"
            >
              <Icon name="edit" size={13} />
            </button>
          ) : null}
        </div>
      </div>
    );
  }

  return (
    <div className="cr-msg-in group flex flex-col w-full self-start" data-density={d}>
      <article className={`flex flex-col ${dens.gap} px-0 py-1 text-text-2`}>
        <ReasoningBlock content={message.reasoningContent ?? ""} />
        <div className={`max-w-[var(--cr-measure)] ${dens.bubbleFont} ${dens.bubbleLeading}`}>
          <RenderedMessageContent content={message.content || "(empty message)"} />
        </div>
      </article>
      <div className={MESSAGE_ACTION_ROW}>
        <button
          type="button"
          className={MESSAGE_ACTION_BUTTON}
          onClick={copyToClipboard}
          title={copyFailed ? "Copy failed" : copied ? "Copied" : "Copy message"}
          aria-label={copyFailed ? "Copy failed" : copied ? "Copied message" : "Copy message"}
        >
          <Icon name={copyFailed ? "flag" : copied ? "check" : "copy"} size={13} />
        </button>
        <span className="sr-only" role="status" aria-live="polite">
          {copyFailed ? "Copy failed" : copied ? "Copied message" : ""}
        </span>
      </div>
    </div>
  );
}

// Actions live in a row *under* the message and stay hidden until the message
// is hovered or a control inside it takes focus (keyboard access). This is the
// Claude/Codex placement — Adrian asked for the side-mounted buttons to move
// here — and it keeps the transcript quiet at rest.
const MESSAGE_ACTION_ROW =
  "flex items-center gap-0.5 mt-1 opacity-100 transition-opacity [@media(hover:hover)]:opacity-0 group-hover:opacity-100 group-focus-within:opacity-100";
const MESSAGE_ACTION_BUTTON =
  "grid place-items-center w-7 h-7 rounded-md text-text-3 cursor-pointer bg-transparent border-0 hover:text-text hover:bg-panel-2 focus-visible:bg-panel-2 focus-visible:text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-focus transition-colors";

export function TranscriptGroupViewInner({
  group,
  onEdit,
  density,
}: {
  group: TranscriptGroup;
  onEdit?: () => void;
  density?: DensityProp | null;
}) {
  switch (group.kind) {
    case "human":
    case "assistant":
    case "compact":
      return <TranscriptBubble message={group.message} onEdit={onEdit} density={density} />;
    case "assistant:processing":
      return <ProcessingTranscriptGroup group={group} density={density} />;
    case "assistant:clarification":
      return <ClarificationTranscriptGroup group={group} density={density} />;
  }
}

// Lane C-2 memo wrap: identity-keyed equality (message id + streaming flag).
// Re-renders only when the same message object's identity changes (new token
// arrives in the existing streaming row) or when an `onEdit` callback swaps.
// The default React.memo shallow compare is enough because the parent passes
// stable callbacks per group.
const TranscriptBubbleMemo = memo(TranscriptBubbleInner, (prev, next) => {
  if (prev.message === next.message && prev.density === next.density) return true;
  if (prev.message.id !== next.message.id) return false;
  if (prev.message.$kind !== next.message.$kind) return false;
  if (prev.density !== next.density) return false;
  // For text-bearing messages, content updates during streaming must re-render.
  if (
    (prev.message.$kind === "assistant" || prev.message.$kind === "user") &&
    (next.message.$kind === "assistant" || next.message.$kind === "user")
  ) {
    if (prev.message.content !== next.message.content) return false;
    if (
      prev.message.$kind === "user" &&
      next.message.$kind === "user" &&
      prev.message.attachments !== next.message.attachments
    ) {
      return false;
    }
    if (
      prev.message.$kind === "assistant" &&
      next.message.$kind === "assistant" &&
      prev.message.reasoningContent !== next.message.reasoningContent
    ) {
      return false;
    }
  }
  return prev.onEdit === next.onEdit;
});
const TranscriptGroupViewMemo = memo(
  TranscriptGroupViewInner,
  (prev, next) =>
    prev.group === next.group && prev.onEdit === next.onEdit && prev.density === next.density,
);

export const TranscriptBubble = TranscriptBubbleMemo;
export default TranscriptGroupViewMemo;
