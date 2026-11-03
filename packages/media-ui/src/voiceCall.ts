import { useCallback, useEffect, useMemo, useReducer, useRef } from "react";

/**
 * Headless state for a voice-call bar. The host feeds it events from rightkit-speech-client (or any event source
 * with the same shape) and supplies the commands; this module never captures audio or recognises speech, so there
 * is no second capture path.
 */

/** What the call is doing. `muted` is a phase of the view, derived from `muted` and the activity (see voiceCallPhase). */
export type VoiceCallActivity =
  | "idle"
  | "connecting"
  | "listening"
  | "user-speaking"
  | "agent-thinking"
  | "agent-speaking"
  | "error"
  | "ended";

export type VoiceCallPhase = VoiceCallActivity | "muted";

/** Events the host reports. Partial and final transcript text share one event; `final` tells them apart. */
export type VoiceCallEvent =
  | { type: "connecting" }
  | { type: "listening" }
  | { type: "user-speaking" }
  | { type: "agent-thinking" }
  | { type: "agent-speaking" }
  | { type: "transcript"; speaker: "user" | "agent"; text: string; final: boolean }
  | { type: "error"; message: string }
  | { type: "ended" };

/** Subscribes to call events and returns the unsubscribe function. Pass a stable function. */
export type VoiceCallEventSource = (listener: (event: VoiceCallEvent) => void) => () => void;

/** Side effects the host performs. The bar updates its own state optimistically, then calls these. */
export interface VoiceCallCommands {
  setMuted(muted: boolean): void;
  hangUp(): void;
  /** Optional: switch the input device. The host owns capture. */
  selectInput?(deviceId: string): void;
}

export interface VoiceCallLine {
  id: number;
  speaker: "user" | "agent";
  text: string;
  final: boolean;
}

export interface VoiceCallState {
  activity: VoiceCallActivity;
  muted: boolean;
  lines: readonly VoiceCallLine[];
  error: string | null;
  inputDeviceId: string | null;
}

/** Oldest transcript lines drop once this many are held. */
export const MAX_VOICE_CALL_LINES = 200;

export type VoiceCallAction =
  | { type: "event"; event: VoiceCallEvent }
  | { type: "muted"; muted: boolean }
  | { type: "input"; deviceId: string | null };

export function initialVoiceCallState(): VoiceCallState {
  return { activity: "idle", muted: false, lines: [], error: null, inputDeviceId: null };
}

/** The phase the view shows: a muted call reads as muted while the user side is listening or speaking. */
export function voiceCallPhase(state: VoiceCallState): VoiceCallPhase {
  if (state.muted && (state.activity === "listening" || state.activity === "user-speaking")) return "muted";
  return state.activity;
}

/**
 * A partial line replaces the last unfinished line of the same speaker; a final line closes it. Anything else
 * starts a new line. The list keeps the newest MAX_VOICE_CALL_LINES.
 */
function applyTranscript(
  lines: readonly VoiceCallLine[],
  speaker: "user" | "agent",
  text: string,
  final: boolean,
): readonly VoiceCallLine[] {
  const last = lines[lines.length - 1];
  let next: VoiceCallLine[];
  if (last && last.speaker === speaker && !last.final) {
    next = [...lines.slice(0, -1), { id: last.id, speaker, text, final }];
  } else {
    next = [...lines, { id: (last?.id ?? 0) + 1, speaker, text, final }];
  }
  return next.length > MAX_VOICE_CALL_LINES ? next.slice(next.length - MAX_VOICE_CALL_LINES) : next;
}

export function reduceVoiceCall(state: VoiceCallState, action: VoiceCallAction): VoiceCallState {
  if (action.type === "muted") return state.muted === action.muted ? state : { ...state, muted: action.muted };
  if (action.type === "input") return { ...state, inputDeviceId: action.deviceId };
  const event = action.event;
  switch (event.type) {
    case "connecting":
    case "listening":
    case "user-speaking":
    case "agent-thinking":
    case "agent-speaking":
      return { ...state, activity: event.type, error: null };
    case "transcript":
      return { ...state, lines: applyTranscript(state.lines, event.speaker, event.text, event.final) };
    case "error":
      return { ...state, activity: "error", error: event.message };
    case "ended":
      return { ...state, activity: "ended" };
  }
}

export interface VoiceCallController {
  state: VoiceCallState;
  phase: VoiceCallPhase;
  setMuted(muted: boolean): void;
  toggleMute(): void;
  /** Sends hang-up and shows the call as ended at once; the host may still report `ended` afterwards. */
  hangUp(): void;
  selectInput(deviceId: string): void;
}

/** Headless hook: subscribes to the event source, keeps the call state and exposes the controls. */
export function useVoiceCall(options: { events: VoiceCallEventSource; commands: VoiceCallCommands }): VoiceCallController {
  const [state, dispatch] = useReducer(reduceVoiceCall, undefined, initialVoiceCallState);
  const commandsRef = useRef(options.commands);
  useEffect(() => {
    commandsRef.current = options.commands;
  });
  const { events } = options;
  useEffect(() => events((event) => dispatch({ type: "event", event })), [events]);

  const setMuted = useCallback((muted: boolean) => {
    dispatch({ type: "muted", muted });
    commandsRef.current.setMuted(muted);
  }, []);
  const toggleMute = useCallback(() => setMuted(!state.muted), [setMuted, state.muted]);
  const hangUp = useCallback(() => {
    commandsRef.current.hangUp();
    dispatch({ type: "event", event: { type: "ended" } });
  }, []);
  const selectInput = useCallback((deviceId: string) => {
    dispatch({ type: "input", deviceId });
    commandsRef.current.selectInput?.(deviceId);
  }, []);

  return useMemo(
    () => ({ state, phase: voiceCallPhase(state), setMuted, toggleMute, hangUp, selectInput }),
    [state, setMuted, toggleMute, hangUp, selectInput],
  );
}
