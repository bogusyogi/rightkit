import { useCallback, useEffect, useRef, useState } from "react";
import {
  defaultMicErrorCopy,
  useMicRecorder,
  type MicRecording,
  type MicrophoneAccessRequester,
} from "./useMicRecorder";

export type ConversationStatus = "idle" | "listening" | "transcribing" | "thinking" | "speaking";

export interface PendingVoiceResponse {
  id: string;
  pending: boolean;
  text: string;
}

export interface VoiceSpeechAdapter {
  speak: (text: string) => Promise<void>;
  stop: () => void;
}

export interface VoiceConversationOptions {
  busy: boolean;
  enabled: boolean;
  onFatalError?: () => void;
  onInterrupt?: () => Promise<void> | void;
  onStopWord?: () => void;
  onSubmit: (text: string) => Promise<void> | void;
  onTranscribeAudio?: (audio: Blob) => Promise<string>;
  pendingResponse: () => PendingVoiceResponse | null;
  consumePendingResponse: () => void;
  beforeMicOpen?: () => Promise<void> | void;
  requestMicrophoneAccess?: MicrophoneAccessRequester;
  speech?: VoiceSpeechAdapter;
}

export interface VoiceConversationController {
  end: () => Promise<void>;
  error: string | null;
  level: number;
  muted: boolean;
  start: () => Promise<void>;
  status: ConversationStatus;
  stopTurn: () => void;
  toggleMute: () => void;
}

const INTERRUPT_SETTLE_TIMEOUT_MS = 5_000;
const LISTEN_TIMEOUT_MS = 60_000;
const BARGE_SPEECH_LEVEL = 0.075;
const BARGE_SILENCE_MS = 1_250;

function defaultSpeechAdapter(): VoiceSpeechAdapter {
  return {
    speak: (text) => {
      if (typeof window === "undefined" || typeof window.speechSynthesis === "undefined") {
        return Promise.reject(new Error("Spoken replies are not supported in this environment."));
      }
      window.speechSynthesis.cancel();
      return new Promise<void>((resolve, reject) => {
        const utterance = new SpeechSynthesisUtterance(text);
        utterance.onend = () => resolve();
        utterance.onerror = (event) => reject(new Error(event.error || "Speech playback failed."));
        window.speechSynthesis.speak(utterance);
      });
    },
    stop: () => {
      if (typeof window !== "undefined" && typeof window.speechSynthesis !== "undefined") {
        window.speechSynthesis.cancel();
      }
    },
  };
}

function stopCommand(text: string): boolean {
  return /^(?:stop|cancel|never mind|nevermind|goodbye|good bye)[.!?]*$/i.test(text.trim());
}

const DEFAULT_SPEECH_ADAPTER = defaultSpeechAdapter();

/**
 * Hermes' hands-free loop, adapted to the renderer seam: recording and VAD
 * stay in useMicRecorder, speech output is an injected adapter, and daemon/STT
 * ownership stays with the caller through onTranscribeAudio/onSubmit.
 */
export function useVoiceConversation({
  busy,
  enabled,
  onFatalError,
  onInterrupt,
  onStopWord,
  onSubmit,
  onTranscribeAudio,
  pendingResponse,
  consumePendingResponse,
  beforeMicOpen,
  requestMicrophoneAccess,
  speech = DEFAULT_SPEECH_ADAPTER,
}: VoiceConversationOptions): VoiceConversationController {
  const { handle, level } = useMicRecorder(defaultMicErrorCopy(), requestMicrophoneAccess);
  const [status, setStatus] = useState<ConversationStatus>("idle");
  const [muted, setMuted] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const turnTimeoutRef = useRef<number | null>(null);
  const pendingStartRef = useRef(false);
  const turnClosingRef = useRef(false);
  const awaitingResponseRef = useRef(false);
  const statusRef = useRef<ConversationStatus>("idle");
  const enabledRef = useRef(enabled);
  const mutedRef = useRef(muted);
  const busyRef = useRef(busy);
  const wasEnabledRef = useRef(enabled);
  const beforeMicOpenRef = useRef(beforeMicOpen);
  const onInterruptRef = useRef(onInterrupt);
  const onStopWordRef = useRef(onStopWord);
  const bargeActiveRef = useRef(false);
  const bargeCapturePendingRef = useRef(false);
  const bargeSpeechRef = useRef(false);
  const speechSequenceRef = useRef(0);
  const speechPromiseRef = useRef<Promise<void> | null>(null);

  useEffect(() => {
    enabledRef.current = enabled;
  }, [enabled]);
  useEffect(() => {
    mutedRef.current = muted;
  }, [muted]);
  useEffect(() => {
    busyRef.current = busy;
  }, [busy]);
  useEffect(() => {
    statusRef.current = status;
  }, [status]);
  useEffect(() => {
    beforeMicOpenRef.current = beforeMicOpen;
  }, [beforeMicOpen]);
  useEffect(() => {
    onInterruptRef.current = onInterrupt;
  }, [onInterrupt]);
  useEffect(() => {
    onStopWordRef.current = onStopWord;
  }, [onStopWord]);

  const setConversationStatus = useCallback((next: ConversationStatus) => {
    statusRef.current = next;
    setStatus(next);
  }, []);

  const clearTurnTimeout = useCallback(() => {
    if (turnTimeoutRef.current !== null) {
      window.clearTimeout(turnTimeoutRef.current);
      turnTimeoutRef.current = null;
    }
  }, []);

  const stopSpeech = useCallback(() => {
    speechSequenceRef.current += 1;
    speech.stop();
    speechPromiseRef.current = null;
  }, [speech]);

  const cancelBargeCapture = useCallback(() => {
    if (!bargeActiveRef.current) return;
    bargeActiveRef.current = false;
    bargeCapturePendingRef.current = false;
    bargeSpeechRef.current = false;
    handle.cancel();
  }, [handle]);

  const submitCapturedUtterance = useCallback(
    async (recording: MicRecording) => {
      if (!onTranscribeAudio) return;
      setConversationStatus("transcribing");
      try {
        const transcript = (await onTranscribeAudio(recording.audio)).trim();
        if (!transcript) {
          pendingStartRef.current = true;
          setConversationStatus("idle");
          return;
        }
        if (stopCommand(transcript)) {
          setConversationStatus("idle");
          onStopWordRef.current?.();
          return;
        }
        const deadline = Date.now() + INTERRUPT_SETTLE_TIMEOUT_MS;
        while (busyRef.current && Date.now() < deadline) {
          await new Promise((resolve) => window.setTimeout(resolve, 100));
        }
        awaitingResponseRef.current = true;
        consumePendingResponse();
        await onSubmit(transcript);
        setConversationStatus("thinking");
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : String(cause));
        pendingStartRef.current = true;
        setConversationStatus("idle");
      }
    },
    [consumePendingResponse, onSubmit, onTranscribeAudio, setConversationStatus],
  );

  const finishBargeCapture = useCallback(async () => {
    if (!bargeActiveRef.current) return;
    bargeActiveRef.current = false;
    bargeCapturePendingRef.current = false;
    const result = await handle.stop();
    if (result?.audio) {
      await submitCapturedUtterance(result);
    } else if (enabledRef.current && !mutedRef.current) {
      pendingStartRef.current = true;
      setConversationStatus("idle");
    }
  }, [handle, setConversationStatus, submitCapturedUtterance]);

  const ensureBargeMonitor = useCallback(async () => {
    if (bargeActiveRef.current || !enabledRef.current || mutedRef.current) return;
    bargeActiveRef.current = true;
    bargeSpeechRef.current = false;
    try {
      await handle.start({
        silenceLevel: BARGE_SPEECH_LEVEL,
        silenceMs: BARGE_SILENCE_MS,
        onLevel: (nextLevel) => {
          if (!bargeSpeechRef.current && nextLevel >= BARGE_SPEECH_LEVEL) {
            bargeSpeechRef.current = true;
            bargeCapturePendingRef.current = true;
            // Invalidate the current spoken reply without cancelling the
            // interruption capture that now owns the microphone.
            speechSequenceRef.current += 1;
            speech.stop();
            speechPromiseRef.current = null;
            if (busyRef.current) void onInterruptRef.current?.();
          }
        },
        onSilence: () => void finishBargeCapture(),
        onError: (cause) => {
          bargeActiveRef.current = false;
          bargeCapturePendingRef.current = false;
          setError(cause.message);
        },
      });
    } catch (cause) {
      bargeActiveRef.current = false;
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
    }
  }, [finishBargeCapture, handle, speech]);

  const handleTurn = useCallback(
    async (forceTranscribe = false) => {
      if (turnClosingRef.current) return;
      turnClosingRef.current = true;
      clearTurnTimeout();
      setConversationStatus("transcribing");
      try {
        const result = await handle.stop();
        if (!result || (!result.heardSpeech && !forceTranscribe) || !onTranscribeAudio) {
          if (enabledRef.current && !mutedRef.current && !busyRef.current) pendingStartRef.current = true;
          setConversationStatus("idle");
          return;
        }
        const transcript = (await onTranscribeAudio(result.audio)).trim();
        if (!transcript) {
          pendingStartRef.current = true;
          setConversationStatus("idle");
          return;
        }
        if (stopCommand(transcript)) {
          setConversationStatus("idle");
          onStopWordRef.current?.();
          return;
        }
        awaitingResponseRef.current = true;
        consumePendingResponse();
        await onSubmit(transcript);
        setConversationStatus("thinking");
      } catch (cause) {
        setError(cause instanceof Error ? cause.message : String(cause));
        if (enabledRef.current && !mutedRef.current) pendingStartRef.current = true;
        setConversationStatus("idle");
      } finally {
        turnClosingRef.current = false;
      }
    },
    [clearTurnTimeout, consumePendingResponse, handle, onSubmit, onTranscribeAudio, setConversationStatus],
  );

  const startListening = useCallback(async () => {
    if (!enabledRef.current || mutedRef.current || busyRef.current) return;
    if (bargeCapturePendingRef.current || statusRef.current !== "idle") return;
    try {
      await beforeMicOpenRef.current?.();
      if (!enabledRef.current || mutedRef.current || busyRef.current || statusRef.current !== "idle") return;
      await handle.start({
        silenceLevel: 0.075,
        silenceMs: 1_250,
        idleSilenceMs: 12_000,
        onError: (cause) => {
          setError(cause.message);
          onFatalError?.();
        },
        onSilence: () => void handleTurn(),
      });
      setError(null);
      setConversationStatus("listening");
      clearTurnTimeout();
      turnTimeoutRef.current = window.setTimeout(() => void handleTurn(), LISTEN_TIMEOUT_MS);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setConversationStatus("idle");
      onFatalError?.();
    }
  }, [clearTurnTimeout, handle, handleTurn, onFatalError, setConversationStatus]);

  const speakPendingResponse = useCallback(async () => {
    const response = pendingResponse();
    if (!response || response.pending || busyRef.current) return;
    const sequence = ++speechSequenceRef.current;
    setConversationStatus("speaking");
    await ensureBargeMonitor();
    if (sequence !== speechSequenceRef.current) return;
    const playback = speech.speak(response.text);
    speechPromiseRef.current = playback;
    try {
      await playback;
      if (sequence !== speechSequenceRef.current) return;
      consumePendingResponse();
      awaitingResponseRef.current = false;
      pendingStartRef.current = enabledRef.current;
      setConversationStatus("idle");
    } catch (cause) {
      if (sequence !== speechSequenceRef.current) return;
      setError(cause instanceof Error ? cause.message : String(cause));
      pendingStartRef.current = enabledRef.current;
      setConversationStatus("idle");
    } finally {
      if (speechPromiseRef.current === playback) speechPromiseRef.current = null;
      cancelBargeCapture();
    }
  }, [cancelBargeCapture, consumePendingResponse, ensureBargeMonitor, pendingResponse, setConversationStatus, speech]);

  const start = useCallback(async () => {
    if (!onTranscribeAudio) {
      setError("Voice input needs a speech-to-text connector.");
      onFatalError?.();
      return;
    }
    setMuted(false);
    setError(null);
    stopSpeech();
    consumePendingResponse();
    awaitingResponseRef.current = false;
    pendingStartRef.current = true;
    await startListening();
  }, [consumePendingResponse, onFatalError, onTranscribeAudio, startListening, stopSpeech]);

  const end = useCallback(async () => {
    pendingStartRef.current = false;
    clearTurnTimeout();
    stopSpeech();
    cancelBargeCapture();
    handle.cancel();
    turnClosingRef.current = false;
    awaitingResponseRef.current = false;
    consumePendingResponse();
    setMuted(false);
    setConversationStatus("idle");
  }, [cancelBargeCapture, clearTurnTimeout, consumePendingResponse, handle, setConversationStatus, stopSpeech]);

  const stopTurn = useCallback(() => {
    if (statusRef.current === "listening") void handleTurn(true);
  }, [handleTurn]);

  const toggleMute = useCallback(() => {
    setMuted((value) => {
      const next = !value;
      if (next) {
        clearTurnTimeout();
        cancelBargeCapture();
        handle.cancel();
        setConversationStatus("idle");
      } else if (enabledRef.current && !busyRef.current && statusRef.current === "idle") {
        pendingStartRef.current = true;
      }
      return next;
    });
  }, [cancelBargeCapture, clearTurnTimeout, handle, setConversationStatus]);

  useEffect(() => {
    if (!enabled || muted) return;
    if (awaitingResponseRef.current && status !== "speaking") {
      // Keep the mic live during generation too. Hermes calls this the
      // full-duplex window: a spoken interruption must reach the same Stop
      // seam before a reply is available, not only after playback begins.
      if (status === "thinking" && busy && !bargeCapturePendingRef.current) {
        void ensureBargeMonitor();
      }
      const response = pendingResponse();
      if (response) {
        void speakPendingResponse();
        return;
      }
      if (!busy && status === "thinking" && !bargeCapturePendingRef.current) {
        awaitingResponseRef.current = false;
        pendingStartRef.current = true;
        setConversationStatus("idle");
        return;
      }
    }
    if (!busy && status === "idle" && pendingStartRef.current) void startListening();
  }, [busy, enabled, ensureBargeMonitor, muted, pendingResponse, speakPendingResponse, startListening, status, setConversationStatus]);

  useEffect(() => {
    if (enabled && !wasEnabledRef.current) void start();
    if (!enabled && wasEnabledRef.current) void end();
    wasEnabledRef.current = enabled;
  }, [enabled, end, start]);

  useEffect(() => () => {
    clearTurnTimeout();
    stopSpeech();
    handle.cancel();
  }, [clearTurnTimeout, handle, stopSpeech]);

  return { end, error, level, muted, start, status, stopTurn, toggleMute };
}
