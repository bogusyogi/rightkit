import { useCallback, useMemo, useRef } from "react";
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
import { getShell } from "@rightkit/document-core/state/tauriBridge";
import { hostAppName } from "../appName.js";
import { workspaceConfig } from "../config.js";

function legalStep(): LegalStepConfig | null {
  const firstRun = workspaceConfig().firstRun;
  if (!firstRun) return null;
  return {
    appName: hostAppName(),
    documents: LEGAL_GATE_DOCUMENTS,
    onQuit: () => void destroyWindow(),
    ...firstRun.legal,
  };
}

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
  // The host may give the gate its own fixed size, independent of the workspace window's last size.
  const size = workspaceConfig().firstRun?.gateWindow;
  if (size) await resizeWindow(size.width, size.height);
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
 * Top-level shell: when the host configured a first-run gate (`configureDocumentWorkspace({ firstRun })`),
 * renders the shared Right Suite legal gate in place of the document workspace until the acceptance
 * receipt in settings is current, then swaps to <App/>. Owns the one call to useReadyShow() for BOTH
 * paths — the gate's own window must reveal itself just like the main app window does, or a real
 * first-run user launching the installer-hidden window sees nothing at all.
 */
export function AppRoot() {
  const { settings, update, updateAsync } = useSettings();
  const legal = useMemo(legalStep, []);
  const needsGate = legal !== null && !legalAcceptanceCurrent(settings);

  // Configure native state before reveal. The gate is fixed at the host's size; accepted/current
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

  if (!needsGate || !legal) return <App />;

  // Onboarding's legal step comes first and wraps the shared LegalGate; with no further steps,
  // accepting the receipt is the whole first run and the settings swap above mounts <App/>.
  return (
    <Onboarding
      settings={onboardingSettings}
      legal={legal}
      legalReceipt={legalReceipt}
      requiredVersion={REQUIRED_LEGAL_ACCEPTANCE_VERSION}
      requiredDocumentsHash={LEGAL_MATERIAL_SHA256}
      isAccepted={(state) => legalRecordCurrent(state.legalAcceptance)}
      onComplete={() => {}}
    />
  );
}
