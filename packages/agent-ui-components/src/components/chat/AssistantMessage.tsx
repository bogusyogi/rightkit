import RenderedMessageContent from "@rightkit/agent-ui-workbench/components/workbench/RenderedMessageContent";
import ThinkingIndicator from "./ThinkingIndicator";

// AssistantMessage — left-aligned plain-text response on the chat canvas.
// Optionally renders a ThinkingIndicator below the assistant text when
// the parent reports `isThinking=true` plus the live elapsed/token
// counters. Inline tool cards already live inside the rendered content
// (RenderedMessageContent handles code fences and image blocks).

export interface AssistantMessageProps {
  content: string;
  isThinking?: boolean;
  elapsedSeconds?: number;
  tokenCount?: number;
  thinkingStatus?: string;
  maxWidthClass?: string;
}

export default function AssistantMessage({
  content,
  isThinking,
  elapsedSeconds = 0,
  tokenCount = 0,
  thinkingStatus,
  maxWidthClass = "max-w-[80%]",
}: AssistantMessageProps) {
  const trimmed = content.trim();
  return (
    <div
      data-testid="assistant-message"
      className={`flex flex-col gap-2 self-start ${maxWidthClass}`}
    >
      {trimmed ? (
        <article className="text-[14px] leading-relaxed text-text">
          <RenderedMessageContent content={trimmed} />
        </article>
      ) : null}
      {isThinking ? (
        <ThinkingIndicator
          elapsedSeconds={elapsedSeconds}
          tokenCount={tokenCount}
          status={thinkingStatus}
        />
      ) : null}
    </div>
  );
}
