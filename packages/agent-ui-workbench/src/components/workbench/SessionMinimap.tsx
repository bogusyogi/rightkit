import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";

export type SessionMinimapKind = "user" | "assistant" | "ask" | "tool" | "compact" | string;

/** OMP's message projection, kept separate from the daemon transcript wire shape. */
export interface SessionMinimapMessage {
  kind: SessionMinimapKind;
  id?: string;
  text?: string;
  tokens?: number | null;
  tokensIn?: number | null;
  tokensOut?: number | null;
  time?: string | null;
  tool?: string | null;
  title?: string | null;
  duration?: number | null;
  status?: string | null;
  streaming?: boolean;
}

type SessionMinimapInput = SessionMinimapMessage | TranscriptMessage;

// OMP's TOOL_META colors, translated to the the host app token palette.
const TOOL_COLORS: Record<string, string> = {
  read: "var(--color-accent-2)",
  search: "var(--color-text-2)",
  edit: "var(--color-accent)",
  bash: "var(--color-warning)",
  write: "var(--color-accent)",
  todo: "var(--color-accent-2)",
  eval: "var(--color-danger)",
  task: "var(--color-accent-2)",
  debug: "var(--color-warning)",
  ask: "var(--color-text-3)",
};

export interface SessionMinimapProps {
  messages: readonly SessionMinimapInput[];
  hoveredIdx: number | null;
  onHover?: (index: number | null) => void;
  onClick?: (index: number) => void;
  className?: string;
}

/**
 * Dense OMP-style overview: one 13-column grid cell per transcript message.
 * Hover/focus is intentionally lifted to the workbench so the corresponding
 * mounted bubble can receive the same cross-highlight.
 */
export function SessionMinimap({
  messages: rawMessages,
  hoveredIdx,
  onHover,
  onClick,
  className,
}: SessionMinimapProps) {
  const messages = rawMessages.map(toMinimapMessage);
  let maxTokens = 0;
  for (const message of messages) {
    if (message.kind === "assistant" && message.tokens && message.tokens > maxTokens) {
      maxTokens = message.tokens;
    }
  }
  const logMax = Math.log10(maxTokens + 1) || 1;

  return (
    <section
      aria-label="Session minimap"
      className={`flex min-h-0 flex-col gap-1.5 ${className ?? ""}`.trim()}
    >
      <div className="flex items-center gap-1.5 text-[11px]">
        <Icon name="model" size={11} />
        <span className="font-mono text-text-3">session</span>
        <span className="ml-auto font-mono tabular-nums text-text-3">{messages.length}</span>
      </div>
      <div
        className="grid min-h-0 flex-1 gap-0.5 overflow-y-auto rounded-md border border-line bg-bg p-1.5"
        style={{ gridTemplateColumns: "repeat(13, minmax(0, 1fr))" }}
        data-testid="session-minimap-grid"
      >
        {messages.map((message, index) => {
          const opacity = messageOpacity(message, maxTokens, logMax);
          const active = hoveredIdx === index;
          const label = minimapTitle(message);
          const kindClass = safeKindClass(message.kind);
          const pulseClass = message.streaming ? "motion-safe:animate-pulse" : "";
          const activeClass = active ? "z-10 scale-125 outline-text-2" : "";
          return (
            <button
              key={message.id ?? `minimap-${index}`}
              type="button"
              className={`minimap-cell group relative aspect-square min-h-6 min-w-6 rounded-[2px] border-0 p-0 outline outline-1 outline-transparent transition-transform duration-75 hover:z-10 hover:scale-125 hover:outline-text-2 focus-visible:z-10 focus-visible:scale-125 focus-visible:outline-2 focus-visible:outline-focus ${pulseClass} ${kindClass} ${activeClass}`.trim()}
              style={{ backgroundColor: messageColor(message), opacity: active ? 1 : opacity }}
              title={label}
              aria-label={label}
              aria-current={active ? true : undefined}
              data-msg-idx={index}
              data-testid={`session-minimap-cell-${index}`}
              onMouseEnter={() => onHover?.(index)}
              onMouseLeave={() => onHover?.(null)}
              onFocus={() => onHover?.(index)}
              onBlur={() => onHover?.(null)}
              onClick={() => onClick?.(index)}
            />
          );
        })}
      </div>
    </section>
  );
}

function toMinimapMessage(message: SessionMinimapInput): SessionMinimapMessage {
  if (!("$kind" in message)) return message;
  switch (message.$kind) {
    case "user":
      return { kind: "user", id: message.id, text: message.content };
    case "assistant":
      return { kind: "assistant", id: message.id, text: message.content };
    case "tool":
      return {
        kind: "tool",
        id: message.id,
        text: message.summary,
        title: message.toolCallId,
        status: message.isError ? "error" : "ok",
      };
    case "compact":
      return { kind: "compact", id: message.id, text: message.summary };
    case "system":
      return { kind: "compact", id: message.id, text: message.content };
  }
}

function messageColor(message: SessionMinimapMessage): string {
  const status = message.status?.toLowerCase();
  if (status === "error" || status === "failed") return "var(--color-danger)";
  if (status === "running" || status === "pending") return "var(--color-accent-2)";
  if (status === "waiting" || status === "blocked") return "var(--color-warning)";
  switch (message.kind) {
    case "user":
      return "var(--color-text-3)";
    case "assistant":
      return "var(--color-accent)";
    case "ask":
      return "var(--color-warning)";
    case "tool":
      return TOOL_COLORS[message.tool ?? ""] ?? "var(--color-text-3)";
    case "compact":
      return "var(--color-text-3)";
    default:
      return "var(--color-text-3)";
  }
}

function messageOpacity(
  message: SessionMinimapMessage,
  maxTokens: number,
  logMax: number,
): number {
  if (message.kind !== "assistant" || !message.tokens || maxTokens <= 0) return 0.7;
  const scaled = Math.log10(message.tokens + 1) / logMax;
  return 0.4 + 0.6 * Math.max(0, Math.min(1, scaled));
}

function minimapTitle(message: SessionMinimapMessage): string {
  if (message.kind === "assistant") {
    const tokens = message.tokens ? `${message.tokens.toLocaleString()} tok` : "—";
    const inOut =
      message.tokensIn != null || message.tokensOut != null
        ? ` (${(message.tokensIn ?? 0).toLocaleString()} in · ${(message.tokensOut ?? 0).toLocaleString()} out)`
        : "";
    return `assistant · ${tokens}${inOut}${message.time ? ` · ${message.time}` : ""}`;
  }
  if (message.kind === "tool") {
    const duration = message.duration ? ` · ${(message.duration / 1000).toFixed(1)}s` : "";
    const status =
      message.status === "running" ? " · running" : message.status === "ok" ? "" : ` · ${message.status ?? "unknown"}`;
    return `${message.tool ?? "tool"}${message.title ? ` ${message.title}` : ""}${duration}${status}`;
  }
  if (message.kind === "user") {
    const text = message.text ?? "";
    const preview = text ? ` · ${text.slice(0, 80)}${text.length > 80 ? "…" : ""}` : "";
    return `you${preview}`;
  }
  return message.kind;
}

function safeKindClass(kind: string): string {
  return ["user", "assistant", "ask", "tool", "compact"].includes(kind) ? kind : "other";
}

export default SessionMinimap;
