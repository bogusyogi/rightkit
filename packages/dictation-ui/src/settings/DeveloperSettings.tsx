import { memo, useEffect, useState } from "react";
import { getPlatform } from "@rightkit/platform-ui";
import { CompactSettingRow, CompactSettingsGroup } from "../components/settings/CompactSettingsGroup.js";
import { hostComponents, hostHooks, type RecognitionSetupOptions } from "../host.js";
import { cx, ui } from "../lib/ui.js";
import type { PatchFn } from "../types/settings.js";
import { sectionClient, type WakeLabStatus } from "./client.js";
import { SectionToggle as Toggle } from "./Toggle.js";

// Without a host recognition setup the route test has no state and no button.
const noRecognitionSetup = (_options: RecognitionSetupOptions) => ({ phase: "idle" });

interface Props {
  loaded: boolean;
  developerMode: boolean;
  captureAudio: boolean;
  streamingInsertion: boolean;
  patch: PatchFn;
}

function DeveloperSettingsImpl(props: Props) {
  const platform = getPlatform();
  const [wakeLab, setWakeLab] = useState<WakeLabStatus | null>(null);
  const [notice, setNotice] = useState("");
  const [coreMlCacheBusy, setCoreMlCacheBusy] = useState(false);
  const [coreMlCacheConfirm, setCoreMlCacheConfirm] = useState(false);
  const [coreMlCacheNotice, setCoreMlCacheNotice] = useState<"" | "success" | "error">("");
  const [diagnosticsBusy, setDiagnosticsBusy] = useState(false);
  // Per export, never remembered: crash dumps can hold memory contents, so each export starts off.
  const [includeCrashDumps, setIncludeCrashDumps] = useState(false);
  const [diagnosticsStatus, setDiagnosticsStatus] = useState<
    { kind: "success" | "error"; message: string } | undefined
  >();
  const [captureNotice, setCaptureNotice] = useState("");
  const [capturedAudioPresent, setCapturedAudioPresent] = useState(false);
  const [routeTestOpen, setRouteTestOpen] = useState(false);
  const [routeTestError, setRouteTestError] = useState("");
  const routeTestHooks = hostHooks();
  const RecognitionSetupStep = hostComponents().RecognitionSetupStep;
  const routeTestAvailable = RecognitionSetupStep !== undefined && routeTestHooks.useRecognitionSetup !== undefined;
  const routeTest = (routeTestHooks.useRecognitionSetup ?? noRecognitionSetup)({
    active: props.developerMode && routeTestOpen,
    setError: setRouteTestError,
  });

  useEffect(() => {
    let alive = true;
    sectionClient("developer")
      .wakeLabStatus()
      .then((v) => alive && setWakeLab(v))
      .catch(() => alive && setWakeLab({ marker_present: false, enabled: false, threshold: 0.25 }));
    return () => {
      alive = false;
    };
  }, []);

  useEffect(() => {
    if (!props.developerMode || props.captureAudio) return;
    let alive = true;
    sectionClient("developer")
      .diagnosticCaptureAudioPresent()
      .then((present) => alive && setCapturedAudioPresent(present))
      .catch(() => alive && setCapturedAudioPresent(false));
    return () => {
      alive = false;
    };
  }, [props.developerMode, props.captureAudio]);

  useEffect(() => {
    if (!props.developerMode) {
      setRouteTestOpen(false);
      setIncludeCrashDumps(false);
    }
  }, [props.developerMode]);

  async function toggleWake(next: boolean) {
    const previous = wakeLab;
    if (!wakeLab) return;
    setWakeLab({ ...wakeLab, enabled: next });
    try {
      await sectionClient("developer").setWakeLabEnabled(next);
      setNotice("");
    } catch (error) {
      setWakeLab(previous);
      setNotice(String(error));
    }
  }

  async function recompileCoreMlCache() {
    setCoreMlCacheBusy(true);
    setCoreMlCacheConfirm(false);
    setCoreMlCacheNotice("");
    try {
      await sectionClient("developer").recompileCoreMlCache();
      setCoreMlCacheNotice("success");
    } catch {
      setCoreMlCacheNotice("error");
    } finally {
      setCoreMlCacheBusy(false);
    }
  }

  async function sendDiagnostics() {
    setDiagnosticsBusy(true);
    setDiagnosticsStatus(undefined);
    try {
      const reportId = await sectionClient("developer").sendDiagnostics();
      setDiagnosticsStatus({ kind: "success", message: `Diagnostics sent · ${reportId}` });
    } catch (error) {
      setDiagnosticsStatus({ kind: "error", message: `Couldn’t send diagnostics · ${String(error)}` });
    } finally {
      setDiagnosticsBusy(false);
    }
  }

  async function exportDiagnostics() {
    setDiagnosticsBusy(true);
    setDiagnosticsStatus(undefined);
    try {
      const archive = await sectionClient("developer").exportDiagnostics(includeCrashDumps);
      setDiagnosticsStatus({ kind: "success", message: `Diagnostics exported · ${archive}` });
    } catch (error) {
      setDiagnosticsStatus({ kind: "error", message: `Couldn’t export diagnostics · ${String(error)}` });
    } finally {
      setDiagnosticsBusy(false);
      // The opt-in covers this one export. Whatever happened, the next export starts off again.
      setIncludeCrashDumps(false);
    }
  }

  return (
    <CompactSettingsGroup title="Developer tools">
      <CompactSettingRow
        label="Developer mode"
        sublabel="Unlocks troubleshooting tools. Nothing is sent automatically."
        settingId="settings.developer-mode"
      >
        <Toggle
          checked={props.developerMode}
          disabled={!props.loaded}
          ariaLabel="Developer mode"
          onChange={(v) => props.patch("developer_mode", v)}
        />
      </CompactSettingRow>
      {props.developerMode ? (
        <div className="divide-y divide-[color-mix(in_srgb,var(--line),transparent_30%)] px-[var(--space-4)]">
          <section
            data-setting-id="settings.diagnostics-and-routing"
            className="grid items-start gap-3 py-[var(--space-3)] lg:grid-cols-[minmax(180px,0.65fr)_minmax(0,2fr)]"
          >
            <div>
              <p className={ui.sectionLabel}>Diagnostics &amp; routing</p>
              <p className={cx(ui.fieldNote, "mt-1")}>Send redacted logs or isolate recognition.</p>
            </div>
            <div className="grid content-start gap-2">
              <div
                className={cx(
                  "grid gap-2",
                  platform === "macos" ? "sm:grid-cols-3" : "sm:grid-cols-2",
                )}
              >
                <button
                  type="button"
                  aria-label="Send diagnostics"
                  className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact, "w-full")}
                  disabled={diagnosticsBusy}
                  onClick={() => void sendDiagnostics()}
                >
                  {diagnosticsBusy ? "Sending…" : "Send diagnostics"}
                </button>
{routeTestAvailable ? (
                <button
                  type="button"
                  aria-label={routeTestOpen ? "Close route test" : "Run route test"}
                  className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact, "w-full")}
                  disabled={routeTest.phase === "processing"}
                  onClick={() => {
                    setRouteTestError("");
                    setRouteTestOpen((open) => !open);
                  }}
                >
                  {routeTestOpen ? "Close route test" : "Run route test"}
                </button>
                ) : null}
                {platform === "macos" ? (
                  <button
                    type="button"
                    className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact, "w-full")}
                    disabled={coreMlCacheBusy}
                    onClick={() => setCoreMlCacheConfirm(true)}
                  >
                    {coreMlCacheBusy ? "Recompiling…" : "Recompile Core ML cache"}
                  </button>
                ) : null}
              </div>
              <div className="grid gap-1">
                <DevToggle
                  label="Include crash dumps in the export"
                  settingId="settings.export-crash-dumps"
                  checked={includeCrashDumps}
                  disabled={diagnosticsBusy}
                  onChange={setIncludeCrashDumps}
                />
                <p className={cx(ui.fieldNote, "mt-0")}>
                  Crash dumps can contain memory contents, such as text you dictated. They go only
                  into a file saved on this computer, and only when this is on. Sending diagnostics
                  never includes them.
                </p>
                <button
                  type="button"
                  aria-label="Export diagnostics"
                  className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact, "w-full")}
                  disabled={diagnosticsBusy}
                  onClick={() => void exportDiagnostics()}
                >
                  Export diagnostics
                </button>
              </div>
              {diagnosticsStatus ? (
                <p
                  className={cx(
                    ui.fieldNote,
                    diagnosticsStatus.kind === "error" ? ui.dangerText : "text-[var(--ok)]",
                    "break-all",
                  )}
                  role={diagnosticsStatus.kind === "error" ? "alert" : "status"}
                >
                  {diagnosticsStatus.message}
                </p>
              ) : null}
              {routeTestAvailable && routeTestOpen ? (
                <div className="grid gap-3">
                  <RecognitionSetupStep setup={routeTest} context="developer" />
                  {routeTestError ? (
                    <p className={cx(ui.fieldNote, ui.dangerText)} role="alert">
                      {routeTestError}
                    </p>
                  ) : null}
                </div>
              ) : null}
              {platform === "macos" && coreMlCacheConfirm ? (
                <div className="mt-1">
                  <div
                    className={cx(ui.surface, "grid gap-2 p-3")}
                    role="alertdialog"
                    aria-label="Recompile Core ML cache confirmation"
                  >
                    <p className="m-0 text-[length:var(--size-sm)] text-[var(--paper)]">
                      Recompile Core ML cache? The current engine will restart and recognition may
                      be unavailable for several minutes.
                    </p>
                    <div className="flex flex-wrap gap-2">
                      <button
                        type="button"
                        className={cx(ui.button, ui.buttonSecondary, ui.buttonCompact)}
                        onClick={() => setCoreMlCacheConfirm(false)}
                      >
                        Cancel
                      </button>
                      <button
                        type="button"
                        className={cx(ui.button, ui.buttonCompact)}
                        disabled={coreMlCacheBusy}
                        onClick={() => void recompileCoreMlCache()}
                      >
                        {coreMlCacheBusy ? "Recompiling…" : "Recompile Core ML cache"}
                      </button>
                    </div>
                  </div>
                  {coreMlCacheNotice === "success" ? (
                    <p className={cx(ui.fieldNote, "mt-2 text-[var(--ok)]")} role="status">
                      Core ML cache recompiled.
                    </p>
                  ) : null}
                  {coreMlCacheNotice === "error" ? (
                    <p className={cx(ui.fieldNote, ui.dangerText, "mt-2")} role="alert">
                      Core ML cache could not be recompiled. Try again from Developer Mode.
                    </p>
                  ) : null}
                </div>
              ) : (
                <>
                  {platform === "macos" && coreMlCacheNotice === "success" ? (
                    <p className={cx(ui.fieldNote, "mt-2 text-[var(--ok)]")} role="status">
                      Core ML cache recompiled.
                    </p>
                  ) : null}
                  {platform === "macos" && coreMlCacheNotice === "error" ? (
                    <p className={cx(ui.fieldNote, ui.dangerText, "mt-2")} role="alert">
                      Core ML cache could not be recompiled. Try again from Developer Mode.
                    </p>
                  ) : null}
                </>
              )}
            </div>
          </section>
          <section data-setting-id="settings.local-experiments" className="py-[var(--space-3)]">
            <div className="flex min-h-8 items-center justify-between gap-3">
              <p className={ui.sectionLabel}>Local experiments</p>
              {props.captureAudio || capturedAudioPresent ? (
                <button
                  type="button"
                  className={cx(ui.button, ui.buttonGhost, ui.buttonCompact)}
                  onClick={() => {
                    setCaptureNotice("");
                    void sectionClient("developer")
                      .openDiagnosticCapturesFolder()
                      .catch((error) => setCaptureNotice(String(error)));
                  }}
                >
                  Open recordings
                </button>
              ) : null}
            </div>
            <div className="grid">
              <DevToggle
                label="Save recording audio locally"
                settingId="settings.save-recording-audio"
                checked={props.captureAudio}
                onChange={(v) => props.patch("diagnostic_audio_capture", v)}
              />
              <DevToggle
                label="Experimental Hey Zephyr"
                settingId="settings.experimental-hey-zephyr"
                checked={wakeLab?.enabled === true}
                disabled={!wakeLab}
                onChange={(v) => void toggleWake(v)}
              />
              <DevToggle
                label="Stream words into the text box"
                settingId="settings.streaming-insertion"
                checked={props.streamingInsertion}
                onChange={(v) => props.patch("streaming_insertion", v)}
              />
            </div>
            {captureNotice ? <p className={cx(ui.fieldNote, ui.dangerText)}>{captureNotice}</p> : null}
            {notice ? <p className={cx(ui.fieldNote, ui.dangerText)}>{notice}</p> : null}
          </section>
        </div>
      ) : null}
    </CompactSettingsGroup>
  );
}

function DevToggle({
  label,
  settingId,
  checked,
  onChange,
  disabled = false,
}: {
  label: string;
  settingId?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}) {
  return (
    <div
      data-setting-id={settingId}
      className="flex min-h-9 items-center justify-between gap-3 text-[var(--paper)]"
    >
      <span>{label}</span>
      <Toggle checked={checked} disabled={disabled} ariaLabel={label} onChange={onChange} />
    </div>
  );
}

export const DeveloperSettings = memo(DeveloperSettingsImpl);
