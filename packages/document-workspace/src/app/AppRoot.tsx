import { useCallback, useMemo, useRef } from "react";
import type { LegalGateTheme } from "@rightkit/legal-ui";
import { Onboarding, type LegalReceiptPort, type LegalStepConfig } from "@rightkit/onboarding";
import { getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { isTauri } from "@rightkit/platform-ui";
import { useSettings } from "@rightkit/document-core/settings/settings";
import {
  LEGAL_GATE_DOCUMENTS,
  LEGAL_MATERIAL_SHA256,
  REQUIRED_LEGAL_ACCEPTANCE_VERSION,
  createOnboardingSettingsBackend,
  legalAcceptanceCurrent,
  legalReceiptFields,
  legalReceiptRecord,
  legalRecordCurrent,
} from "../state/legal.js";
import { App } from "./App.js";
import { useReadyShow } from "./useReadyShow.js";
import { logError, type DiagnosticEvent } from "@rightkit/document-core/state/diagnostics";
import { getShell, isBrowserQaMode } from "@rightkit/document-core/state/tauriBridge";
import { hostAppName } from "../appName.js";

// Locked (Adrian, 2026-07-17): the gate is its own fixed 960x560 window, independent of whatever
// size the user last left the main 1100x750 workspace at. See memory
// "legal-gate-fixed-960x560-own-window".
const GATE_WIDTH = 960;
const GATE_HEIGHT = 560;

const TRUST_LINE =
  `${hostAppName()} works on the documents already on your disk. Nothing is uploaded to open or edit them.`;
const AGREEMENT_STATEMENT =
  `I have read and agree to the ${hostAppName()} License, End User License Agreement, Acceptable Use Policy, and Product Schedule.`;

// Transcribed from .claude/rules/brands.md (Right Suite — App Identities — the host app) LIGHT tokens.
// The legal gate is locked to light mode for every app regardless of the app's own theme setting.
const RIGHTKIT_LEGAL_GATE_THEME: LegalGateTheme = {
  bg: "#F7F3EC",
  surface: "#FFFDF8",
  border: "#E2DAD1",
  text: "#211C18",
  muted: "#6E665F",
  accent: "#0C6AA5",
  accentContrast: "#FFFFFF",
  fontDisplay: "Tanker, 'Arial Narrow', sans-serif",
  fontBody: "Geist, 'Hanken Grotesk', sans-serif",
};

const LEGAL_STEP: LegalStepConfig = {
  theme: RIGHTKIT_LEGAL_GATE_THEME,
  appName: hostAppName(),
  trustLine: TRUST_LINE,
  documents: LEGAL_GATE_DOCUMENTS,
  agreementStatement: AGREEMENT_STATEMENT,
  onQuit: () => void destroyWindow(),
  wordmarkFontUrl: "/fonts/Tanker-400.woff2",
};

async function resizeWindow(width: number, height: number): Promise<void> {
  if (!isTauri()) return;
  try {
    const win = getCurrentWindow();
    await win.setSize(new LogicalSize(width, height));
    await win.center();
  } catch (error) {
    // Never swallow this silently. Tauri rejects setSize/center unless the capability grants
    // core:window:allow-set-size and allow-center — and a swallowed rejection is invisible: every
    // test passes while the real app ignores the resize and renders the gate at the full workspace
    // size. That is exactly how this shipped wrong once (2026-07-17). The reveal must still proceed,
    // so log rather than throw. logError, NOT logEvent: an event reaches the exported diagnostics
    // bundle as an anonymous info-level `renderer_event`, which is its own kind of silence.
    try {
      logError("legal-gate:resize-failed");
    } catch {
      console.error("legal-gate: window resize failed", error);
    }
  }
}

// The shell's `fullscreen` action only toggles, so leave fullscreen through the window API.
const exitFullscreen = () => getCurrentWindow().setFullscreen(false);

async function tryWindowAction(label: DiagnosticEvent, action: () => Promise<void>): Promise<void> {
  try {
    await action();
  } catch (error) {
    try {
      logError(label);
    } catch {
      console.error(`${label}: ${String(error)}`);
    }
  }
}

/** Put the first-run gate into its exact, fixed-size state before revealing the hidden window. */
async function prepareLegalGateWindow(): Promise<void> {
  if (!isTauri()) return;
  const win = getShell().window;
  // A prior native state (including macOS fullscreen) must not survive into the legal gate.
  await tryWindowAction("legal-gate:fullscreen-exit-failed", exitFullscreen);
  await tryWindowAction("legal-gate:unmaximize-failed", () => win.do("unmaximize"));
  await tryWindowAction("legal-gate:resizable-failed", () => getCurrentWindow().setResizable(false));
  await tryWindowAction("legal-gate:maximizable-failed", () => getCurrentWindow().setMaximizable(false));
  await resizeWindow(GATE_WIDTH, GATE_HEIGHT);
}

/** Restore normal controls & maximize workspace before the accepted app shell mounts. */
async function prepareWorkspaceWindow(): Promise<void> {
  if (!isTauri()) return;
  const win = getShell().window;
  await tryWindowAction("workspace:fullscreen-exit-failed", exitFullscreen);
  await tryWindowAction("workspace:resizable-failed", () => getCurrentWindow().setResizable(true));
  await tryWindowAction("workspace:maximizable-failed", () => getCurrentWindow().setMaximizable(true));
  await tryWindowAction("workspace:maximize-failed", () => win.do("maximize"));
}

async function destroyWindow(): Promise<void> {
  if (!isTauri()) return;
  try {
    await getCurrentWindow().destroy();
  } catch {
    // Outside Tauri there is no window to destroy.
  }
}

/**
 * Top-level shell: renders the shared Right Suite legal gate in place of the document workspace
 * until the acceptance receipt in settings is current, then swaps to <App/>. Owns the one call to
 * useReadyShow() for BOTH paths — the gate's own window (960x560, fixed) must reveal itself just
 * like the main app window does, or a real first-run user launching the installer-hidden window
 * sees nothing at all.
 */
export function AppRoot() {
  const { settings, update, updateAsync } = useSettings();
  // Browser QA runs against a fresh profile every launch, so the legal gate would otherwise block
  // every automated run behind a manual click. QA mode is a deterministic harness, never a shipped
  // build (isBrowserQaMode is false in production), so skip the gate there — mirrors how dev-native
  // bypasses the license gate. Real users always see the gate until they accept.
  // Browser QA normally bypasses the first-run gate so unattended document journeys can mount.
  // The explicit qaLegal switch exercises the shipped gate itself with the same browser fixtures;
  // it is only honored in QA/dev builds because isBrowserQaMode() is false in production.
  const qaLegalGate =
    isBrowserQaMode() &&
    typeof window !== "undefined" &&
    new URLSearchParams(window.location.search).get("qaLegal") === "1";
  const needsGate = (!isBrowserQaMode() || qaLegalGate) && !legalAcceptanceCurrent(settings);

  // Configure native state before reveal. The gate is exact 960x560 and fixed; accepted/current
  // workspace launches with normal controls and maximized geometry.
  const beforeShow = useCallback(
    () => (needsGate ? prepareLegalGateWindow() : prepareWorkspaceWindow()),
    [needsGate],
  );
  useReadyShow(beforeShow);

  // Stable ports: onboarding reloads when either identity changes, so they read the live settings
  // through refs instead of closing over each render's values.
  const settingsRef = useRef(settings);
  settingsRef.current = settings;
  const updateRef = useRef(update);
  updateRef.current = update;
  const updateAsyncRef = useRef(updateAsync);
  updateAsyncRef.current = updateAsync;
  const onboardingSettings = useMemo(() => createOnboardingSettingsBackend(), []);
  const legalReceipt = useMemo<LegalReceiptPort>(
    () => ({
      read: () => legalReceiptRecord(settingsRef.current),
      write: async (record) => {
        // Restore native controls & maximize BEFORE settings swaps in <App/>, so app never mounts in
        // fixed legal-gate state even for a single frame.
        await prepareWorkspaceWindow();
        await updateAsyncRef.current(legalReceiptFields(record));
      },
    }),
    [],
  );

  if (!needsGate) return <App />;

  // Onboarding's legal step comes first and wraps the shared LegalGate; with no further steps,
  // accepting the receipt is the whole first run and the settings swap above mounts <App/>.
  return (
    <Onboarding
      settings={onboardingSettings}
      legal={LEGAL_STEP}
      legalReceipt={legalReceipt}
      requiredVersion={REQUIRED_LEGAL_ACCEPTANCE_VERSION}
      requiredDocumentsHash={LEGAL_MATERIAL_SHA256}
      isAccepted={(state) => legalRecordCurrent(state.legalAcceptance)}
      onComplete={() => {}}
    />
  );
}
