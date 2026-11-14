import {
  type Dispatch,
  type SetStateAction,
  useCallback,
  useEffect,
  useRef,
  useState,
} from "react";
import { hostAppName } from "../host.js";
import { accessibilityClient, onboardingClient } from "./client.js";
import type { PermissionState, Step } from "./types.js";

type PermissionBusy = "accessibility" | "keychain" | "mic" | null;

// The frame renders only the active step, so these hooks remount on every visit (Back and forward).
// State a step has already settled lives here, once per window, so a revisit neither shows a
// finished Keychain row as "Allow" again nor re-requests the microphone (which reopens Windows
// Settings).
let keychainReadyThisProcess = false;
let micAutoRequestFiredThisProcess = false;

// Windows distinguishes eight microphone states end-to-end (see
// `windows_microphone_state_from_capability` in
// src-tauri/src/commands/microphone.rs) — this app's
// `PermissionState` enum only has five buckets for the shared badge, so this
// type carries the full-fidelity Windows classification the copy/action
// helpers below key off of. macOS never returns any of these; its "denied" /
// "restricted" labels are handled entirely by the existing branches above.
type WindowsMicStatus =
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

function isWindowsMicStatus(value: string): value is WindowsMicStatus {
  return (WINDOWS_MIC_STATUSES as readonly string[]).includes(value);
}

/// Map a Windows microphone status onto the shared five-value badge state.
/// `denied_by_system` maps to `restricted` because Windows reports it for a
/// policy/MDM-controlled block, matching how macOS's own MDM-restricted
/// state (`restricted`) is rendered — same "ask your administrator" shape,
/// never the "Open Settings" self-serve action offered for `denied_by_user`.
/// The hardware/technical states (`identity_unavailable`,
/// `device_unavailable`, `capture_failed`, `unknown`) map to `needed` — none
/// of them is a permission denial, so they must never render with the same
/// "blocked" styling as an actual `denied` state.
function windowsMicPermissionState(status: WindowsMicStatus): PermissionState {
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

/// Truthful, plain-language copy for each Windows microphone status. Never
/// describes a hardware/technical gap as a permission denial, and never
/// promises a system prompt Windows will not show again once a user has
/// explicitly denied access.
function windowsMicStatusCopy(status: WindowsMicStatus): string {
  switch (status) {
    case "authorized":
      return "Microphone is ready.";
    case "denied_by_user":
      return "Microphone access is blocked. Windows Settings opened — turn it on under Privacy & security > Microphone > Let desktop apps access your microphone.";
    case "denied_by_system":
      return "Microphone access is turned off by a device or organization policy. Windows Settings opened — if the switch there is greyed out, ask your administrator.";
    case "not_determined":
      return "Windows hasn't decided on microphone access yet. Windows Settings opened — turn on “Let desktop apps access your microphone.” This page updates automatically.";
    case "identity_unavailable":
      return `Windows couldn't confirm ${hostAppName()}'s microphone permission just now — this isn't a denial. Windows Settings opened; try again in a moment.`;
    case "device_unavailable":
      return "No working microphone was found. Plug one in or choose a different input above, then check again.";
    case "capture_failed":
      return "The microphone couldn't be opened because of a device or driver error. Windows Settings opened in case access needs to be re-granted; you can also choose a different input above.";
    case "unknown":
      return "Couldn't determine microphone access. Windows Settings opened; try again.";
  }
}

/// The action button label for each Windows microphone status — always
/// describes what the click actually does, never a prompt Windows cannot
/// show. `device_unavailable` never offers "Open settings": there is
/// nothing there to fix, so it only re-probes the hardware.
function windowsMicActionLabel(status: WindowsMicStatus): string | undefined {
  switch (status) {
    case "authorized":
      return undefined;
    case "denied_by_system":
      // Mirrors the mac `restricted` badge: no self-serve action rendered,
      // the error copy above tells the user to contact their administrator.
      return undefined;
    case "device_unavailable":
      return "Check again";
    case "denied_by_user":
    case "not_determined":
    case "identity_unavailable":
    case "capture_failed":
    case "unknown":
      return "Open settings";
  }
}

type Args = {
  mac: boolean;
  step: Step;
  setError: Dispatch<SetStateAction<string>>;
};

export function useOnboardingPermissions({ mac, step, setError }: Args) {
  const [permissionStatus, setPermissionStatus] = useState("");
  const [accessibilityState, setAccessibilityState] = useState<PermissionState>(
    mac ? "needed" : "ready",
  );
  const [keychainState, setKeychainState] = useState<PermissionState>(
    mac && !keychainReadyThisProcess ? "needed" : "ready",
  );
  const [micState, setMicState] = useState<PermissionState>(mac ? "needed" : "waiting");
  // Full-fidelity Windows classification behind `micState`'s five-value
  // badge — drives the state-specific copy and action label below. Stays
  // null on macOS and before the first Windows status read resolves.
  const [windowsMicStatus, setWindowsMicStatus] = useState<WindowsMicStatus | null>(null);
  const [permissionBusy, setPermissionBusy] = useState<PermissionBusy>(null);
  // Auto-request mic once when the user enters step 3, so a stuck onboarding
  // never depends on the user clicking a "Request microphone" button. The
  // module-level flag guards re-entry (e.g. user steps back to 2 and forward
  // to 3 again, which remounts this hook) from firing a second request
  // against an already-resolved state.
  // Monotonic sequence guard: every mic-status read (poll, focus recheck, or
  // an explicit `runMicFlow` request) claims the next number before it
  // awaits. If a newer read starts and resolves first — e.g. the user's
  // explicit "Check settings" click lands a fresh `denied` while an older
  // background poll's `invoke` is still in flight — the older promise's
  // resolution is discarded on arrival instead of overwriting the newer,
  // more authoritative state. Without this, a denial set by the fast path
  // could be silently clobbered moments later by a stale "authorized"/
  // "not_determined" read that was already in flight when the deny landed.
  const micRequestSeqRef = useRef(0);

  const refreshPermissionState = useCallback(async () => {
    if (mac) {
      await accessibilityClient().granted(false)
        .then((trusted) => {
          if (trusted) {
            setAccessibilityState("ready");
            void accessibilityClient().activate().catch(() => undefined);
          }
        })
        .catch(() => undefined);
    }
    if (mac) {
      const seq = ++micRequestSeqRef.current;
      await onboardingClient().microphoneAccessStatus()
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
    // Background recheck on the 1.5s poll / window-focus triggers below: pick
    // up an out-of-band grant (e.g. the user flipped the toggle in the
    // Windows Settings page this hook already opened) without waiting for
    // another click.
    const seq = ++micRequestSeqRef.current;
    await onboardingClient().microphoneAccessStatus()
      .then((status) => {
        if (micRequestSeqRef.current !== seq) return;
        if (!isWindowsMicStatus(status)) return;
        setWindowsMicStatus(status);
        setMicState(windowsMicPermissionState(status));
        if (status === "authorized") {
          setPermissionStatus("Microphone is ready.");
          setError("");
        }
      })
      .catch(() => undefined);
  }, [mac, micState, setError]);

  // A relaunch that resumes on this step starts with the Keychain row at "Allow" although the key
  // is already stored. Read the stored state once (never create the key: that stays the user's
  // "Allow" click).
  useEffect(() => {
    if (!mac || step !== 3 || keychainReadyThisProcess) return;
    let live = true;
    onboardingClient().databaseKeyPresent()
      .then((present) => {
        if (!present) return;
        keychainReadyThisProcess = true;
        if (live) setKeychainState((state) => (state === "needed" ? "ready" : state));
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [mac, step]);

  useEffect(() => {
    if (step !== 3 && step !== 4) return;
    void refreshPermissionState();
    const interval = window.setInterval(() => {
      void refreshPermissionState();
    }, 1500);
    const onFocus = () => void refreshPermissionState();
    window.addEventListener("focus", onFocus);
    document.addEventListener("visibilitychange", onFocus);
    return () => {
      window.clearInterval(interval);
      window.removeEventListener("focus", onFocus);
      document.removeEventListener("visibilitychange", onFocus);
    };
  }, [refreshPermissionState, step]);

  // Windows has no completion callback for the mic consent settings pane —
  // the Rust side polls in the background after opening ms-settings and emits
  // this once the capture probe reports granted, so onboarding advances
  // without waiting for the next manual "Check again" click.
  useEffect(() => {
    if (mac) return;
    if (step !== 3 && step !== 4) return;
    const unlisten = onboardingClient().on("microphonePermissionGranted", () => {
      // Bump the sequence so any poll/request already in flight (started
      // before this authoritative event landed) is dropped instead of
      // overwriting this state when it resolves.
      micRequestSeqRef.current += 1;
      setWindowsMicStatus("authorized");
      setMicState("ready");
      setPermissionStatus("Microphone is ready.");
      setError("");
    });
    return () => {
      void unlisten.then((fn) => fn());
    };
    // setError is the useState setter from OnboardingImpl (stable identity across
    // renders), so adding it here cannot cause an extra re-subscribe/re-run.
  }, [mac, step, setError]);

  const keychainTitle = "Keychain";
  const keychainCopy =
    "The local database key stays in Keychain, so transcript history is protected without any account.";
  const accessibilityCopy =
    "Accessibility lets the app receive fn shortcuts and paste back into the app you were typing in.";
  const accessibilityAction =
    accessibilityState !== "ready"
      ? accessibilityState === "waiting"
        ? "Check again"
        : mac
          ? "Open settings"
          : "Check"
      : undefined;
  const accessibilityBusyLabel =
    permissionBusy === "accessibility"
      ? accessibilityState === "waiting"
        ? "Checking…"
        : "Opening…"
      : accessibilityAction;
  // The button label is only shown after auto-request resolves to a non-ready,
  // non-restricted outcome. The previous `"Opened"` branch (micState === "waiting"
  // on Windows) implied the prompt was already opened, but the UI never called
  // `request_microphone_access` until the user clicked — it was a fake state.
  // Auto-request is now wired on step-3 entry, so the "waiting" UI is the
  // post-click `permissionBusy === "mic"` affordance (rendered as "Waiting…"
  // by MicSetupStep), not a button label. Drop the branch.
  const micAction =
    micState === "ready"
      ? undefined
      : !mac && windowsMicStatus
        ? windowsMicActionLabel(windowsMicStatus)
        : micState === "restricted"
          ? undefined
          : micState === "denied"
            ? mac
              ? "Open settings"
              : "Check settings"
            : mac
              ? "Check mic"
              : "Request microphone";
  const keychainAction =
    keychainState === "ready" ? undefined : keychainState === "waiting" ? "Waiting…" : "Allow";

  async function runAccessibilityFlow() {
    if (!mac) {
      setAccessibilityState("ready");
      setPermissionStatus("Shortcuts are ready.");
      return;
    }
    setError("");
    setPermissionBusy("accessibility");
    setPermissionStatus(
      accessibilityState === "waiting"
        ? "Checking Accessibility access…"
        : "Opening Accessibility settings…",
    );
    try {
      const trusted = await accessibilityClient().granted(false);
      if (trusted) {
        setAccessibilityState("ready");
        await accessibilityClient().activate().catch(() => undefined);
        setPermissionStatus("Accessibility is ready.");
        return;
      }
      if (accessibilityState === "waiting") {
        const refreshed = await accessibilityClient().granted(false);
        if (refreshed) {
          setAccessibilityState("ready");
          await accessibilityClient().activate().catch(() => undefined);
          setPermissionStatus("Accessibility is ready.");
          return;
        }
        // Re-open the settings pane. If the user closed the System Settings window
        // without granting, a bare re-check left them stuck with no way back to it —
        // "Check again" must reopen it (idempotent: refocuses if already open).
        await accessibilityClient().openSettings().catch(() => undefined);
        setPermissionStatus(
          `Reopened Accessibility settings — turn ${hostAppName()} on there (toggle it off/on once if it's already listed). This page updates automatically.`,
        );
        return;
      }
      await accessibilityClient().openSettings();
      setAccessibilityState("waiting");
      setPermissionStatus(
        "Turn on Accessibility for the app. This page will update automatically.",
      );
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
      await onboardingClient().ensureDatabaseKey();
      keychainReadyThisProcess = true;
      setKeychainState("ready");
      setPermissionStatus(`${keychainTitle} is ready.`);
    } catch (e) {
      setKeychainState("denied");
      setError(String(e));
    } finally {
      setPermissionBusy(null);
    }
  }

  const runMicFlow = useCallback(async () => {
    setError("");
    setPermissionBusy("mic");
    setMicState("waiting");
    const seq = ++micRequestSeqRef.current;
    try {
      const result = await onboardingClient().requestMicrophoneAccess();
      if (micRequestSeqRef.current !== seq) return;
      if (mac) {
        if (result === "authorized") {
          setMicState("ready");
          setPermissionStatus("Microphone is ready.");
        } else if (result === "restricted") {
          // MDM/parental-control block — the user cannot grant this
          // themselves, so don't send them to System Settings like the
          // `denied` path does.
          setMicState("restricted");
          setError(
            "Microphone access is managed by your organization (MDM) and can't be enabled from this Mac. Ask your administrator.",
          );
        } else if (result === "denied") {
          setMicState("denied");
          await onboardingClient().openMicrophoneSettings().catch(() => undefined);
          setError(
            "Microphone access is blocked. Turn it on in System Settings > Privacy & Security > Microphone.",
          );
        } else {
          setPermissionStatus(
            "Answer the microphone permission dialog. This page will update automatically.",
          );
        }
        return;
      }
      // Windows: eight distinct states end-to-end, each with its own
      // truthful copy and action (see `windowsMicStatusCopy` /
      // `windowsMicActionLabel` above). `microphone_access_status` and
      // `request_microphone_access` share the same classifier on the Rust
      // side, so this is the single place both funnel through.
      if (!isWindowsMicStatus(result)) {
        setMicState("needed");
        setError(`Couldn't determine microphone access (${result}).`);
        return;
      }
      setWindowsMicStatus(result);
      setMicState(windowsMicPermissionState(result));
      if (result === "authorized") {
        setPermissionStatus("Microphone is ready.");
        setError("");
      } else if (
        result === "denied_by_user" ||
        result === "denied_by_system" ||
        result === "device_unavailable" ||
        result === "capture_failed"
      ) {
        // Genuinely blocking states — surfaced as an error like the mac
        // `denied`/`restricted` branches above, not a transient status line.
        setError(windowsMicStatusCopy(result));
      } else {
        setPermissionStatus(windowsMicStatusCopy(result));
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

  // Auto-request the mic on step-3 entry so onboarding can never hard-stick
  // a user behind a button click. The module-level guard prevents re-fire when the
  // user steps back to step 2 and forward to step 3 again after permission
  // has already been resolved (authorized/denied/restricted). On macOS the
  // initial mic state is "needed" — wait for the user to click the explicit
  // "Allow microphone" button there, the OS prompt needs the focused app.
  useEffect(() => {
    if (mac) return;
    if (step !== 3) return;
    if (micAutoRequestFiredThisProcess) return;
    if (micState !== "waiting") return;
    if (permissionBusy !== null) return;
    micAutoRequestFiredThisProcess = true;
    void runMicFlow();
  }, [mac, step, micState, permissionBusy, runMicFlow]);

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
    // Full-fidelity Windows status (null on macOS / before first read) and
    // its truthful copy, so MicSetupStep can render state-specific detail
    // beyond the shared five-value badge.
    windowsMicStatus,
    windowsMicStatusCopy: windowsMicStatus ? windowsMicStatusCopy(windowsMicStatus) : undefined,
    permissionBusy,
    permissionStatus,
    runAccessibilityFlow,
    runKeychainFlow,
    runMicFlow,
    setMicState,
    setPermissionStatus,
  };
}
