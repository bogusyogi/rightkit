import { useCallback, useEffect, useRef, useState } from "react";
import type { Dispatch, SetStateAction } from "react";
import { sendMessage, steerMessage } from "@rightkit/agent-ui-core/lib/daemon";
import type { Attachment } from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import type { ReferenceMeta } from "@rightkit/agent-ui-core/lib/types/references";

export interface QueuedMessage {
  id: string;
  text: string;
  attachments?: Attachment[];
  references?: ReferenceMeta[];
}

interface UseQueuedMessagesOptions {
  config: DaemonConfig | null;
  sessionId: string | null;
  isWorking: boolean;
  hasPendingApproval: boolean;
  formatError: (error: unknown, fallback: string) => string;
  onEditQueuedMessage: (message: QueuedMessage) => void;
  onMessagesChanged: Dispatch<SetStateAction<TranscriptMessage[]>>;
  onStatus: (status: string) => void;
  onWorking: (working: boolean) => void;
}

export function useQueuedMessages({
  config,
  sessionId,
  isWorking,
  hasPendingApproval,
  formatError,
  onEditQueuedMessage,
  onMessagesChanged,
  onStatus,
  onWorking,
}: UseQueuedMessagesOptions) {
  const [queuedMessages, setQueuedMessages] = useState<QueuedMessage[]>([]);
  const queuedRef = useRef<QueuedMessage[]>([]);
  queuedRef.current = queuedMessages;

  const queueMessage = useCallback(
    (text: string, attachments: Attachment[] = [], references: ReferenceMeta[] = []) => {
      setQueuedMessages((prev) => [
        ...prev,
        { id: `queued-${Date.now()}`, text, attachments, references },
      ]);
    },
    [],
  );

  const clearQueuedMessages = useCallback(() => {
    setQueuedMessages([]);
  }, []);

  const steerQueued = useCallback(
    async (id: string) => {
      const msg = queuedRef.current.find((message) => message.id === id);
      if (!msg || !config || !sessionId) return;
      if (msg.attachments?.length || msg.references?.length) {
        onStatus("Attachments and references stay queued until the current run finishes");
        return;
      }
      setQueuedMessages((prev) => prev.filter((message) => message.id !== id));
      try {
        await steerMessage(config, sessionId, msg.text);
      } catch {
        setQueuedMessages((prev) => [msg, ...prev]);
      }
    },
    [config, onStatus, sessionId],
  );

  const editQueued = useCallback(
    (id: string) => {
      const msg = queuedRef.current.find((message) => message.id === id);
      if (!msg) return;
      setQueuedMessages((prev) => prev.filter((message) => message.id !== id));
      onEditQueuedMessage(msg);
    },
    [onEditQueuedMessage],
  );

  const deleteQueued = useCallback((id: string) => {
    setQueuedMessages((prev) => prev.filter((message) => message.id !== id));
  }, []);

  useEffect(() => {
    if (isWorking || hasPendingApproval) return;
    const q = queuedRef.current;
    if (!q.length || !config || !sessionId) return;
    const [next, ...rest] = q;
    setQueuedMessages(rest);
    onMessagesChanged((rows) => [
      ...rows,
      {
        $kind: "user",
        id: `q-${next.id}`,
        createdAt: new Date().toISOString(),
        content:
          next.text ||
          `[${next.attachments?.length ?? 0} image attachment${next.attachments?.length === 1 ? "" : "s"}]`,
        pinned: false,
        attachments: next.references?.length ? next.references : undefined,
      },
    ]);
    onWorking(true);
    onStatus("Sending queued message...");
    void sendMessage(
      config,
      sessionId,
      next.text,
      undefined,
      next.attachments?.length ? next.attachments : undefined,
      undefined,
      next.references?.length ? next.references : undefined,
    ).catch((err) => {
      setQueuedMessages((prev) => [next, ...prev]);
      onMessagesChanged((rows) => rows.filter((row) => row.id !== `q-${next.id}`));
      onWorking(false);
      onStatus(formatError(err, "Send failed"));
    });
  }, [
    config,
    formatError,
    hasPendingApproval,
    isWorking,
    onMessagesChanged,
    onStatus,
    onWorking,
    sessionId,
  ]);

  return {
    queuedMessages,
    queueMessage,
    clearQueuedMessages,
    steerQueued,
    editQueued,
    deleteQueued,
  };
}
