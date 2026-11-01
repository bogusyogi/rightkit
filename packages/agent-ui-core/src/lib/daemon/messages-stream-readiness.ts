import type { DaemonConfig, GoalSnapshotV1, ProtocolEvent } from "../types";
import { requestDaemonApi } from "../generated/daemon-openapi";
import type { Attachment } from "./config-host";
import type { ReferenceMeta } from "../types/references";

export async function orchestrate(
  config: DaemonConfig,
  sessionId: string,
  goal: string,
  dryRun = false,
  /** What the user typed, when `goal` is an expansion of it (e.g. "/audit"). */
  requestLabel?: string,
): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdOrchestrate", {
    pathParams: { id: sessionId },
    body: { goal, dryRun, requestLabel },
  });
}

export async function sendMessage(
  config: DaemonConfig,
  sessionId: string,
  text: string,
  nonce?: string,
  attachments?: Attachment[],
  workspaceRoot?: string,
  references?: ReferenceMeta[],
): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdMessages", {
    pathParams: { id: sessionId },
    body: {
      text,
      nonce,
      attachments: attachments?.length ? attachments : undefined,
      references: references?.length ? references : undefined,
      // Bind an auto-created session to the active workspace on first message.
      workspaceRoot: workspaceRoot?.trim() || undefined,
    },
  });
}

export async function steerMessage(
  config: DaemonConfig,
  sessionId: string,
  text: string,
): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdSteer", {
    pathParams: { id: sessionId },
    body: { text },
  });
}

export async function setGoal(
  config: DaemonConfig,
  sessionId: string,
  text: string,
): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdGoal", {
    pathParams: { id: sessionId },
    body: { text },
  });
}

export async function clearGoal(config: DaemonConfig, sessionId: string): Promise<void> {
  await requestDaemonApi(config, "deleteSessionsByIdGoal", {
    pathParams: { id: sessionId },
  });
}

export async function getGoalSnapshot(
  config: DaemonConfig,
  sessionId: string,
): Promise<GoalSnapshotV1 | null> {
  const data = await requestDaemonApi<{ goal: GoalSnapshotV1 | null }>(
    config,
    "getSessionsByIdGoal",
    { pathParams: { id: sessionId } },
  );
  return data.goal;
}

export async function pauseGoal(config: DaemonConfig, sessionId: string): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdGoalPause", {
    pathParams: { id: sessionId },
  });
}

export async function resumeGoal(config: DaemonConfig, sessionId: string): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdGoalResume", {
    pathParams: { id: sessionId },
  });
}

export async function editGoal(
  config: DaemonConfig,
  sessionId: string,
  body: { text?: string; budgetTokens?: number; budgetUsd?: number },
): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdGoalEdit", {
    pathParams: { id: sessionId },
    body,
  });
}

export async function rewindSession(
  config: DaemonConfig,
  sessionId: string,
): Promise<string | null> {
  const data = await requestDaemonApi<{ rewound: boolean; text: string | null }>(
    config,
    "postSessionsByIdRewind",
    { pathParams: { id: sessionId } },
  );
  return data.text ?? null;
}

export async function interruptRun(config: DaemonConfig, sessionId: string): Promise<void> {
  await requestDaemonApi(config, "postSessionsByIdInterrupt", {
    pathParams: { id: sessionId },
  });
}

function readSseMessages(buffer: string, onData: (data: string) => void): string {
  let remaining = buffer;
  while (true) {
    const lfLf = remaining.indexOf("\n\n");
    const crlfCrlf = remaining.indexOf("\r\n\r\n");
    const nextBreak = [lfLf, crlfCrlf].filter((index) => index >= 0).sort((a, b) => a - b)[0];
    if (nextBreak === undefined) return remaining;

    const separatorLength = remaining.startsWith("\r\n\r\n", nextBreak) ? 4 : 2;
    const frame = remaining.slice(0, nextBreak);
    remaining = remaining.slice(nextBreak + separatorLength);
    const data = frame
      .split(/\r?\n/)
      .filter((line) => line.startsWith("data:"))
      .map((line) => line.slice(5).trimStart())
      .join("\n");
    if (data) onData(data);
  }
}

interface ReplayPage {
  events: ProtocolEvent[];
  next: number;
  complete: boolean;
  highWaterMark?: number;
}

/**
 * Page `/events/replay?after=N` with `after=next` until `complete`, applying
 * each event in order. Returns the final cursor to reconnect `/events` from.
 */
export async function replayEventPages(
  config: DaemonConfig,
  sessionId: string,
  afterSeq: number,
  onEvent: (event: ProtocolEvent) => void,
  signal?: AbortSignal,
): Promise<number> {
  let cursor = afterSeq;
  while (true) {
    const url = new URL(
      `${trimUrl(config.url)}/sessions/${encodeURIComponent(sessionId)}/events/replay`,
    );
    url.searchParams.set("after", String(cursor));
    const res = await fetch(url.toString(), { headers: tokenHeaders(config), signal });
    if (!res.ok) throw new Error(`Event replay failed: ${res.status}`);
    const page = (await res.json()) as ReplayPage;
    for (const event of page.events ?? []) {
      cursor = Math.max(cursor, event.seq);
      onEvent(event);
    }
    if (page.complete) return Math.max(cursor, page.next ?? cursor);
    if (!(page.next > cursor) && (page.events ?? []).length === 0) {
      throw new Error(`Event replay made no progress at after=${cursor}`);
    }
    cursor = Math.max(cursor, page.next);
  }
}

export function openEventStream(
  config: DaemonConfig,
  sessionId: string,
  afterSeq: number,
  onEvent: (event: ProtocolEvent) => void,
  onError: (error?: unknown) => void,
): () => void {
  const controller = new AbortController();
  let cancelled = false;
  let lastSeq = afterSeq;

  async function connectOnce() {
    const url = new URL(`${trimUrl(config.url)}/sessions/${encodeURIComponent(sessionId)}/events`);
    url.searchParams.set("after", String(lastSeq));
    const res = await fetch(url.toString(), {
      headers: tokenHeaders(config),
      signal: controller.signal,
    });
    if (!res.ok || !res.body) throw new Error(`SSE stream failed: ${res.status}`);

    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buffer = "";
    let resync = false;
    while (!cancelled && !resync) {
      const { value, done } = await reader.read();
      if (done) break;
      buffer += decoder.decode(value, { stream: true });
      buffer = readSseMessages(buffer, (data) => {
        if (resync) return;
        try {
          const event = JSON.parse(data) as ProtocolEvent;
          if ((event.$kind as string) === "streamResyncRequired") {
            // The synthetic resync seq must not advance the cursor.
            resync = true;
            return;
          }
          lastSeq = Math.max(lastSeq, event.seq);
          onEvent(event);
        } catch {
          /* ignore malformed frames */
        }
      });
    }
    if (resync) {
      await reader.cancel().catch(() => undefined);
      lastSeq = await replayEventPages(
        config,
        sessionId,
        lastSeq,
        (event) => {
          lastSeq = Math.max(lastSeq, event.seq);
          onEvent(event);
        },
        controller.signal,
      );
      // connectOnce returns; connect() reopens /events?after=lastSeq.
    }
  }

  async function connect() {
    let delayMs = 1000;
    while (!cancelled) {
      try {
        await connectOnce();
        delayMs = 1000;
      } catch (err) {
        if (cancelled || (err instanceof DOMException && err.name === "AbortError")) return;
        onError(err);
      }

      if (cancelled) return;
      await new Promise((resolve) => setTimeout(resolve, delayMs));
      delayMs = Math.min(delayMs * 2, 30000);
    }
  }

  void connect();
  return () => {
    cancelled = true;
    controller.abort();
  };
}

export function tokenHeaders(config: DaemonConfig): HeadersInit {
  return config.token ? { Authorization: `Bearer ${config.token}` } : {};
}

export function trimUrl(url: string) {
  return url.replace(/\/+$/, "");
}

export const ENABLE_READINESS_PANEL = true;

export type ReadinessVerdict = "pass" | "warn" | "block";
export type ReadinessState = "ready" | "degraded" | "unavailable" | "blocked";

export interface ReadinessRow {
  id: string;
  label: string;
  verdict: ReadinessVerdict;
  state: ReadinessState;
  detail: string;
  fix: string;
}

export interface ReadinessSnapshot {
  verdict: ReadinessVerdict;
  rows: ReadinessRow[];
}

export async function getReadiness(config: DaemonConfig): Promise<ReadinessSnapshot> {
  return requestDaemonApi<ReadinessSnapshot>(config, "getHealthReadiness");
}

export function isTauriHost() {
  return (
    typeof window !== "undefined" && ("__TAURI_INTERNALS__" in window || "__TAURI__" in window)
  );
}
