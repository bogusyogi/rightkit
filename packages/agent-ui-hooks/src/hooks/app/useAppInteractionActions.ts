import type React from "react";
import { useRef } from "react";
import {
  createExecutionProcess,
  executeCommand,
  getTranscript,
  interruptRun,
  killExecutionProcess,
  orchestrate,
  rewindSession,
  sendMessage,
  writeExecutionProcessStdin,
  type Attachment,
} from "@rightkit/agent-ui-core/lib/daemon";
import { sanitizeError } from "../../components/sanitizeError.js";
import { useStreamingStore } from "@rightkit/agent-ui-core/state/streamingStore";
import { useReferenceStore } from "@rightkit/agent-ui-core/state/referenceStore";
import { useComposerStore } from "@rightkit/agent-ui-core/state/composerStore";
import type { DaemonConfig, SessionRow, TranscriptMessage } from "@rightkit/agent-ui-core/lib/types";
import type { ReferenceMeta } from "@rightkit/agent-ui-core/lib/types/references";
import { fromReferenceMeta } from "@rightkit/agent-ui-core/lib/types/references";

interface ComposerState {
  composer: string;
  attachments: Attachment[];
  rememberPrompt: (prompt: string) => void;
  setComposer: (value: string) => void;
  setAttachments: (attachments: Attachment[]) => void;
  clear: () => void;
}

interface UseAppInteractionActionsParams {
  config: DaemonConfig | null;
  selected: SessionRow | null;
  workspaceRoot: string;
  providerKeyMissing: boolean;
  isWorking: boolean;
  messages: TranscriptMessage[];
  terminalInput: string;
  selectedTerminalProcessId: string | null;
  composerState: ComposerState;
  addPendingUserNonce: (nonce: string) => void;
  queueMessage: (text: string, attachments?: Attachment[], references?: ReferenceMeta[]) => void;
  setMessages: React.Dispatch<React.SetStateAction<TranscriptMessage[]>>;
  setStreamingText: (value: string) => void;
  setStreamingReasoning: (value: string) => void;
  setIsWorking: (value: boolean) => void;
  setStatus: (value: string) => void;
  setPane: (value: "session" | "terminal") => void;
  setSelectedId: (value: string) => void;
  setTerminalActionPending: (value: string | null) => void;
  setTerminalError: (value: string | null) => void;
  setSelectedTerminalProcessId: (value: string | null) => void;
  setTerminalInput: (value: string) => void;
  loadTerminalProcesses: () => Promise<void>;
  loadTerminalLogs: (processId: string) => Promise<void>;
  refresh: (config?: DaemonConfig) => Promise<void>;
  compact: () => Promise<void>;
  messageOf: (error: unknown, fallback: string) => string;
  isEditableUserMessage: (content: string) => boolean;
  /** Present only while an external conductor owns the session. */
  driverSend?: ((text: string) => Promise<void>) | null;
}

export function useAppInteractionActions({
  config,
  selected,
  workspaceRoot,
  providerKeyMissing,
  isWorking,
  messages,
  terminalInput,
  selectedTerminalProcessId,
  composerState,
  addPendingUserNonce,
  queueMessage,
  setMessages,
  setStreamingText,
  setStreamingReasoning,
  setIsWorking,
  setStatus,
  setPane,
  setSelectedId,
  setTerminalActionPending,
  setTerminalError,
  setSelectedTerminalProcessId,
  setTerminalInput,
  loadTerminalProcesses,
  loadTerminalLogs,
  refresh,
  compact,
  messageOf,
  isEditableUserMessage,
  driverSend,
}: UseAppInteractionActionsParams) {
  const selectedIdRef = useRef<string | null>(selected?.id ?? null);
  selectedIdRef.current = selected?.id ?? null;

  async function sendSessionMessage(
    text: string,
    attachments: Attachment[] = [],
    references: ReferenceMeta[] = [],
  ) {
    if (!config || !selected) return;
    const messageText = text.trim();
    if (!messageText && attachments.length === 0 && references.length === 0) return;
    if (providerKeyMissing) {
      setStatus("Add a provider key first");
      return;
    }
    if (isWorking) {
      queueMessage(messageText, attachments, references);
      return;
    }
    const nonce = crypto.randomUUID();
    const localMessageId = `local-${Date.now()}`;
    addPendingUserNonce(nonce);
    setMessages((rows) => [
      ...rows,
      {
        $kind: "user",
        id: localMessageId,
        createdAt: new Date().toISOString(),
        content:
          messageText ||
          `[${attachments.length} image attachment${attachments.length === 1 ? "" : "s"}]`,
        pinned: false,
        nonce,
        attachments: references.length ? references : undefined,
      },
    ]);
    setStreamingText("");
    useStreamingStore.getState().clear();
    setIsWorking(true);
    setStatus("Sending...");
    try {
      await sendMessage(
        config,
        selected.id,
        messageText,
        nonce,
        attachments.length ? attachments : undefined,
        workspaceRoot,
        references.length ? references : undefined,
      );
      void reconcileTranscriptAfterSend(config, selected.id, messageText);
    } catch (err) {
      setMessages((rows) => rows.filter((row) => row.id !== localMessageId));
      const currentComposer = useComposerStore.getState();
      if (!currentComposer.composer && currentComposer.attachments.length === 0) {
        composerState.setComposer(messageText);
        composerState.setAttachments(attachments);
        useReferenceStore.getState().addReferences(
          references.map((reference) => {
            const attachment = attachments.find((item) => item.id === reference.id);
            return {
              ...reference,
              ...(attachment
                ? { previewUrl: `data:${attachment.mimeType};base64,${attachment.data}` }
                : {}),
            };
          }),
        );
      }
      setIsWorking(false);
      setStatus(messageOf(err, "Send failed"));
    }
  }

  async function reconcileTranscriptAfterSend(
    activeConfig: DaemonConfig,
    sessionId: string,
    submittedText: string,
  ) {
    const deadline = Date.now() + 60_000;
    while (Date.now() < deadline) {
      await new Promise((resolve) => window.setTimeout(resolve, 1000));
      let rows: TranscriptMessage[];
      try {
        rows = (await getTranscript(activeConfig, sessionId)).filter(
          (row) => row.$kind !== "system",
        );
      } catch {
        continue;
      }
      if (selectedIdRef.current !== sessionId) return;
      setMessages(rows);
      let submittedIndex = -1;
      for (let index = rows.length - 1; index >= 0; index -= 1) {
        const row = rows[index];
        if (row.$kind === "user" && (!submittedText || row.content === submittedText)) {
          submittedIndex = index;
          break;
        }
      }
      const completed = rows
        .slice(Math.max(0, submittedIndex + 1))
        .some((row) => row.$kind === "assistant");
      if (submittedIndex >= 0 && completed) {
        setIsWorking(false);
        setStatus("Ready");
        return;
      }
    }
  }

  function appendLocalAssistant(content: string, prefix: string) {
    setMessages((rows) => [
      ...rows,
      {
        $kind: "assistant",
        id: `${prefix}-${Date.now()}`,
        createdAt: new Date().toISOString(),
        content,
        toolCalls: [],
        pinned: false,
      },
    ]);
  }

  /// Echo the command the user actually typed. Slash commands never round-trip
  /// through the transcript, so without this the pane showed only the response
  /// ("Starting whole-repo auditor pass…") with no record of the request —
  /// leaving the human unable to see their own raw instruction.
  function appendLocalUser(content: string) {
    setMessages((rows) => [
      ...rows,
      {
        $kind: "user",
        id: `command-input-${Date.now()}`,
        createdAt: new Date().toISOString(),
        content,
        toolCalls: [],
        pinned: false,
      },
    ]);
  }

  function commandActionKind(action: unknown) {
    return typeof action === "string"
      ? action
      : action && typeof action === "object"
        ? Object.keys(action)[0]
        : "none";
  }

  async function applyCommandAction(action: unknown, requestLabel?: string) {
    if (!config || !selected) return;
    const kind = commandActionKind(action);
    switch (kind) {
      case "clear_chat":
        setMessages([]);
        setStreamingText("");
        setStreamingReasoning("");
        useStreamingStore.getState().clear();
        break;
      case "compact_context":
        await compact();
        break;
      case "list_sessions":
        await refresh(config);
        break;
      case "switch_session": {
        const id = (action as { switch_session?: string }).switch_session;
        if (id) {
          setSelectedId(id);
        }
        break;
      }
      case "submit_message": {
        const message = (action as { submit_message?: string }).submit_message;
        if (message?.trim()) {
          await sendSessionMessage(message.trim());
        }
        break;
      }
      case "conduct_goal": {
        // /audit, /blueprint and /commit resolve to a conductor goal. Without
        // this case the switch fell through silently: the command printed
        // "Starting whole-repo auditor pass…" and then nothing ran — no task
        // graph, no agents, no error. The orchestrator was never reached from
        // the UI at all.
        const goal = (action as { conduct_goal?: string }).conduct_goal;
        if (goal?.trim()) {
          setStatus("Orchestrating…");
          // Mark the session busy for the whole dispatch. Without this the
          // window sat completely static while the conductor planned — no
          // spinner, no thinking state — so a running audit and a hung one
          // looked identical.
          setIsWorking(true);
          try {
            await orchestrate(config, selected.id, goal.trim(), false, requestLabel);
          } catch (error) {
            setIsWorking(false);
            appendLocalAssistant(
              `Orchestration failed to start: ${sanitizeError(error)}`,
              "command-error",
            );
            setStatus("Orchestration failed");
          }
        }
        break;
      }
    }
  }

  async function runSlashCommand(input: string) {
    if (!config || !selected) return;
    try {
      setStatus(`Running ${input.split(/\s+/, 1)[0]}...`);
      appendLocalUser(input);
      const result = await executeCommand(config, input, selected.id);
      const actionKind = commandActionKind(result.action);
      await applyCommandAction(result.action, input);
      if (result.output.trim() && actionKind !== "clear_chat" && actionKind !== "submit_message") {
        appendLocalAssistant(result.output, result.success ? "command" : "command-error");
      }
      setStatus(result.success ? "Command complete" : "Command failed");
    } catch (err) {
      setStatus(messageOf(err, "Command failed"));
    }
  }

  async function submit() {
    if (!config || !selected) return;
    const text = composerState.composer.trim();
    const attachments = [...composerState.attachments];
    const references = useReferenceStore.getState().toWireMeta();
    if (!text && attachments.length === 0 && references.length === 0) return;
    if (text.startsWith("/") && attachments.length === 0 && references.length === 0) {
      composerState.rememberPrompt(text);
      composerState.setComposer("");
      await runSlashCommand(text);
      return;
    }
    // An external conductor owns the whole turn. Without this branch the
    // composer posts to the native /messages route no matter which conductor
    // the session names, so a Codex session answers as the native engine on
    // whatever worker model is configured — while the composer still says
    // "Codex via the host app".
    if (driverSend) {
      if (attachments.length > 0 || references.length > 0) {
        // Refuse before clearing: the driver prompt carries text only, and
        // silently dropping an attachment the user attached is worse than
        // not sending.
        setStatus("This conductor takes text only — remove attachments or switch to Native");
        return;
      }
      composerState.rememberPrompt(text);
      composerState.clear();
      try {
        await driverSend(text);
      } catch (err) {
        setStatus(messageOf(err, "Conductor could not accept the prompt"));
      }
      return;
    }
    if (providerKeyMissing) {
      setStatus("Add a provider key first");
      return;
    }
    composerState.rememberPrompt(text);
    composerState.clear();
    await sendSessionMessage(text, attachments, references);
  }

  async function createTerminalSession() {
    if (!config || !selected) return;
    setTerminalActionPending("create");
    setTerminalError(null);
    try {
      const response = await createExecutionProcess(config, { sessionId: selected.id });
      setSelectedTerminalProcessId(response.process.id);
      setTerminalInput("");
      await loadTerminalProcesses();
      await loadTerminalLogs(response.process.id);
      setPane("terminal");
      setStatus("Terminal opened");
    } catch (err) {
      const message = messageOf(err, "Terminal launch failed");
      setTerminalError(message);
      setStatus(message);
    } finally {
      setTerminalActionPending(null);
    }
  }

  async function sendTerminalInput() {
    if (!config || !selectedTerminalProcessId || !terminalInput.length) return;
    setTerminalActionPending(`stdin:${selectedTerminalProcessId}`);
    try {
      await writeExecutionProcessStdin(config, selectedTerminalProcessId, {
        input: terminalInput,
        appendNewline: true,
      });
      setTerminalInput("");
      await loadTerminalLogs(selectedTerminalProcessId);
    } catch (err) {
      setTerminalError(messageOf(err, "Terminal stdin failed"));
    } finally {
      setTerminalActionPending(null);
    }
  }

  async function stopTerminalProcess(processId: string) {
    if (!config) return;
    setTerminalActionPending(`kill:${processId}`);
    try {
      await killExecutionProcess(config, processId);
      await loadTerminalProcesses();
      if (selectedTerminalProcessId === processId) {
        await loadTerminalLogs(processId);
      }
      setStatus("Terminal stopped");
    } catch (err) {
      const message = messageOf(err, "Terminal stop failed");
      setTerminalError(message);
      setStatus(message);
    } finally {
      setTerminalActionPending(null);
    }
  }

  async function interrupt() {
    if (!config || !selected) return;
    try {
      await interruptRun(config, selected.id);
      setStatus("Interrupting...");
    } catch (err) {
      setStatus(messageOf(err, "Interrupt failed"));
    }
  }

  async function editMessage(messageId: string) {
    if (!config || !selected || isWorking) return;
    const visibleUserMessages = messages.filter(
      (message): message is Extract<TranscriptMessage, { $kind: "user" }> =>
        message.$kind === "user" && isEditableUserMessage(message.content),
    );
    const targetVisibleIndex = visibleUserMessages.findIndex((message) => message.id === messageId);
    if (targetVisibleIndex === -1) return;
    const targetMessage = visibleUserMessages[targetVisibleIndex];
    const targetRawIndex = messages.findIndex((message) => message.id === messageId);
    if (targetRawIndex === -1) return;
    const rewindCount = visibleUserMessages.length - targetVisibleIndex;
    try {
      setStatus(rewindCount === 1 ? "Rewinding..." : `Rewinding ${rewindCount} turns...`);
      let restoredText: string | null = null;
      for (let i = 0; i < rewindCount; i += 1) {
        restoredText = await rewindSession(config, selected.id);
      }
      setMessages((rows) => rows.slice(0, targetRawIndex));
      composerState.setComposer(restoredText ?? targetMessage.content ?? "");
      useReferenceStore.getState().clear();
      useReferenceStore
        .getState()
        .addReferences((targetMessage.attachments ?? []).map(fromReferenceMeta));
      setStatus("Ready");
    } catch (err) {
      setStatus(messageOf(err, "Rewind failed"));
    }
  }

  return {
    submit,
    sendSessionMessage,
    createTerminalSession,
    sendTerminalInput,
    stopTerminalProcess,
    runSlashCommand,
    interrupt,
    editMessage,
  };
}
