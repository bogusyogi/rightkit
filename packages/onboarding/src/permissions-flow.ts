// The setup permissions state machine: Accessibility (macOS), the local-database key (macOS Keychain) and the
// microphone, with polling, a stale-read guard and the eight Windows microphone states. The OS commands are a
// port and every sentence is `copy`; the rows that render this are the host's. Imports only `react`.
import { type Dispatch, type SetStateAction, useCallback, useEffect, useRef, useState } from "react";
import type { PermissionState } from "./types.js";

export type PermissionBusy = "accessibility" | "keychain" | "mic" | null;

// The frame renders only the active step, so these hooks remount on every visit (Back and forward). State a
// step has already settled lives here, once per window, so a revisit neither shows a finished Keychain row as
// "Allow" again nor re-requests the microphone (which reopens Windows Settings).
let keychainReadyThisProcess = false;
let micAutoRequestFiredThisProcess = false;

/**
 * Windows distinguishes eight microphone states end to end. The shared badge has five, so this carries the
 * full classification the copy and action helpers key off. macOS never returns any of these.
 */
export type WindowsMicStatus =
  | "authorized"
  | "not_determined"
  | "denied_by_user"
  | "denied_by_system"
  | "identity_unavailable"
  | "device_unavailable"
  | "capture_failed"
  | "unknown";

const WINDOWS_MIC_STATUSES: readonly WindowsMicStatus[] = [
  "authorized",
  "not_determined",
  "denied_by_user",
  "denied_by_system",
  "identity_unavailable",
  "device_unavailable",
  "capture_failed",
  "unknown",
];

export function isWindowsMicStatus(value: string): value is WindowsMicStatus {
  return (WINDOWS_MIC_STATUSES as readonly string[]).includes(value);
}

/**
 * Maps a Windows microphone status onto the shared five-value badge state. `denied_by_system` maps to
 * `restricted` (a policy or MDM block: "ask your administrator", never the self-serve "open settings" action).
 * The hardware and technical states map to `needed`: none of them is a permission denial, so they never render
 * with the "blocked" styling of a real denial.
 */
export function windowsMicPermissionState(status: WindowsMicStatus): PermissionState {
  switch (status) {
    case "authorized":
      return "ready";
    case "denied_by_user":
      return "denied";
    case "denied_by_system":
      return "restricted";
    case "not_determined":
      return "waiting";
    case "identity_unavailable":
    case "device_unavailable":
    case "capture_failed":
    case "unknown":
      return "needed";
  }
}

/** The OS operations the flow needs. Each is the host's command. */
export interface PermissionsPort {
  /** Whether Accessibility is granted; never prompts. */
  accessibilityTrusted(): Promise<boolean>;
  /** Starts the shortcut listeners once Accessibility is granted. */
  activateShortcuts(): Promise<unknown>;
  openAccessibilitySettings(): Promise<unknown>;
  /** macOS: `authorized`, `denied`, `restricted` or another value. Windows: a `WindowsMicStatus`. */
  micStatus(): Promise<string>;
  /** Asks for microphone access; resolves the same values as `micStatus`. */
  requestMic(): Promise<string>;
  openMicSettings(): Promise<unknown>;
  /** Whether the database key is already stored; never creates it. */
  keychainPresent(): Promise<boolean>;
  /** Creates or reads the database key (this is the user's "Allow"). */
  ensureKeychain(): Promise<unknown>;
  /** Subscribes to the native "microphone granted" event; resolves the unsubscribe. */
  onMicGranted(handler: () => void): Promise<() => void>;
}

/** Every sentence the flow shows. The host supplies them (product name and platform wording included). */
export interface PermissionsCopy {
  keychainTitle: string;
  keychainCopy: string;
  accessibilityCopy: string;
  /** Windows microphone status text. */
  windowsMic: Record<WindowsMicStatus, string>;
  actions: {
    /** Accessibility, not ready, was already opened (waiting). */
    checkAgain: string;
    /** Accessibility on macOS, microphone settings. */
    openSettings: string;
    /** Accessibility off macOS. */
    check: string;
    /** Windows microphone, hardware missing. */
    micCheckAgain: string;
    /** macOS microphone, denied: opens settings; Windows: re-check. */
    micCheckSettings: string;
    micCheck: string;
    micRequest: string;
    keychainAllow: string;
    keychainWaiting: string;
    accessibilityChecking: string;
    accessibilityOpening: string;
  };
  status: {
    micReady: string;
    shortcutsReady: string;
    accessibilityChecking: string;
    accessibilityOpening: string;
    accessibilityReady: string;
    accessibilityReopened: string;
    accessibilityTurnOn: string;
    micAnswerDialog: string;
    keychainReady: (title: string) => string;
  };
  errors: {
    micRestricted: string;
    micDenied: string;
    micUnknown: (result: string) => string;
  };
}

export interface UseOnboardingPermissionsArgs {
  mac: boolean;
  /** The microphone step is showing. */
  onMicStep: boolean;
  /** The shortcuts step is showing. */
  onShortcutsStep: boolean;
  setError: Dispatch<SetStateAction<string>>;
}

/** Poll interval while a permission step shows. */
const POLL_MS = 1500;

export function useOnboardingPermissions(
  port: PermissionsPort,
  copy: PermissionsCopy,
  { mac, onMicStep, onShortcutsStep, setError }: UseOnboardingPermissionsArgs,
) {
  const portRef = useRef(port);
  portRef.current = port;
  const copyRef = useRef(copy);
  copyRef.current = copy;
  const onPermissionStep = onMicStep || onShortcutsStep;

  const [permissionStatus, setPermissionStatus] = useState("");
  const [accessibilityState, setAccessibilityState] = useState<PermissionState>(mac ? "needed" : "ready");
  const [keychainState, setKeychainState] = useState<PermissionState>(
    mac && !keychainReadyThisProcess ? "needed" : "ready",
  );
  const [micState, setMicState] = useState<PermissionState>(mac ? "needed" : "waiting");
  // Full-fidelity Windows classification behind `micState`: drives the state-specific copy and action label.
  // Stays null on macOS and before the first Windows status read resolves.
  const [windowsMicStatus, setWindowsMicStatus] = useState<WindowsMicStatus | null>(null);
  const [permissionBusy, setPermissionBusy] = useState<PermissionBusy>(null);
  // Monotonic sequence guard: every mic-status read (poll, focus recheck, or an explicit `runMicFlow`
  // request) claims the next number before it awaits. If a newer read starts and resolves first (for example
  // an explicit check lands a fresh `denied` while an older background poll is still in flight), the older
  // resolution is discarded on arrival instead of overwriting the newer, more authoritative state.
  const micRequestSeqRef = useRef(0);

  const refreshPermissionState = useCallback(async () => {
    if (mac) {
      await portRef.current
        .accessibilityTrusted()
        .then((trusted) => {
          if (trusted) {
            setAccessibilityState("ready");
            void portRef.current.activateShortcuts().catch(() => undefined);
          }
        })
        .catch(() => undefined);
    }
    if (mac) {
      const seq = ++micRequestSeqRef.current;
      await portRef.current
        .micStatus()
        .then((status) => {
          if (micRequestSeqRef.current !== seq) return;
          if (status === "authorized") setMicState("ready");
          if (status === "denied") setMicState("denied");
          if (status === "restricted") setMicState("restricted");
        })
        .catch(() => undefined);
      return;
    }
    if (micState === "ready") return;
    // Background recheck on the poll and window-focus triggers below: pick up an out-of-band grant (the user
    // flipped the toggle in the settings page this hook already opened) without waiting for another click.
    const seq = ++micRequestSeqRef.current;
    await portRef.current
      .micStatus()
      .then((status) => {
        if (micRequestSeqRef.current !== seq) return;
        if (!isWindowsMicStatus(status)) return;
        setWindowsMicStatus(status);
        setMicState(windowsMicPermissionState(status));
        if (status === "authorized") {
          setPermissionStatus(copyRef.current.status.micReady);
          setError("");
        }
      })
      .catch(() => undefined);
  }, [mac, micState, setError]);

  // A relaunch that resumes on the microphone step starts with the Keychain row at "Allow" although the key is
  // already stored. Read the stored state once (never create the key: that stays the user's "Allow" click).
  useEffect(() => {
    if (!mac || !onMicStep || keychainReadyThisProcess) return;
    let live = true;
    portRef.current
      .keychainPresent()
      .then((present) => {
        if (!present) return;
        keychainReadyThisProcess = true;
        if (live) setKeychainState((state) => (state === "needed" ? "ready" : state));
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [mac, onMicStep]);

  useEffect(() => {
    if (!onPermissionStep) return;
    void refreshPermissionState();
    const interval = window.setInterval(() => {
      void refreshPermissionState();
    }, POLL_MS);
    const onFocus = () => void refreshPermissionState();
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      window.clearInterval(interval);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    };
  }, [refreshPermissionState, onPermissionStep]);

  // Windows has no completion callback for the microphone consent pane: the native side polls in the
  // background after opening it and emits this once the capture probe reports granted, so setup advances
  // without waiting for the next manual check.
  useEffect(() => {
    if (mac) return;
    if (!onPermissionStep) return;
    const unlisten = portRef.current.onMicGranted(() => {
      // Bump the sequence so any poll or request already in flight (started before this authoritative event
      // landed) is dropped instead of overwriting this state when it resolves.
      micRequestSeqRef.current += 1;
      setWindowsMicStatus("authorized");
      setMicState("ready");
      setPermissionStatus(copyRef.current.status.micReady);
      setError("");
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
  }, [mac, onPermissionStep, setError]);

  const keychainTitle = copy.keychainTitle;
  const keychainCopy = copy.keychainCopy;
  const accessibilityCopy = copy.accessibilityCopy;
  const accessibilityAction =
    accessibilityState !== "ready"
      ? accessibilityState === "waiting"
        ? copy.actions.checkAgain
        : mac
          ? copy.actions.openSettings
          : copy.actions.check
      : undefined;
  const accessibilityBusyLabel =
    permissionBusy === "accessibility"
      ? accessibilityState === "waiting"
        ? copy.actions.accessibilityChecking
        : copy.actions.accessibilityOpening
      : accessibilityAction;
  // The button label is only shown after the auto-request resolves to a non-ready, non-restricted outcome.
  // The post-click `permissionBusy === "mic"` state is a "Waiting" affordance the step renders itself, not a
  // button label.
  const windowsMicAction = (status: WindowsMicStatus): string | undefined => {
    switch (status) {
      case "authorized":
        return undefined;
      case "denied_by_system":
        // Mirrors the macOS `restricted` badge: no self-serve action; the copy tells the user to contact
        // their administrator.
        return undefined;
      case "device_unavailable":
        // Nothing to fix in settings: it only re-probes the hardware.
        return copy.actions.micCheckAgain;
      case "denied_by_user":
      case "not_determined":
      case "identity_unavailable":
      case "capture_failed":
      case "unknown":
        return copy.actions.openSettings;
    }
  };
  const micAction =
    micState === "ready"
      ? undefined
      : !mac && windowsMicStatus
        ? windowsMicAction(windowsMicStatus)
        : micState === "restricted"
          ? undefined
          : micState === "denied"
            ? mac
              ? copy.actions.openSettings
              : copy.actions.micCheckSettings
            : mac
              ? copy.actions.micCheck
              : copy.actions.micRequest;
  const keychainAction =
    keychainState === "ready" ? undefined : keychainState === "waiting" ? copy.actions.keychainWaiting : copy.actions.keychainAllow;

  async function runAccessibilityFlow() {
    if (!mac) {
      setAccessibilityState("ready");
      setPermissionStatus(copy.status.shortcutsReady);
      return;
    }
    setError("");
    setPermissionBusy("accessibility");
    setPermissionStatus(
      accessibilityState === "waiting" ? copy.status.accessibilityChecking : copy.status.accessibilityOpening,
    );
    try {
      const trusted = await port.accessibilityTrusted();
      if (trusted) {
        setAccessibilityState("ready");
        await port.activateShortcuts().catch(() => undefined);
        setPermissionStatus(copy.status.accessibilityReady);
        return;
      }
      if (accessibilityState === "waiting") {
        const refreshed = await port.accessibilityTrusted();
        if (refreshed) {
          setAccessibilityState("ready");
          await port.activateShortcuts().catch(() => undefined);
          setPermissionStatus(copy.status.accessibilityReady);
          return;
        }
        // Re-open the settings pane. If the user closed the window without granting, a bare re-check left
        // them stuck with no way back to it: "check again" must reopen it (idempotent: refocuses if open).
        await port.openAccessibilitySettings().catch(() => undefined);
        setPermissionStatus(copy.status.accessibilityReopened);
        return;
      }
      await port.openAccessibilitySettings();
      setAccessibilityState("waiting");
      setPermissionStatus(copy.status.accessibilityTurnOn);
    } catch (e) {
      setError(String(e));
      setAccessibilityState("needed");
    } finally {
      setPermissionBusy(null);
    }
  }

  async function runKeychainFlow() {
    setError("");
    setPermissionBusy("keychain");
    setKeychainState("waiting");
    try {
      await port.ensureKeychain();
      keychainReadyThisProcess = true;
      setKeychainState("ready");
      setPermissionStatus(copy.status.keychainReady(keychainTitle));
    } catch (e) {
      setKeychainState("denied");
      setError(String(e));
    } finally {
      setPermissionBusy(null);
    }
  }

  const runMicFlow = useCallback(async () => {
    const text = copyRef.current;
    setError("");
    setPermissionBusy("mic");
    setMicState("waiting");
    const seq = ++micRequestSeqRef.current;
    try {
      const result = await portRef.current.requestMic();
      if (micRequestSeqRef.current !== seq) return;
      if (mac) {
        if (result === "authorized") {
          setMicState("ready");
          setPermissionStatus(text.status.micReady);
        } else if (result === "restricted") {
          // MDM or parental-control block: the user cannot grant this themselves, so do not send them to
          // settings like the `denied` path does.
          setMicState("restricted");
          setError(text.errors.micRestricted);
        } else if (result === "denied") {
          setMicState("denied");
          await portRef.current.openMicSettings().catch(() => undefined);
          setError(text.errors.micDenied);
        } else {
          setPermissionStatus(text.status.micAnswerDialog);
        }
        return;
      }
      // Windows: eight distinct states end to end, each with its own truthful copy and action. The status
      // and request commands share one classifier natively, so this is the single place both funnel through.
      if (!isWindowsMicStatus(result)) {
        setMicState("needed");
        setError(text.errors.micUnknown(result));
        return;
      }
      setWindowsMicStatus(result);
      setMicState(windowsMicPermissionState(result));
      if (result === "authorized") {
        setPermissionStatus(text.status.micReady);
        setError("");
      } else if (
        result === "denied_by_user" ||
        result === "denied_by_system" ||
        result === "device_unavailable" ||
        result === "capture_failed"
      ) {
        // Genuinely blocking states: surfaced as an error like the macOS `denied` and `restricted` branches,
        // not a transient status line.
        setError(text.windowsMic[result]);
      } else {
        setPermissionStatus(text.windowsMic[result]);
      }
    } catch (e) {
      if (micRequestSeqRef.current !== seq) return;
      setMicState("needed");
      setWindowsMicStatus(null);
      setError(String(e));
    } finally {
      setPermissionBusy(null);
    }
  }, [mac, setError]);

  // Auto-request the microphone on entering the step so setup can never hard-stick a user behind a button
  // click. The module-level guard prevents a re-fire when the user steps back and forward again after the
  // permission has already been resolved. On macOS the initial state is "needed": wait for the explicit
  // button there, the OS prompt needs the focused app.
  useEffect(() => {
    if (mac) return;
    if (!onMicStep) return;
    if (micAutoRequestFiredThisProcess) return;
    if (micState !== "waiting") return;
    if (permissionBusy !== null) return;
    micAutoRequestFiredThisProcess = true;
    void runMicFlow();
  }, [mac, onMicStep, micState, permissionBusy, runMicFlow]);

  return {
    accessibilityAction,
    accessibilityBusyLabel,
    accessibilityCopy,
    accessibilityState,
    keychainAction,
    keychainCopy,
    keychainState,
    keychainTitle,
    micAction,
    micState,
    // Full-fidelity Windows status (null on macOS and before the first read) and its truthful copy.
    windowsMicStatus,
    windowsMicStatusCopy: windowsMicStatus ? copy.windowsMic[windowsMicStatus] : undefined,
    permissionBusy,
    permissionStatus,
    runAccessibilityFlow,
    runKeychainFlow,
    runMicFlow,
    setMicState,
    setPermissionStatus,
  };
}
