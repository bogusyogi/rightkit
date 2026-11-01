import type { ProtocolEvent, TranscriptMessage } from "./types";

type SubAgentSpawnedEvent = Extract<ProtocolEvent, { $kind: "subAgentSpawned" }>;

const NOTICE_ID_PREFIX = "orchestrator-subagent-";

export function orchestrationNoticeForSpawn(event: SubAgentSpawnedEvent): TranscriptMessage {
  return {
    $kind: "assistant",
    id: `${NOTICE_ID_PREFIX}${event.subSessionId}`,
    createdAt: event.createdAt,
    content: `**Orchestrator**\n\nI’m starting a ${event.role} agent to handle: ${event.taskTitle}`,
    toolCalls: [],
    pinned: false,
  };
}

export function isOrchestrationNotice(message: TranscriptMessage): boolean {
  return message.id.startsWith(NOTICE_ID_PREFIX);
}

export function insertOrchestrationNotice(
  messages: TranscriptMessage[],
  event: SubAgentSpawnedEvent,
): TranscriptMessage[] {
  const notice = orchestrationNoticeForSpawn(event);
  if (messages.some((message) => message.id === notice.id)) return messages;

  return [...messages, notice]
    .map((message, index) => ({ message, index }))
    .sort((left, right) => {
      const byTime = left.message.createdAt.localeCompare(right.message.createdAt);
      return byTime || left.index - right.index;
    })
    .map(({ message }) => message);
}

export function insertOrchestrationNoticeFromRoster(
  messages: TranscriptMessage[],
  row: { subSessionId: string; role: string },
  createdAt: string,
): TranscriptMessage[] {
  return insertOrchestrationNotice(messages, {
    $kind: "subAgentSpawned",
    sessionId: "",
    seq: 0,
    createdAt,
    subSessionId: row.subSessionId,
    role: row.role,
    taskTitle: `${row.role} task`,
  });
}

export function mergeOrchestrationNotices(
  transcript: TranscriptMessage[],
  currentMessages: TranscriptMessage[],
): TranscriptMessage[] {
  let merged = transcript;
  for (const notice of currentMessages.filter(isOrchestrationNotice)) {
    if (merged.some((message) => message.id === notice.id)) continue;
    merged = [...merged, notice];
  }

  return merged
    .map((message, index) => ({ message, index }))
    .sort((left, right) => {
      const byTime = left.message.createdAt.localeCompare(right.message.createdAt);
      return byTime || left.index - right.index;
    })
    .map(({ message }) => message);
}
