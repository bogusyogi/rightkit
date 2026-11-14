import {
  type Dispatch,
  type SetStateAction,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { useSettings } from "../hooks/useSettings.js";
import { onboardingClient } from "./client.js";
import { chordDisplayKeyForEvent } from "./helpers.js";
import { isMacPttKeyEvent, isWindowsPttKeyEvent } from "./pttKey.js";
import type {
  MicCheckState,
  OS,
  PermissionState,
  ShortcutKind,
  Step,
} from "./types.js";

type Args = {
  micState: PermissionState;
  platform: OS;
  setError: Dispatch<SetStateAction<string>>;
  setPermissionStatus: Dispatch<SetStateAction<string>>;
  step: Step;
};

export function useOnboardingRecording({
  micState,
  platform,
  setError,
  setPermissionStatus,
  step,
}: Args) {
  const [micSignalSeen, setMicSignalSeen] = useState(false);
  // Authorized but capture dead — Continue must stay skippable (State B
  // "unavailability") even though micSignalSeen never flips.
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
  const { data: settings, get: getSetting, patch: patchSetting } = useSettings();
  const inputDevice = settings.input_device;

  useEffect(() => {
    // The live meter level renders inside MicMeter's own subscription — this
    // listener keeps only the threshold-crossing detection so a 12.5Hz event
    // stream no longer re-renders the wizard.
    const unlistenMicMeter = onboardingClient().on("micLevel", (payload) => {
      const level = Math.max(0, Math.min(1, Number(payload.level) || 0));
      if (level > 0.08) {
        micSignalSeenRef.current = true;
        setMicSignalSeen(true);
        setMicCheckState("verified");
      }
    });
    const unlistenMicMeterError = onboardingClient().on(
      "micMeterError",
      (payload) => {
        setMicCaptureUnavailable(true);
        const detail = String(payload.message ?? "capture stopped");
        setPermissionStatus(`Selected microphone stopped (${detail}). Restart microphone check.`);
      },
    );
    const unlistenMicMeterStatus = onboardingClient().on(
      "micMeterStatus",
      (payload) => {
        switch (payload.status) {
          case "reopening":
            setMicCheckState("listening");
            setPermissionStatus("Restarting selected microphone…");
            break;
          case "recovered":
            setMicCheckState("listening");
            setPermissionStatus("Selected microphone restarted. Speak once to confirm.");
            break;
          case "verified":
            setMicCheckState("verified");
            setPermissionStatus("Audio detected.");
            break;
          case "no_signal":
            setMicCheckState("no_signal");
            setPermissionStatus(
              "No signal was detected. Choose another input or retry; you can still continue.",
            );
            break;
          case "unavailable":
            setMicCaptureUnavailable(true);
            setMicCheckState("unavailable");
            setPermissionStatus(
              "Selected microphone is unavailable. Choose another input or check system settings.",
            );
            break;
        }
      },
    );
    const unlistenShortcut = onboardingClient().on("shortcut", (payload) => {
      const kind = payload.kind;
      if (kind !== "toggle" && kind !== "send" && kind !== "cancel") return;
      setShortcutSeen((current) => ({ ...current, [kind]: true }));
      setActiveShortcut(kind);
      window.setTimeout(
        () => setActiveShortcut((current) => (current === kind ? null : current)),
        450,
      );
    });
    const unlistenFn = onboardingClient().on("fn", (payload) => {
      setFnHeld(!!payload.down);
    });
    const unlistenPtt = onboardingClient().on("ptt", (payload) => {
      const down = !!payload.down;
      setCtrlHeld(down);
      if (down) {
        // Right Alt is represented by Windows as AltGr: synthetic left Ctrl,
        // then Right Alt. Native preview owns Right Alt, so discard only that
        // leaked Ctrl chip once selected-key evidence arrives.
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

  // biome-ignore lint/correctness/useExhaustiveDependencies: micProbeRevision is the retryMicCheck retrigger — bumping it must re-run this effect even though the body never reads it.
  useEffect(() => {
    if (step !== 3 || micState !== "ready") {
      if (step !== 3) {
        setMicCaptureUnavailable(false);
        setMicCheckState("idle");
        void onboardingClient().stopMicMeter().catch(() => undefined);
      }
      return;
    }
    setMicCaptureUnavailable(false);
    setMicCheckState("listening");
    setMicSignalSeen(false);
    micSignalSeenRef.current = false;
    void onboardingClient().startMicMeter(inputDevice).catch((e) => {
      setMicCaptureUnavailable(true);
      setMicCheckState("unavailable");
      setPermissionStatus(
        "Selected microphone is unavailable. Choose another input or check system settings, then retry.",
      );
      setError(String(e));
    });
    return () => {
      void onboardingClient().stopMicMeter().catch(() => undefined);
    };
  }, [inputDevice, micProbeRevision, micState, setError, setPermissionStatus, step]);

  const selectMicDevice = useCallback(
    async (value: string) => {
      await patchSetting("input_device", value);
    },
    [patchSetting],
  );

  const retryMicCheck = useCallback(() => {
    setMicProbeRevision((current) => current + 1);
  }, []);

  useEffect(() => {
    const shortcutTestStep = step === 4;
    if (!shortcutTestStep) {
      void onboardingClient().setShortcutsActive(false, false).catch(() => undefined);
      setPressedShortcutKeys([]);
      return;
    }
    void onboardingClient().setShortcutsActive(true, step === 4).catch((e) => {
      setError(String(e));
    });
    const pttKey =
      platform === "macos"
        ? (getSetting("mac_push_to_talk_key", "ctrl") as string) || "ctrl"
        : (getSetting("push_to_talk_key", "ralt") as string) || "ralt";
    const isPttEvent = (event: KeyboardEvent) =>
      platform === "macos"
        ? isMacPttKeyEvent(event.code, pttKey)
        : isWindowsPttKeyEvent(event.code, pttKey);
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
      const displayKey = chordDisplayKeyForEvent(event.key, platform);
      if (!displayKey) return;
      setPressedShortcutKeys((current) =>
        current.includes(displayKey) ? current : [...current, displayKey],
      );
    };
    const onKeyUp = (event: KeyboardEvent) => {
      if (isPttEvent(event)) {
        setCtrlHeld(false);
        if (platform === "windows") return;
      }
      const displayKey = chordDisplayKeyForEvent(event.key, platform);
      if (!displayKey) return;
      setPressedShortcutKeys((current) => current.filter((item) => item !== displayKey));
    };
    const pulseMiddleClick = () => {
      setMiddleClickPulse(true);
      setControlSeen((s) => (s.middle ? s : { ...s, middle: true }));
      if (middleClickTimeoutRef.current !== null)
        window.clearTimeout(middleClickTimeoutRef.current);
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
    const unlistenMiddle = onboardingClient().on("middleClick", () => pulseMiddleClick());
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
      void onboardingClient().setShortcutsActive(false, false).catch(() => undefined);
    };
  }, [getSetting, platform, setError, step]);

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
    inputDevice,
    retryMicCheck,
    selectMicDevice,
    shortcutSeen,
  };
}
