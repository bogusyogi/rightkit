// The recognition check: the user reads a passage for a set time, then the native side verifies that the
// speech engine heard it. The package owns the timing and the phases; the app owns the passage, every label
// and the three native commands (it passes them in a `RecognitionCheckPort`).
import { useCallback, useEffect, useRef, useState } from "react";

/** What starting a check returns; `minSeconds` and `targetSeconds` size the reading. */
export interface RecognitionCheckStart {
  sessionId: string;
  minSeconds: number;
  targetSeconds: number;
}

export interface RecognitionCheckResult {
  status: "passed" | "failed" | "capture_failed";
  message: string;
}

export interface RecognitionCheckProgress {
  sessionId: string;
  phase: "recording" | "captured" | "testing_route" | "complete" | "failed";
  message?: string;
}

/** The native side of the check. Keep one object for the life of the screen: a new object resubscribes. */
export interface RecognitionCheckPort {
  start(): Promise<RecognitionCheckStart>;
  stop(sessionId: string): Promise<RecognitionCheckResult>;
  /** Idempotent: a session that already ended is not an error. */
  cancel(sessionId: string): Promise<void>;
  /** Progress of the running session; resolves to the unsubscribe. */
  subscribe(handler: (progress: RecognitionCheckProgress) => void): Promise<() => void>;
}

export interface InvokeRecognitionCheckPortOptions {
  invoke: <T = unknown>(command: string, args?: Record<string, unknown>) => Promise<T>;
  listen: <T>(event: string, handler: (event: { payload: T }) => void) => Promise<() => void>;
  /** The app's command names. `stop` and `cancel` are sent `{ sessionId }`. */
  commands: { start: string; stop: string; cancel: string };
  /** The app's progress event; its payload is a `RecognitionCheckProgress`. */
  progressEvent: string;
}

/** A `RecognitionCheckPort` over the app's own `invoke` and `listen` (no Tauri import in this package). */
export function createInvokeRecognitionCheckPort(options: InvokeRecognitionCheckPortOptions): RecognitionCheckPort {
  const { invoke, listen, commands, progressEvent } = options;
  return {
    start: () => invoke<RecognitionCheckStart>(commands.start),
    stop: (sessionId) => invoke<RecognitionCheckResult>(commands.stop, { sessionId }),
    cancel: (sessionId) => invoke<void>(commands.cancel, { sessionId }),
    subscribe: (handler) => listen<RecognitionCheckProgress>(progressEvent, (event) => handler(event.payload)),
  };
}

const FINISH_MARGIN_SECONDS = 1;

type CheckPhase = "idle" | "recording" | "processing" | "passed" | "failed";

interface CheckRun {
  sessionId?: string;
  startedAt?: number;
  finishing: boolean;
}

export interface UseRecognitionCheckOptions {
  port: RecognitionCheckPort;
  /** False cancels a running check (the screen or panel holding it closed). */
  active: boolean;
  setError: (message: string) => void;
  /** Shown as `progress` while the native side checks the recording. */
  processingLabel?: string;
}

/**
 * Drives one recognition check. `start` opens a session and the clock; `finish` is allowed once the reading
 * reached `minSeconds` (plus one second: the capture stream opens after the command returns, so finishing at
 * exactly `minSeconds` could fail the native sample-count check as too short). Leaving (`active` false, or an
 * unmount while a capture runs) cancels the session so the microphone is released and the user's media resumes.
 */
export function useRecognitionCheck({ port, active, setError, processingLabel = "" }: UseRecognitionCheckOptions) {
  const [phase, setPhase] = useState<CheckPhase>("idle");
  const [elapsed, setElapsed] = useState(0);
  const [minSeconds, setMinSeconds] = useState(30);
  const [targetSeconds, setTargetSeconds] = useState(35);
  const [progress, setProgress] = useState("");
  const runRef = useRef<CheckRun | null>(null);

  useEffect(() => {
    const unlisten = port.subscribe((payload) => {
      const run = runRef.current;
      if (!run || payload.sessionId !== run.sessionId) return;
      if (payload.phase === "recording" && !run.finishing) setPhase("recording");
      if (payload.phase === "captured" || payload.phase === "testing_route") {
        setPhase("processing");
      }
      if (payload.message) setProgress(payload.message);
    });
    void unlisten.catch(() => undefined);
    return () => void unlisten.then((fn) => fn()).catch(() => undefined);
  }, [port]);

  useEffect(() => {
    if (phase !== "recording") return;
    const interval = window.setInterval(() => {
      const startedAt = runRef.current?.startedAt ?? Date.now();
      setElapsed(Math.floor((Date.now() - startedAt) / 1000));
    }, 250);
    return () => window.clearInterval(interval);
  }, [phase]);

  const abandonRun = useCallback(async () => {
    const sessionId = runRef.current?.sessionId;
    runRef.current = null;
    if (sessionId) {
      await port.cancel(sessionId).catch(() => undefined);
    }
  }, [port]);

  const cancel = useCallback(async () => {
    const abandoned = abandonRun();
    setElapsed(0);
    setProgress("");
    setPhase("idle");
    await abandoned;
  }, [abandonRun]);

  useEffect(() => {
    if (!active) void cancel();
  }, [active, cancel]);

  // Leaving the step (Back, a rail click, a WebView reload) unmounts the hook while a native capture may be
  // running. Cancel it so the microphone is released; `cancel` is idempotent natively.
  useEffect(() => () => void abandonRun(), [abandonRun]);

  async function start() {
    if (!active || runRef.current) return;
    const run: CheckRun = { finishing: false };
    runRef.current = run;
    setError("");
    setProgress("");
    setElapsed(0);
    try {
      const started = await port.start();
      if (runRef.current !== run) {
        await port.cancel(started.sessionId).catch(() => undefined);
        return;
      }
      run.sessionId = started.sessionId;
      run.startedAt = Date.now();
      setMinSeconds(started.minSeconds + FINISH_MARGIN_SECONDS);
      setTargetSeconds(started.targetSeconds);
      setPhase("recording");
    } catch (error) {
      if (runRef.current !== run) return;
      runRef.current = null;
      setError(String(error));
      setPhase("failed");
    }
  }

  async function finish() {
    const run = runRef.current;
    if (!run?.sessionId || run.finishing || elapsed < minSeconds) return;
    run.finishing = true;
    setError("");
    setPhase("processing");
    setProgress(processingLabel);
    try {
      const result = await port.stop(run.sessionId);
      if (runRef.current !== run) return;
      runRef.current = null;
      setProgress(result.message);
      setPhase(result.status === "passed" ? "passed" : "failed");
    } catch (error) {
      if (runRef.current !== run) return;
      runRef.current = null;
      setError(String(error));
      setPhase("failed");
    }
  }

  return {
    cancel,
    elapsed,
    finish,
    minSeconds,
    phase,
    progress,
    readyToFinish: phase === "recording" && elapsed >= minSeconds,
    setupComplete: phase === "passed" || phase === "failed",
    start,
    targetSeconds,
  };
}

export type RecognitionCheckController = ReturnType<typeof useRecognitionCheck>;
