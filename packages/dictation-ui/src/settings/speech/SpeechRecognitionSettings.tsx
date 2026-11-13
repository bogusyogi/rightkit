import { memo, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { SettingsGroup } from "@rightkit/settings-ui/core";
import { dictationClient, openPricing } from "../../host.js";
import { PREP_ASSUMED_SECS, prepPercent } from "../../lib/modelPrep.js";
import { cx, ui } from "../../lib/ui.js";
import {
  formatDownloadSize,
  modelUpdatePhaseLabel,
  startModelUpdateDownload,
  useModelUpdate,
} from "../modelUpdate.js";
import type { PatchFn } from "../../types/settings.js";
import { sectionClient } from "../client.js";
import type { LocalAsrModelStatus, LocalBackend, WhisperModelStatus } from "../client.js";
import { MODELS, PICK_LANGUAGES, WHISPER_LANGUAGE_COUNT } from "./options.js";
import type { DlState, WhisperDlState } from "./options.js";
import {
  chipBase,
  chipInfo,
  ChipRow,
  ModelAction,
  ModelCard,
  ModelCardHeader,
  ModelStatus,
  ProBadge,
} from "./ModelCard.js";

const WHISPER_DETAIL_PRO =
  `${WHISPER_LANGUAGE_COUNT} on-device languages, with an optional single-language lock.`;
const WHISPER_DETAIL_LOCKED =
  "Extra Pro transcription languages can be downloaded here after activation.";

function phaseLabel(dl: DlState, elapsed: number) {
  if (dl.phase === "downloading") return `Downloading… ${dl.pct}%`;
  if (dl.phase === "extracting") return "Installing model…";
  if (dl.phase === "preparing") {
    return `Preparing model... ${prepPercent(elapsed, PREP_ASSUMED_SECS.parakeet)}%`;
  }
  return "";
}

function SpeechRecognitionSettingsImpl({
  loaded,
  language,
  asrBackend,
  patch,
}: {
  loaded: boolean;
  language: string;
  asrBackend: string;
  patch: PatchFn;
}) {
  const value = language || "auto";
  // Unified is retired: TDT is the only local lane. A not-yet-migrated install
  // (asrBackend still "parakeet-unified" until TDT finishes downloading) has NO
  // active local card — the TDT card offers its Download button instead, and the
  // backend flips itself on verified install.
  const activeLocal = asrBackend === "parakeet-unified" ? null : "parakeet-tdt";
  const unifiedReplacementPending = asrBackend === "parakeet-unified";
  const [pro, setPro] = useState(() => dictationClient().entitlement() === true);
  const [modelStatus, setModelStatus] = useState<WhisperModelStatus>({
    installed: false,
    prepared: false,
  });
  const [localStatuses, setLocalStatuses] = useState<
    Partial<Record<LocalBackend, LocalAsrModelStatus>>
  >({});
  const [localDl, setLocalDl] = useState<Record<LocalBackend, DlState>>({
    "parakeet-unified": { active: false, phase: "", pct: 0, error: "" },
    "parakeet-tdt": { active: false, phase: "", pct: 0, error: "" },
  });
  const [whisperDl, setWhisperDl] = useState<WhisperDlState>({
    active: false,
    phase: "",
    pct: 0,
    error: "",
  });
  const [elapsed, setElapsed] = useState(0);
  // Both model cards start compact, then expand their language chips in place.
  // This keeps their resting heights aligned without hiding supported languages.
  const [showAllLocalLangs, setShowAllLocalLangs] = useState(false);
  const [showAllLangs, setShowAllLangs] = useState(false);
  const whisperLangs = useMemo(
    () => [{ code: "multi", name: "Auto-detect" }, ...PICK_LANGUAGES],
    [],
  );

  const localBusy = useMemo(() => Object.values(localDl).some((d) => d.active), [localDl]);

  const refreshLocalStatuses = useCallback(async () => {
    const entries = await Promise.all(
      MODELS.map(async (m) => {
        const status = await sectionClient("speech").localAsrModelStatus(m.backend);
        return [m.backend, status] as const;
      }),
    );
    setLocalStatuses(Object.fromEntries(entries) as Record<LocalBackend, LocalAsrModelStatus>);
  }, []);

  useEffect(() => {
    void sectionClient("speech")
      .whisperModelStatus()
      .then(setModelStatus)
      .catch(() => {});
    void refreshLocalStatuses().catch(() => {});
  }, [refreshLocalStatuses]);

  useEffect(() => {
    const unsubscribe = dictationClient().on("entitlement", (entitled) => setPro(entitled === true));
    return () => void unsubscribe.then((f) => f()).catch(() => undefined);
  }, []);

  useEffect(() => {
    if (!localBusy && !whisperDl.active) {
      setElapsed(0);
      return;
    }
    const start = Date.now();
    const id = setInterval(() => setElapsed(Math.floor((Date.now() - start) / 1000)), 1000);
    return () => clearInterval(id);
  }, [localBusy, whisperDl.active]);

  useEffect(() => {
    const un = dictationClient().on("localAsrModel", (payload) => {
      const { phase, received, total } = payload;
      const backend = payload.backend as LocalBackend;
      if (backend !== "parakeet-unified" && backend !== "parakeet-tdt") return;
      // The shared model-update store owns every Parakeet TDT/Ultra download.
      if (backend === "parakeet-tdt") {
        if (phase === "installed") void refreshLocalStatuses().catch(() => {});
        return;
      }
      if (phase === "installed") {
        setLocalDl((d) => ({ ...d, [backend]: { active: false, phase: "", pct: 0, error: "" } }));
        void refreshLocalStatuses().catch(() => {});
        return;
      }
      const pct = phase === "downloading" && total > 0 ? Math.round((received / total) * 100) : 0;
      setLocalDl((d) => ({ ...d, [backend]: { active: true, phase: phase as DlState["phase"], pct, error: "" } }));
    });
    return () => void un.then((f) => f()).catch(() => undefined);
  }, [refreshLocalStatuses]);

  useEffect(() => {
    const un = dictationClient().on("whisperModel", (payload) => {
      const { phase, received, total } = payload;
      if (phase === "downloading") {
        const pct = total > 0 ? Math.round((received / total) * 100) : 0;
        setWhisperDl((d) => ({ ...d, active: true, phase, pct }));
      } else if (phase === "extracting" || phase === "preparing") {
        setWhisperDl((d) => ({ ...d, active: true, phase }));
      } else if (phase === "installed") {
        setModelStatus({ installed: true, prepared: true });
        setWhisperDl({ active: false, phase: "", pct: 0, error: "" });
      }
    });
    return () => void un.then((f) => f()).catch(() => undefined);
  }, []);

  const downloadLocalModel = useCallback(
    async (backend: LocalBackend) => {
      if (backend === "parakeet-tdt") {
        // One shared flow (and one download) for the modal and Settings.
        void startModelUpdateDownload();
        return;
      }
      if (localDl[backend].active) return;
      setLocalDl((d) => ({
        ...d,
        [backend]: { active: true, phase: "downloading", pct: 0, error: "" },
      }));
      try {
        const status = await sectionClient("speech").downloadLocalAsrModel(backend);
        setLocalStatuses((s) => ({ ...s, [backend]: status }));
        if (status.installed) {
          await dictationClient().registerHotkeys().catch(() => undefined);
        }
        setLocalDl((d) => ({
          ...d,
          [backend]: { active: false, phase: "", pct: 0, error: "" },
        }));
      } catch (e) {
        setLocalDl((d) => ({
          ...d,
          [backend]: { active: false, phase: "", pct: 0, error: String(e) },
        }));
      }
    },
    [localDl],
  );

  async function selectLocalModel(backend: LocalBackend) {
    const status = localStatuses[backend];
    if (!loaded || !status?.installed || localDl[backend].active) return;
    await patch("asr_backend", backend);
    if (value !== "auto") await patch("dictation_language", "auto");
    setLocalDl((d) => ({
      ...d,
      [backend]: { active: true, phase: "preparing", pct: 0, error: "" },
    }));
    try {
      await sectionClient("speech").prewarmEngine();
      setLocalDl((d) => ({ ...d, [backend]: { active: false, phase: "", pct: 0, error: "" } }));
    } catch (e) {
      // A bare try/finally with no catch would clear the "preparing" spinner
      // (so the UI looks done) while a prewarm failure vanishes as a silent
      // unhandled promise rejection, leaving the user unaware the model never
      // actually finished preparing.
      setLocalDl((d) => ({
        ...d,
        [backend]: { active: false, phase: "", pct: 0, error: String(e) },
      }));
    }
  }

  function pickWhisperTarget(code: string) {
    if (!pro || !modelStatus.prepared) return;
    if (asrBackend !== "whisper") void patch("asr_backend", "whisper");
    void patch("dictation_language", code);
  }

  async function downloadWhisperModel() {
    if (whisperDl.active) return;
    setWhisperDl({ active: true, phase: "downloading", pct: 0, error: "" });
    try {
      await sectionClient("speech").downloadWhisperModel();
      setModelStatus({ installed: true, prepared: true });
      setWhisperDl({ active: false, phase: "", pct: 0, error: "" });
      // Switching to Whisper is valid from any prior lane, including a
      // retired-Unified install that never migrated (activeLocal === null).
      if (asrBackend !== "whisper") {
        void patch("asr_backend", "whisper");
        void patch("dictation_language", "multi");
      }
    } catch (e) {
      setWhisperDl({ active: false, phase: "", pct: 0, error: String(e) });
    }
  }

  const modelUpdate = useModelUpdate();
  const tdtDl: DlState = {
    active: modelUpdate.busy,
    phase:
      modelUpdate.phase === "verifying"
        ? "extracting"
        : modelUpdate.phase === "preparing"
          ? "preparing"
          : "downloading",
    pct: modelUpdate.pct,
    error: "",
  };
  const wasBusy = useRef(false);
  const tdtInstalled = localStatuses["parakeet-tdt"]?.installed === true;
  const tdtReplacementRequired =
    localStatuses["parakeet-tdt"]?.replacement_required === true;
  const modelReplacementPending = unifiedReplacementPending || tdtReplacementRequired;

  // Rust persists the replacement after verified install, but this Settings
  // mount may still hold the pre-download Unified snapshot. Reconcile it here
  // as soon as the status refresh proves TDT exists, including users already
  // stuck on the old "Ready" state from an earlier attempt.
  useEffect(() => {
    if (loaded && tdtInstalled && unifiedReplacementPending) {
      void patch("asr_backend", "parakeet-tdt");
    }
  }, [loaded, patch, tdtInstalled, unifiedReplacementPending]);

  // When the shared download ends (installed or failed), re-read model status.
  useEffect(() => {
    if (wasBusy.current && !modelUpdate.busy) void refreshLocalStatuses().catch(() => {});
    wasBusy.current = modelUpdate.busy;
  }, [modelUpdate.busy, refreshLocalStatuses]);

  return (
    <SettingsGroup>
      {modelReplacementPending ? (
        <div className="mb-[var(--space-2)] flex flex-wrap items-center justify-between gap-[var(--space-2)] rounded-[10px] border border-[color-mix(in_srgb,var(--warn),transparent_55%)] bg-[color-mix(in_srgb,var(--warn),transparent_92%)] px-[var(--space-3)] py-[var(--space-2)]">
          <p className="m-0 text-[length:var(--size-xs)] leading-snug text-[var(--paper)]">
            <span className="font-medium">A faster speech model is ready.</span>{" "}
            {modelUpdate.modelLabel} is faster and smaller. Your current model keeps working until
            the new one is verified.
            {modelUpdate.downloadBytes > 0
              ? ` Download size: ${formatDownloadSize(modelUpdate.downloadBytes)}.`
              : ""}
          </p>
          {tdtDl.active ? (
            <span
              aria-live="polite"
              className="whitespace-nowrap text-[length:var(--size-xs)] text-[var(--muted)]"
            >
              {modelUpdatePhaseLabel(modelUpdate)}
            </span>
          ) : !tdtInstalled ? (
            <button
              type="button"
              onClick={() => void startModelUpdateDownload()}
              className={cx(ui.button, ui.buttonCompact, "whitespace-nowrap")}
            >
              Download update
            </button>
          ) : null}
        </div>
      ) : null}
      <div
        className="grid gap-[var(--space-3)] sm:grid-cols-2"
        data-setting-id="settings.speech-recognition-model"
      >
        {MODELS.map((m) => {
          const status = localStatuses[m.backend];
          const active = activeLocal === m.backend && asrBackend !== "whisper";
          const dl = m.backend === "parakeet-tdt" ? tdtDl : localDl[m.backend];
          return (
            <ModelCard
              key={m.backend}
              active={active}
              overlayActive={dl.active}
              overlayLabel={phaseLabel(dl, elapsed)}
              error={dl.error}
              onDismissError={() =>
                setLocalDl((d) => ({
                  ...d,
                  [m.backend]: { ...d[m.backend], error: "" },
                }))
              }
            >
              <ModelCardHeader
                title={m.title}
                summary={m.summary}
                detail={m.detail}
                selected={active}
                selectDisabled={!loaded || !status?.installed || dl.active}
                onSelect={() => void selectLocalModel(m.backend)}
                status={
                  !status ? (
                    <ModelStatus state="checking" />
                  ) : status.installed ? (
                    <ModelStatus state={active ? "active" : "ready"} />
                  ) : !dl.active ? (
                    <ModelAction onClick={() => void downloadLocalModel(m.backend)}>
                      Download model
                    </ModelAction>
                  ) : null
                }
              />
              <ChipRow
                items={m.languages}
                expanded={showAllLocalLangs}
                onToggleExpanded={() => setShowAllLocalLangs((v) => !v)}
                keyOf={(name) => name}
                renderItem={(name) => <span className={chipInfo}>{name}</span>}
              />
            </ModelCard>
          );
        })}

        {/* Whisper card: the SAME ModelCard / ModelCardHeader / ChipRow
            skeleton as the Parakeet card, so title, status/action slot,
            detail and chips sit in identical places. Language chips are the
            selectors here (Parakeet selects on the header). */}
        <ModelCard
          active={asrBackend === "whisper"}
          muted={!pro}
          overlayActive={whisperDl.active}
          overlayLabel={
            whisperDl.phase === "downloading"
              ? `Downloading… ${whisperDl.pct}%`
              : whisperDl.phase === "preparing"
                ? `Preparing model... ${prepPercent(elapsed, PREP_ASSUMED_SECS.whisper)}%`
                : "Installing…"
          }
          error={whisperDl.error}
          onDismissError={() => setWhisperDl((d) => ({ ...d, error: "" }))}
        >
          <ModelCardHeader
            title="Pro languages"
            summary="Powered by Whisper"
            detail={
pro ? WHISPER_DETAIL_PRO : WHISPER_DETAIL_LOCKED
            }
            status={
              !pro ? (
                <ProBadge onClick={() => void openPricing()} />
              ) : modelStatus.prepared ? (
                <ModelStatus state={asrBackend === "whisper" ? "active" : "ready"} />
              ) : !whisperDl.active ? (
                <ModelAction onClick={() => void downloadWhisperModel()}>
                  {modelStatus.installed ? "Prepare model" : "Download model"}
                </ModelAction>
              ) : null
            }
          />

          {pro ? (
            <div data-setting-id="settings.dictation-language">
              {!modelStatus.prepared ? (
                <p className="m-0 mb-[var(--space-2)] text-[length:var(--size-xs)] text-[var(--muted)]">
                  {modelStatus.installed
                    ? "Prepare Whisper to choose a language."
                    : "Download Whisper to choose a language."}{" "}
                  <button
                    type="button"
                    className="font-medium text-[var(--accent-cool)] underline disabled:cursor-not-allowed disabled:opacity-50"
                    onClick={() => void downloadWhisperModel()}
                    disabled={!loaded || whisperDl.active}
                  >
                    {modelStatus.installed ? "Prepare Whisper" : "Download Whisper"}
                  </button>
                </p>
              ) : null}
              <ChipRow
                items={whisperLangs}
                expanded={showAllLangs}
                onToggleExpanded={() => setShowAllLangs((v) => !v)}
                keyOf={(l) => l.code}
                renderItem={(l) => {
                  const active = asrBackend === "whisper" && value === l.code;
                  return (
                    <button
                      type="button"
                      disabled={!loaded || !modelStatus.prepared}
                      aria-pressed={active}
                      title={
                        !modelStatus.prepared
                          ? modelStatus.installed
                            ? "Prepare Whisper to choose a language"
                            : "Download Whisper to choose a language"
                          : undefined
                      }
                      onClick={() => pickWhisperTarget(l.code)}
                      className={cx(
                        chipBase,
                        "cursor-pointer border transition-[background-color,border-color,transform] duration-150 ease-[var(--ease-out-strong)] focus-visible:outline-none focus-visible:[box-shadow:var(--focus-ring)] active:scale-[0.97] disabled:cursor-not-allowed disabled:opacity-50",
                        active
                          ? "border-[color-mix(in_srgb,var(--paper),transparent_45%)] bg-[var(--surface-3)] font-medium text-[var(--paper)]"
                          : "border-[var(--line)] bg-transparent text-[var(--paper)] hover:bg-[var(--surface-hover)]",
                      )}
                    >
                      {l.name}
                    </button>
                  );
                }}
              />
            </div>
          ) : null}
        </ModelCard>
      </div>
      <p className="m-0 mx-[14px] mt-[7px] text-[length:var(--size-xs)] leading-[1.45] text-[var(--muted)] [text-wrap:pretty]">
            Models are private downloads stored in app data. Parakeet Ultra (based on NVIDIA Parakeet TDT) is licensed under{" "}
            <a
              href="https://creativecommons.org/licenses/by/4.0/"
              className="text-[var(--accent-cool)] underline"
            >
              Creative Commons Attribution 4.0
            </a>
            . Exact notices ship with the app.
      </p>
    </SettingsGroup>
  );
}

export const SpeechRecognitionSettings = memo(SpeechRecognitionSettingsImpl);
