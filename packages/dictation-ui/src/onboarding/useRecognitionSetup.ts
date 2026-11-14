import {
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { onboardingClient } from "./client.js";

const FINISH_MARGIN_SECONDS = 1;

type SetupPhase = "idle" | "recording" | "processing" | "passed" | "failed";

type Args = {
  active: boolean;
  setError: (message: string) => void;
};

export function useOnboardingRecognitionSetup({ active, setError }: Args) {
  const [phase, setPhase] = useState<SetupPhase>("idle");
  const [elapsed, setElapsed] = useState(0);
  const [minSeconds, setMinSeconds] = useState(30);
  const [targetSeconds, setTargetSeconds] = useState(35);
  const [progress, setProgress] = useState("");
  const sessionIdRef = useRef<string | null>(null);
  const startedAtRef = useRef<number | null>(null);

  useEffect(() => {
    const unlisten = onboardingClient().on("recognitionSetupProgress", (payload) => {
      if (payload.sessionId !== sessionIdRef.current) return;
      if (payload.phase === "recording") setPhase("recording");
      if (payload.phase === "captured" || payload.phase === "testing_route") {
        setPhase("processing");
      }
      if (payload.message) setProgress(payload.message);
    });
    return () => void unlisten.then((fn) => fn()).catch(() => undefined);
  }, []);

  useEffect(() => {
    if (phase !== "recording") return;
    const interval = window.setInterval(() => {
      const startedAt = startedAtRef.current ?? Date.now();
      setElapsed(Math.floor((Date.now() - startedAt) / 1000));
    }, 250);
    return () => window.clearInterval(interval);
  }, [phase]);

  const cancel = useCallback(async () => {
    const sessionId = sessionIdRef.current;
    sessionIdRef.current = null;
    startedAtRef.current = null;
    if (sessionId) {
      await onboardingClient().cancelRecognitionSetup(sessionId).catch(() => undefined);
    }
    setElapsed(0);
    setProgress("");
    setPhase("idle");
  }, []);

  useEffect(() => {
    if (!active && sessionIdRef.current) void cancel();
  }, [active, cancel]);

  // Leaving the step (Back, a rail click, a WebView reload) unmounts the hook while a
  // native capture may be running. Cancel it so the microphone is released and the
  // user's media resumes; `cancel` is idempotent natively.
  useEffect(
    () => () => {
      const sessionId = sessionIdRef.current;
      sessionIdRef.current = null;
      startedAtRef.current = null;
      if (sessionId) {
        void onboardingClient().cancelRecognitionSetup(sessionId).catch(() => undefined);
      }
    },
    [],
  );

  async function start() {
    setError("");
    setProgress("");
    setElapsed(0);
    try {
      const start = await onboardingClient().startRecognitionSetup();
      sessionIdRef.current = start.sessionId;
      startedAtRef.current = Date.now();
      // The clock starts when the command returns, but the capture stream
      // opens afterwards on the worker thread, so finishing at exactly
      // `minSeconds` could fail the backend's sample-count check as "too
      // short". Gate Finish one second later.
      setMinSeconds(start.minSeconds + FINISH_MARGIN_SECONDS);
      setTargetSeconds(start.targetSeconds);
      setPhase("recording");
    } catch (error) {
      setError(String(error));
      setPhase("failed");
    }
  }

  async function finish() {
    const sessionId = sessionIdRef.current;
    if (!sessionId || elapsed < minSeconds) return;
    setError("");
    setPhase("processing");
    setProgress("Checking speech recognition…");
    try {
      const result = await onboardingClient().stopRecognitionSetup(sessionId);
      sessionIdRef.current = null;
      startedAtRef.current = null;
      setProgress(result.message);
      setPhase(result.status === "passed" ? "passed" : "failed");
    } catch (error) {
      sessionIdRef.current = null;
      startedAtRef.current = null;
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

export type OnboardingRecognitionSetupController = ReturnType<typeof useOnboardingRecognitionSetup>;
