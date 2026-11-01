import type { ToolCall, TranscriptMessage, UserQuestion } from "./types";
import type { TranscriptDensity } from "./types";

export type TranscriptGroupKind =
  | "human"
  | "assistant:processing"
  | "assistant"
  | "assistant:clarification"
  | "compact";

export type TranscriptGroup =
  | {
      kind: "human";
      id: string;
      message: Extract<TranscriptMessage, { $kind: "user" }>;
    }
  | {
      kind: "assistant";
      id: string;
      message: Extract<TranscriptMessage, { $kind: "assistant" }>;
    }
  | {
      kind: "assistant:processing";
      id: string;
      assistant?: Extract<TranscriptMessage, { $kind: "assistant" }>;
      toolCalls: ToolCall[];
      toolResults: Extract<TranscriptMessage, { $kind: "tool" }>[];
    }
  | {
      kind: "assistant:clarification";
      id: string;
      questionId: string;
      questions: UserQuestion[];
    }
  | {
      kind: "compact";
      id: string;
      message: Extract<TranscriptMessage, { $kind: "compact" }>;
    };

/** Normalize density policy: compact ↔ summary, comfortable ↔ verbose (B-5 aliases). */
export function normalizeTranscriptDensity(
  density?: TranscriptDensity | "compact" | "comfortable" | null,
): TranscriptDensity {
  if (density === "compact" || density === "summary") return "summary";
  if (density === "comfortable" || density === "verbose") return "verbose";
  return "normal";
}

export function groupTranscriptMessages(
  messages: TranscriptMessage[],
  pendingQuestion: { questionId: string; questions: UserQuestion[] } | null,
  density?: TranscriptDensity | "compact" | "comfortable" | null,
): TranscriptGroup[] {
  const effectiveDensity = normalizeTranscriptDensity(density);
  const groups: TranscriptGroup[] = [];
  let pendingProcessing: Extract<TranscriptGroup, { kind: "assistant:processing" }> | null = null;

  function flushProcessing() {
    if (!pendingProcessing) return;
    if (pendingProcessing.toolCalls.length || pendingProcessing.toolResults.length) {
      groups.push(pendingProcessing);
    } else if (pendingProcessing.assistant) {
      groups.push({
        kind: "assistant",
        id: pendingProcessing.assistant.id,
        message: pendingProcessing.assistant,
      });
    }
    pendingProcessing = null;
  }

  for (const message of messages) {
    if (message.$kind === "system") continue;
    if (message.$kind === "user") {
      if (isHiddenUserMessage(message.content)) continue;
      flushProcessing();
      groups.push({ kind: "human", id: message.id, message });
      continue;
    }

    if (message.$kind === "compact") {
      flushProcessing();
      groups.push({ kind: "compact", id: message.id, message });
      continue;
    }

    if (message.$kind === "assistant") {
      flushProcessing();
      // Conductor / sub-agent assistant messages can arrive without a toolCalls
      // field; treat a missing list as empty rather than crashing the transcript.
      const toolCalls = message.toolCalls ?? [];
      if (toolCalls.length > 0) {
        pendingProcessing = {
          kind: "assistant:processing",
          id: `processing-${message.id}`,
          assistant: message,
          toolCalls,
          toolResults: [],
        };
      } else {
        groups.push({ kind: "assistant", id: message.id, message });
      }
      continue;
    }

    if (message.$kind === "tool") {
      // Summary density collapses successful tool output — durable history is preserved
      // but the grouped view hides non-error results to reduce noise. Normal/verbose keep all.
      if (effectiveDensity === "summary" && !message.isError) {
        // Keep a processing group for the call but omit the successful result detail.
        if (!pendingProcessing) {
          pendingProcessing = {
            kind: "assistant:processing",
            id: `processing-${message.id}`,
            toolCalls: [],
            toolResults: [],
          };
        }
        continue;
      }
      if (!pendingProcessing) {
        pendingProcessing = {
          kind: "assistant:processing",
          id: `processing-${message.id}`,
          toolCalls: [],
          toolResults: [],
        };
      }
      pendingProcessing.toolResults.push(message);
    }
  }

  flushProcessing();

  if (pendingQuestion) {
    groups.push({
      kind: "assistant:clarification",
      id: `clarification-${pendingQuestion.questionId}`,
      questionId: pendingQuestion.questionId,
      questions: pendingQuestion.questions,
    });
  }

  return groups;
}

export function visibleToolCallNames(
  toolCalls: ToolCall[],
  density?: TranscriptDensity | "compact" | "comfortable" | null,
) {
  const effective = normalizeTranscriptDensity(density);
  const names = toolCalls.map((call) => call.name).filter((name) => !isHiddenInternalName(name));
  // Density caps visible names to control header density: summary shows 1, normal 3, verbose 5.
  if (effective === "summary") return names.slice(0, 1);
  if (effective === "verbose") return names.slice(0, 5);
  return names.slice(0, 3);
}

function isHiddenUserMessage(content: string) {
  const trimmed = content.trim();
  return trimmed.startsWith("<system-reminder>") || trimmed.startsWith("[view_image:");
}

function isHiddenInternalName(name: string) {
  const normalized = name.trim().toLowerCase();
  return (
    !normalized ||
    normalized.startsWith("__") ||
    normalized.startsWith("internal") ||
    normalized.startsWith("coderight_internal") ||
    normalized.startsWith("system_")
  );
}
