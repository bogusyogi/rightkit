import type { ProtocolEvent, ToolCall, TranscriptMessage } from "./types.js";

export type AssistantTranscriptMessage = Extract<TranscriptMessage, { $kind: "assistant" }>;

export function assistantToolCalls(message: AssistantTranscriptMessage): ToolCall[] {
  return Array.isArray(message.toolCalls) ? message.toolCalls : [];
}

export function normalizeTranscriptMessage(message: TranscriptMessage): TranscriptMessage {
  if (message.$kind !== "assistant") return message;
  if (Array.isArray(message.toolCalls)) return message;
  return { ...message, toolCalls: [] };
}

export function normalizeTranscriptMessages(messages: TranscriptMessage[]): TranscriptMessage[] {
  return messages.map(normalizeTranscriptMessage);
}

export function isInternalTranscriptMessage(message: TranscriptMessage): boolean {
  return (
    message.$kind === "assistant" &&
    (message.id.startsWith("blueprint-init-") ||
      message.content.startsWith("Blueprint initialized: OKF understanding bundle") ||
      message.content.startsWith("Blueprint verified: OKF understanding bundle"))
  );
}

export function isInternalRunFailure(
  event: Extract<ProtocolEvent, { $kind: "runFailed" }>,
): boolean {
  return event.kind === "blueprint_init";
}

export function visibleTranscriptMessages(messages: TranscriptMessage[]): TranscriptMessage[] {
  return normalizeTranscriptMessages(messages).filter(
    (message) => !isInternalTranscriptMessage(message),
  );
}
