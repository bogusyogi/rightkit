import { useCallback, useEffect, useState } from "react";
import { forkSession as forkSessionApi, rewindSession as rewindSessionApi } from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, SessionRow } from "@rightkit/agent-ui-core/lib/types";

export interface ForkInteraction {
  kind: "fork";
  sourceSessionId: string;
  sourceTitle: string;
  title: string;
}

export interface ResponseForkInteraction {
  kind: "response-fork";
  sourceSessionId: string;
  messageId: string;
  turnCount: number;
  sourceTitle: string;
  title: string;
}

export interface RewindInteraction {
  kind: "rewind";
  sourceSessionId: string;
  messageId: string;
  turns: number;
}

export type SessionBranchingInteraction =
  | ForkInteraction
  | ResponseForkInteraction
  | RewindInteraction;

export interface RewindResult {
  sourceSessionId: string;
  messageId: string;
  turns: number;
  restoredText: string | null;
}

export interface UseSessionBranchingOptions {
  config: DaemonConfig | null;
  sessionId: string | null;
  sessionTitle?: string | null;
  existingSessionTitles?: readonly string[];
  isWorking?: boolean;
  formatError?: (error: unknown, fallback: string) => string;
  onStatus?: (status: string) => void;
  onForked?: (session: SessionRow) => void;
  onRewound?: (result: RewindResult) => void;
}

function defaultFormatError(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

function titleForSession(sessionId: string, title: string | null | undefined): string {
  return title?.trim() || sessionId;
}

function requireSelectedSession(
  config: DaemonConfig | null,
  sessionId: string | null,
  isWorking: boolean,
  onStatus: (status: string) => void,
  action: "fork" | "rewind",
): sessionId is string {
  if (!config || !sessionId) {
    onStatus(`Select a session before ${action === "fork" ? "forking" : "rewinding"}`);
    return false;
  }
  if (isWorking) {
    onStatus(`Stop the session before ${action === "fork" ? "forking" : "rewinding"}`);
    return false;
  }
  return true;
}

/**
 * Preserve Waku's response-fork title sequence. A title with a numeric suffix
 * is only treated as part of a sequence when its unsuffixed sibling exists.
 */
export function nextResponseForkTitle(
  sourceTitle: string,
  existingTitles: Iterable<string>,
): string {
  const titles = [...existingTitles];
  const numbered = numberedTitleSuffix(sourceTitle);
  const base = numbered && titles.some((title) => title === numbered[0]) ? numbered[0] : sourceTitle;
  let highestNumber = 1;

  for (const title of titles) {
    if (title === base) {
      highestNumber = Math.max(highestNumber, 1);
      continue;
    }
    const candidate = numberedTitleSuffix(title);
    if (candidate && candidate[0] === base) highestNumber = Math.max(highestNumber, candidate[1]);
  }

  return `${base} (${Math.max(highestNumber + 1, 2)})`;
}

function numberedTitleSuffix(title: string): [string, number] | null {
  const marker = title.lastIndexOf(" (");
  if (marker <= 0 || !title.endsWith(")")) return null;
  const base = title.slice(0, marker);
  const rawNumber = title.slice(marker + 2, -1);
  if (!/^\d+$/.test(rawNumber)) return null;
  const number = Number(rawNumber);
  return Number.isSafeInteger(number) && number >= 2 ? [base, number] : null;
}

/**
 * Renderer-side interaction adapter for the daemon's session fork/rewind
 * routes. It owns only confirmation and in-flight state; transcript, session
 * selection, persistence, and provider execution remain daemon/app owners.
 *
 * Waku's DriverHandle exposes `rollback(turns)` and `fork(turnsToRemove)` as
 * distinct provider operations. the host app exposes the corresponding durable
 * session operations as `POST /sessions/:id/rewind` and
 * `POST /sessions/:id/fork`; the adapter deliberately keeps those operations
 * separate rather than mutating transcript state locally.
 */
export function useSessionBranching({
  config,
  sessionId,
  sessionTitle,
  existingSessionTitles = [],
  isWorking = false,
  formatError = defaultFormatError,
  onStatus = () => undefined,
  onForked,
  onRewound,
}: UseSessionBranchingOptions) {
  const [interaction, setInteraction] = useState<SessionBranchingInteraction | null>(null);
  const [actionPending, setActionPending] = useState<"fork" | "rewind" | null>(null);

  // biome-ignore lint/correctness/useExhaustiveDependencies: session transitions discard stale branching interaction.
  useEffect(() => {
    setInteraction(null);
  }, [sessionId]);

  const cancel = useCallback(() => {
    if (actionPending === null) setInteraction(null);
  }, [actionPending]);

  const beginFork = useCallback(
    (title?: string) => {
      if (!requireSelectedSession(config, sessionId, isWorking, onStatus, "fork")) return;
      const sourceTitle = titleForSession(sessionId, sessionTitle);
      setInteraction({
        kind: "fork",
        sourceSessionId: sessionId,
        sourceTitle,
        title: title?.trim() || `Fork of ${sourceTitle}`,
      });
    },
    [config, isWorking, onStatus, sessionId, sessionTitle],
  );

  const beginResponseFork = useCallback(
    (messageId: string, turnCount: number, title?: string) => {
      if (!requireSelectedSession(config, sessionId, isWorking, onStatus, "fork")) return;
      if (!messageId.trim() || !Number.isSafeInteger(turnCount) || turnCount < 1) {
        onStatus("Choose a response before forking");
        return;
      }
      const sourceTitle = titleForSession(sessionId, sessionTitle);
      setInteraction({
        kind: "response-fork",
        sourceSessionId: sessionId,
        messageId,
        turnCount,
        sourceTitle,
        title:
          title?.trim() || nextResponseForkTitle(sourceTitle, existingSessionTitles),
      });
    },
    [config, existingSessionTitles, isWorking, onStatus, sessionId, sessionTitle],
  );

  const beginRewind = useCallback(
    (messageId: string, turns = 1) => {
      if (!requireSelectedSession(config, sessionId, isWorking, onStatus, "rewind")) return;
      if (!messageId.trim() || !Number.isSafeInteger(turns) || turns < 1) {
        onStatus("Choose a message before rewinding");
        return;
      }
      setInteraction({ kind: "rewind", sourceSessionId: sessionId, messageId, turns });
    },
    [config, isWorking, onStatus, sessionId],
  );

  const setForkTitle = useCallback((title: string) => {
    setInteraction((current) =>
      current?.kind === "fork" || current?.kind === "response-fork"
        ? { ...current, title }
        : current,
    );
  }, []);

  const forkSessionNow = useCallback(
    async (title?: string): Promise<SessionRow | null> => {
      if (!requireSelectedSession(config, sessionId, isWorking, onStatus, "fork")) return null;
      if (!config || actionPending !== null) return null;
      const sourceSessionId = sessionId;
      const sourceTitle = titleForSession(sourceSessionId, sessionTitle);
      const nextTitle = title?.trim() || `Fork of ${sourceTitle}`;
      setActionPending("fork");
      try {
        const forked = await forkSessionApi(config, sourceSessionId, nextTitle);
        onForked?.(forked);
        onStatus(`Forked session: ${forked.title}`);
        return forked;
      } catch (error) {
        onStatus(formatError(error, "Fork session failed"));
        return null;
      } finally {
        setActionPending(null);
      }
    },
    [actionPending, config, formatError, isWorking, onForked, onStatus, sessionId, sessionTitle],
  );

  const confirmFork = useCallback(async (): Promise<SessionRow | null> => {
    if (!interaction || (interaction.kind !== "fork" && interaction.kind !== "response-fork")) {
      return null;
    }
    if (interaction.kind === "response-fork") {
      onStatus(
        "Response branching needs a daemon turn-scoped fork endpoint; the whole-session fork was not used",
      );
      setInteraction(null);
      return null;
    }
    const title = interaction.title.trim();
    if (!title) {
      onStatus("Fork title cannot be empty");
      return null;
    }
    const sourceSessionId = interaction.sourceSessionId;
    if (sourceSessionId !== sessionId) {
      onStatus("Session changed before fork confirmation");
      setInteraction(null);
      return null;
    }
    setInteraction(null);
    return forkSessionNow(title);
  }, [forkSessionNow, interaction, onStatus, sessionId]);

  const rewindMessage = useCallback(
    async (messageId: string, turns = 1): Promise<string | null> => {
      if (!requireSelectedSession(config, sessionId, isWorking, onStatus, "rewind")) return null;
      if (!config || !messageId.trim() || !Number.isSafeInteger(turns) || turns < 1) {
        onStatus("Choose a message before rewinding");
        return null;
      }
      if (actionPending !== null) return null;
      const sourceSessionId = sessionId;
      setActionPending("rewind");
      try {
        let restoredText: string | null = null;
        for (let index = 0; index < turns; index += 1) {
          restoredText = await rewindSessionApi(config, sourceSessionId);
        }
        const result = { sourceSessionId, messageId, turns, restoredText };
        onRewound?.(result);
        onStatus("Ready");
        return restoredText;
      } catch (error) {
        onStatus(formatError(error, "Rewind failed"));
        return null;
      } finally {
        setActionPending(null);
      }
    },
    [actionPending, config, formatError, isWorking, onRewound, onStatus, sessionId],
  );

  const confirmRewind = useCallback(async (): Promise<string | null> => {
    if (!interaction || interaction.kind !== "rewind") return null;
    const { messageId, turns, sourceSessionId } = interaction;
    if (sourceSessionId !== sessionId) {
      onStatus("Session changed before rewind confirmation");
      setInteraction(null);
      return null;
    }
    setInteraction(null);
    return rewindMessage(messageId, turns);
  }, [interaction, onStatus, rewindMessage, sessionId]);

  return {
    interaction,
    forkInteraction: interaction?.kind === "fork" ? interaction : null,
    responseForkInteraction: interaction?.kind === "response-fork" ? interaction : null,
    rewindInteraction: interaction?.kind === "rewind" ? interaction : null,
    actionPending,
    isPending: actionPending !== null,
    setForkTitle,
    beginFork,
    beginResponseFork,
    beginRewind,
    cancel,
    confirmFork,
    confirmRewind,
    forkSessionNow,
    rewindMessage,
  };
}

export type UseSessionBranchingReturn = ReturnType<typeof useSessionBranching>;
