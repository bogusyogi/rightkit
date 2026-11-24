// The setup microphone check and shortcut preview: native level and status events become a check state, and
// while the shortcuts step shows, key and mouse events become the chord the user is holding. The commands,
// event names, key matching and every sentence are inputs. Imports only `react`.
import { type Dispatch, type SetStateAction, useCallback, useEffect, useRef, useState } from "react";
import type { PermissionState } from "./types.js";
import { platformPttMatcher, resolvePttKey } from "./ptt.js";
import type { PlatformPttMatchers, ResolvePttKeyConfig } from "./ptt.js";

export type MicCheckState = "idle" | "listening" | "verified" | "no_signal" | "unavailable";
export type ShortcutKind = "toggle" | "send" | "cancel";

/** The native commands and event subscription the step needs. */
export interface SetupRecordingPort {
  startMicMeter(inputDevice: string | undefined): Promise<unknown>;
  stopMicMeter(): Promise<unknown>;
  setShortcutsActive(active: boolean, previewOnly: boolean): Promise<unknown>;
  /** Subscribes to a native event; resolves the unsubscribe. */
  listen<T>(event: string, handler: (payload: T) => void): Promise<() => void>;
}

/** Native event names. */
export interface SetupRecordingEvents {
  /** Payload `{ level?: number }`, 0 to 1. */
  micLevel: string;
  /** Payload `{ message?: string }`. */
  micMeterError: string;
  /** Payload `{ status?: "reopening" | "recovered" | "verified" | "no_signal" | "unavailable" }`. */
  micMeterStatus: string;
  /** Payload `{ kind?: ShortcutKind }`. */
  shortcut: string;
  /** Payload `{ down?: boolean }`. */
  fn: string;
  /** Payload `{ down?: boolean }`. */
  ptt: string;
  middleClick: string;
}

export interface SetupRecordingCopy {
  /** Shown with the error detail, e.g. "Selected microphone stopped (detail). Restart microphone check." */
  captureStopped: (detail: string) => string;
  /** Detail used when the event has no message. */
  captureStoppedDefaultDetail: string;
  reopening: string;
  recovered: string;
  verified: string;
  noSignal: string;
  unavailable: string;
  /** After the meter failed to start. */
  unavailableRetry: string;
}

export interface UseSetupRecordingArgs {
  micState: PermissionState;
  /** `"macos"`, `"windows"`, ... Only `"windows"` treats a leaked left Ctrl specially. */
  platform: string;
  /** The microphone step is showing. */
  onMicStep: boolean;
  /** The shortcuts step is showing (the preview runs only then). */
  onShortcutsStep: boolean;
  /** The saved input device, passed to the meter. */
  inputDevice: string | undefined;
  /** Stores a chosen input device. */
  patchInputDevice: (value: string) => Promise<unknown>;
  /** The push-to-talk key setting for this platform. Optional when `pttSettings` resolves it. */
  pttKey?: string;
  /** True when the keyboard event is the push-to-talk key. Optional when `pttMatchers` picks it by platform. */
  isPttKeyEvent?: (event: KeyboardEvent, pttKey: string) => boolean;
  /** Resolves `pttKey` from the host's settings by platform (see `resolvePttKey`). Used when `pttKey` is not passed. */
  pttSettings?: ResolvePttKeyConfig;
  /** Picks the matcher by platform (see `platformPttMatcher`). Used when `isPttKeyEvent` is not passed. */
  pttMatchers?: PlatformPttMatchers;
  /** The chip name of a pressed key, or null when it is not one to show. */
  chordKeyForEvent: (key: string) => string | null;
  setError: Dispatch<SetStateAction<string>>;
  setPermissionStatus: Dispatch<SetStateAction<string>>;
}

/** The meter level above which a signal counts as heard. */
const SIGNAL_LEVEL = 0.08;

export function useSetupRecording(
  port: SetupRecordingPort,
  events: SetupRecordingEvents,
  copy: SetupRecordingCopy,
  args: UseSetupRecordingArgs,
) {
  const {
    micState,
    platform,
    onMicStep,
    onShortcutsStep,
    inputDevice,
    patchInputDevice,
    chordKeyForEvent,
    setError,
    setPermissionStatus,
  } = args;
  const pttKey =
    args.pttKey ?? (args.pttSettings ? resolvePttKey(platform, args.pttSettings) : "");
  const isPttKeyEvent =
    args.isPttKeyEvent ??
    (args.pttMatchers ? platformPttMatcher(platform, args.pttMatchers) : () => false);
  const portRef = useRef(port);
  portRef.current = port;
  const eventsRef = useRef(events);
  eventsRef.current = events;
  const copyRef = useRef(copy);
  copyRef.current = copy;
  const isPttRef = useRef(isPttKeyEvent);
  isPttRef.current = isPttKeyEvent;
  const chordKeyRef = useRef(chordKeyForEvent);
  chordKeyRef.current = chordKeyForEvent;

  const [micSignalSeen, setMicSignalSeen] = useState(false);
  // Authorized but capture dead: Continue must stay skippable even though micSignalSeen never flips.
  const [micCaptureUnavailable, setMicCaptureUnavailable] = useState(false);
  const [micCheckState, setMicCheckState] = useState<MicCheckState>("idle");
  const [micProbeRevision, setMicProbeRevision] = useState(0);
  const [controlSeen, setControlSeen] = useState({ ptt: false, middle: false });
  const [shortcutSeen, setShortcutSeen] = useState<Record<ShortcutKind, boolean>>({
    toggle: false,
    send: false,
    cancel: false,
  });
  const [activeShortcut, setActiveShortcut] = useState<ShortcutKind | null>(null);
  const [pressedShortcutKeys, setPressedShortcutKeys] = useState<string[]>([]);
  const [fnHeld, setFnHeld] = useState(false);
  const [ctrlHeld, setCtrlHeld] = useState(false);
  const [middleClickPulse, setMiddleClickPulse] = useState(false);
  const middleClickTimeoutRef = useRef<number | null>(null);
  const micSignalSeenRef = useRef(false);

  useEffect(() => {
    const { listen } = portRef.current;
    const names = eventsRef.current;
    // The live meter level renders inside the host's own subscription: this listener keeps only the
    // threshold-crossing detection so a 12.5 Hz event stream no longer re-renders the wizard.
    const unlistenMicMeter = listen<{ level?: number }>(names.micLevel, (payload) => {
      const level = Math.max(0, Math.min(1, Number(payload.level) || 0));
      if (level > SIGNAL_LEVEL) {
        micSignalSeenRef.current = true;
        setMicSignalSeen(true);
        setMicCheckState("verified");
      }
    });
    const unlistenMicMeterError = listen<{ message?: string }>(names.micMeterError, (payload) => {
      setMicCaptureUnavailable(true);
      const detail = String(payload.message ?? copyRef.current.captureStoppedDefaultDetail);
      setPermissionStatus(copyRef.current.captureStopped(detail));
    });
    const unlistenMicMeterStatus = listen<{ status?: string }>(names.micMeterStatus, (payload) => {
      const text = copyRef.current;
      switch (payload.status) {
        case "reopening":
          setMicCheckState("listening");
          setPermissionStatus(text.reopening);
          break;
        case "recovered":
          setMicCheckState("listening");
          setPermissionStatus(text.recovered);
          break;
        case "verified":
          setMicCheckState("verified");
          setPermissionStatus(text.verified);
          break;
        case "no_signal":
          setMicCheckState("no_signal");
          setPermissionStatus(text.noSignal);
          break;
        case "unavailable":
          setMicCaptureUnavailable(true);
          setMicCheckState("unavailable");
          setPermissionStatus(text.unavailable);
          break;
      }
    });
    const unlistenShortcut = listen<{ kind?: ShortcutKind }>(names.shortcut, (payload) => {
      const kind = payload.kind;
      if (kind !== "toggle" && kind !== "send" && kind !== "cancel") return;
      setShortcutSeen((current) => ({ ...current, [kind]: true }));
      setActiveShortcut(kind);
      window.setTimeout(() => setActiveShortcut((current) => (current === kind ? null : current)), 450);
    });
    const unlistenFn = listen<{ down?: boolean }>(names.fn, (payload) => {
      setFnHeld(!!payload.down);
    });
    const unlistenPtt = listen<{ down?: boolean }>(names.ptt, (payload) => {
      const down = !!payload.down;
      setCtrlHeld(down);
      if (down) {
        // Right Alt is represented by Windows as AltGr: synthetic left Ctrl, then Right Alt. The native
        // preview owns Right Alt, so discard only that leaked Ctrl chip once selected-key evidence arrives.
        setPressedShortcutKeys((current) => current.filter((key) => key !== "Ctrl"));
        setControlSeen((current) => (current.ptt ? current : { ...current, ptt: true }));
      }
    });
    return () => {
      void unlistenMicMeter.then((fn) => fn());
      void unlistenMicMeterError.then((fn) => fn());
      void unlistenMicMeterStatus.then((fn) => fn());
      void unlistenShortcut.then((fn) => fn());
      void unlistenFn.then((fn) => fn());
      void unlistenPtt.then((fn) => fn());
    };
  }, [setPermissionStatus]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: micProbeRevision is the retryMicCheck retrigger: bumping it must re-run this effect even though the body never reads it.
  useEffect(() => {
    if (!onMicStep || micState !== "ready") {
      if (!onMicStep) {
        setMicCaptureUnavailable(false);
        setMicCheckState("idle");
        void portRef.current.stopMicMeter().catch(() => undefined);
      }
      return;
    }
    setMicCaptureUnavailable(false);
    setMicCheckState("listening");
    setMicSignalSeen(false);
    micSignalSeenRef.current = false;
    void portRef.current.startMicMeter(inputDevice).catch((e) => {
      setMicCaptureUnavailable(true);
      setMicCheckState("unavailable");
      setPermissionStatus(copyRef.current.unavailableRetry);
      setError(String(e));
    });
    return () => {
      void portRef.current.stopMicMeter().catch(() => undefined);
    };
  }, [inputDevice, micProbeRevision, micState, setError, setPermissionStatus, onMicStep]);

  const selectMicDevice = useCallback(
    async (value: string) => {
      await patchInputDevice(value);
    },
    [patchInputDevice],
  );

  const retryMicCheck = useCallback(() => {
    setMicProbeRevision((current) => current + 1);
  }, []);

  useEffect(() => {
    if (!onShortcutsStep) {
      void portRef.current.setShortcutsActive(false, false).catch(() => undefined);
      setPressedShortcutKeys([]);
      return;
    }
    void portRef.current.setShortcutsActive(true, true).catch((e) => {
      setError(String(e));
    });
    const isPttEvent = (event: KeyboardEvent) => isPttRef.current(event, pttKey);
    const onKeyDown = (event: KeyboardEvent) => {
      if (isPttEvent(event)) {
        setCtrlHeld(true);
        setControlSeen((s) => (s.ptt ? s : { ...s, ptt: true }));
        if (platform === "windows") {
          // AltGr may emit a synthetic left Ctrl before AltRight.
          setPressedShortcutKeys((current) => current.filter((key) => key !== "Ctrl"));
          return;
        }
      }
      const displayKey = chordKeyRef.current(event.key);
      if (!displayKey) return;
      setPressedShortcutKeys((current) => (current.includes(displayKey) ? current : [...current, displayKey]));
    };
    const onKeyUp = (event: KeyboardEvent) => {
      if (isPttEvent(event)) {
        setCtrlHeld(false);
        if (platform === "windows") return;
      }
      const displayKey = chordKeyRef.current(event.key);
      if (!displayKey) return;
      setPressedShortcutKeys((current) => current.filter((item) => item !== displayKey));
    };
    const pulseMiddleClick = () => {
      setMiddleClickPulse(true);
      setControlSeen((s) => (s.middle ? s : { ...s, middle: true }));
      if (middleClickTimeoutRef.current !== null) window.clearTimeout(middleClickTimeoutRef.current);
      middleClickTimeoutRef.current = window.setTimeout(() => setMiddleClickPulse(false), 600);
    };
    const onMouseDown = (event: MouseEvent) => {
      if (event.button === 1) pulseMiddleClick();
    };
    const clear = () => {
      setPressedShortcutKeys([]);
      setCtrlHeld(false);
    };
    window.addEventListener("keydown", onKeyDown);
    window.addEventListener("keyup", onKeyUp);
    window.addEventListener("mousedown", onMouseDown);
    window.addEventListener("blur", clear);
    const unlistenMiddle = portRef.current.listen(eventsRef.current.middleClick, () => pulseMiddleClick());
    return () => {
      window.removeEventListener("keydown", onKeyDown);
      window.removeEventListener("keyup", onKeyUp);
      window.removeEventListener("mousedown", onMouseDown);
      window.removeEventListener("blur", clear);
      void unlistenMiddle.then((fn) => fn()).catch(() => undefined);
      if (middleClickTimeoutRef.current !== null) {
        window.clearTimeout(middleClickTimeoutRef.current);
        middleClickTimeoutRef.current = null;
      }
      setPressedShortcutKeys([]);
      setCtrlHeld(false);
      setMiddleClickPulse(false);
      void portRef.current.setShortcutsActive(false, false).catch(() => undefined);
    };
  }, [onShortcutsStep, platform, pttKey, setError]);

  const liveChordKeys = fnHeld ? [...pressedShortcutKeys, "fn"] : pressedShortcutKeys;

  return {
    activeShortcut,
    controlSeen,
    ctrlHeld,
    liveChordKeys,
    micCaptureUnavailable,
    micCheckState,
    micRefreshToken: micProbeRevision,
    micSignalSeen,
    middleClickPulse,
    /** The push-to-talk key in effect (passed, or resolved from `pttSettings`), for the controls step to show. */
    pttKey,
    inputDevice,
    retryMicCheck,
    selectMicDevice,
    shortcutSeen,
  };
}
