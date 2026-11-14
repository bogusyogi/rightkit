import {
  Onboarding as RightKitOnboarding,
  modelsStep,
  type LegalReceiptPort,
  type LegalStepConfig,
  type ModelEntry,
  type ModelsBackend,
  type OnboardingStep,
  type OnboardingStepContext,
} from "@rightkit/onboarding";
import "@rightkit/onboarding/onboarding.css";
import { useEffect, useMemo, useRef, useState, type MutableRefObject, type ReactNode } from "react";
import { dictationClient, hostAppName, hostOnboarding } from "../host.js";
import type { SettingsBlob } from "../types/settings.js";
import { onboardingClient } from "./client.js";
import { speechClient } from "./speech.js";
import { PRIVACY_LINE } from "./constants.js";
import { ControlsStep } from "./ControlsStep.js";
import {
  MODEL_STEP_ID,
  ONBOARDING_STEPS,
  blockedHintFor,
  controlsReady,
  hardwareReady,
  modelAdvancePatch,
  nextModelProgress,
  stepHoldsContinue,
  stepProgressLabel,
} from "./flow.js";
import { MicSetupStep } from "./MicSetupStep.js";
import { ModelDownloadExtras, ModelIntro } from "./ModelStep.js";
import { onboardingModel } from "./model.js";
import { usePlatform } from "./platform.js";
import { RecognitionSetupStep } from "./RecognitionSetupStep.js";
import { AppLivesStep, CommandsStep, WelcomeStep } from "./StaticSteps.js";
import "./stepCount.css";
import { TrialStep } from "./TrialStep.js";
import type { OS, Step } from "./types.js";
import { useOnboardingPermissions } from "./usePermissions.js";
import { useOnboardingRecognitionSetup } from "./useRecognitionSetup.js";
import { useOnboardingRecording } from "./useRecording.js";
import { useOnboardingTrial } from "./useTrial.js";

function modelEntry(status: { installed: boolean }): ModelEntry {
  const model = onboardingModel();
  return {
    id: model.id,
    label: model.label,
    description: "Downloaded once and verified before it is used.",
    // macOS may resolve a working model from RightKit's shared model store;
    // `installed` is the native readiness result, regardless of storage root.
    state: status.installed ? "installed" : "missing",
  };
}

function createModelsBackend(): ModelsBackend {
  // The verify and install phases report no byte total; the bar keeps its place instead of dropping to 0.
  let lastProgress = 0;
  return {
    async list() {
      const status = await speechClient().localAsrModelStatus(onboardingModel().id);
      return [modelEntry(status)];
    },
    async install() {
      await speechClient().downloadLocalAsrModel(onboardingModel().id);
    },
    // No cancel(): the native download has no cancellation command, so the row draws no Pause button.
    async subscribe(handler) {
      const { id, label } = onboardingModel();
      const unlisten = await dictationClient().on("localAsrModel", (payload) => {
        if (payload.backend !== id) return;
        const { phase, received, total, message } = payload;
        const downloading = phase === "downloading" || phase === "preparing";
        const verifying = phase === "extracting" || phase === "verifying";
        const failed = phase === "error";
        lastProgress = nextModelProgress(lastProgress, { phase, received, total });
        handler({
          id,
          label,
          description: message ?? "Downloaded once and verified before it is used.",
          sizeBytes: total > 0 ? total : undefined,
          state: phase === "installed" ? "installed" : failed ? "error" : verifying ? "verifying" : downloading ? "downloading" : "missing",
          progress: lastProgress,
          error: failed ? { code: "download", message: message ?? "Model download failed." } : undefined,
        });
      });
      return unlisten;
    },
  };
}

/**
 * The legal receipt: the `legal_*` settings fields (version, UTC time, eligibility basis, material hash) mapped to
 * the owner's `LegalReceiptPort`. The owner reads it on load (it overrides the settings copy, so a changed version
 * or hash re-prompts) and records through `write` when the user agrees. There is no native receipt service in this
 * build, so `create` is not offered.
 */
function createLegalReceipt(isCurrent: (values: SettingsBlob) => boolean): LegalReceiptPort {
  const settings = dictationClient().settings;
  return {
    async read() {
      const values = (await settings.load()).values as SettingsBlob;
      if (!isCurrent(values)) return null;
      return {
        version: String(values.legal_acceptance_version),
        documentsHash: String(values.legal_material_sha256),
        acceptedAt: String(values.legal_accepted_at),
        eligibilityBasis: values.legal_eligibility_basis as "individual" | "enterprise",
      };
    },
    async write(record) {
      await settings.patch({
        legal_acceptance_version: record.version,
        legal_accepted_at: record.acceptedAt,
        legal_eligibility_basis: record.eligibilityBasis,
        legal_material_sha256: record.documentsHash,
      });
      // The hub opens at the agreement's size; the native side now resolves the onboarding surface, so resize to it.
      await onboardingClient().resizeHubForSurface().catch(() => undefined);
    },
    // No reset(): the native validators reject a null legal_* patch, so a reset could only fail.
  };
}

type FlowProps = {
  id: string;
  context: OnboardingStepContext;
  platform: OS;
  recognitionReady: MutableRefObject<boolean>;
};

function HardwareStep({ id, context, platform }: FlowProps) {
  const appStep: Step = id === "hardware" ? 3 : 4;
  const [error, setError] = useState("");
  const permissions = useOnboardingPermissions({ mac: platform === "macos", step: appStep, setError });
  const recording = useOnboardingRecording({
    micState: permissions.micState,
    platform,
    setError,
    setPermissionStatus: permissions.setPermissionStatus,
    step: appStep,
  });
  // Continue is gated on this step's readiness. Microphone: Keychain answered (macOS), no mic check still listening.
  // Shortcuts: Accessibility granted on macOS.
  const mac = platform === "macos";
  const ready =
    appStep === 3
      ? hardwareReady({
          permissionBusy: permissions.permissionBusy,
          keychainState: permissions.keychainState,
          micState: permissions.micState,
          micCheckState: recording.micCheckState,
        })
      : controlsReady({ mac, accessibilityState: permissions.accessibilityState });
  const { setReady } = context;
  useEffect(() => setReady(ready), [setReady, ready]);
  if (appStep === 3) {
    return (
      <>
        {error ? <p role="alert" className="rk-onboarding__error">{error}</p> : null}
        <MicSetupStep
          mac={platform === "macos"}
          keychainTitle={permissions.keychainTitle}
          keychainCopy={permissions.keychainCopy}
          keychainAction={permissions.keychainAction}
          permissionBusy={permissions.permissionBusy}
          keychainState={permissions.keychainState}
          micAction={permissions.micAction}
          micState={permissions.micState}
          micCheckState={recording.micCheckState}
          micSignalSeen={recording.micSignalSeen}
          inputDevice={recording.inputDevice}
          micRefreshToken={recording.micRefreshToken}
          permissionStatus={permissions.permissionStatus}
          onRunKeychainFlow={() => void permissions.runKeychainFlow()}
          onRunMicFlow={() => void permissions.runMicFlow()}
          onMicDeviceChange={(value) => void recording.selectMicDevice(value)}
          onRetryMicCheck={recording.retryMicCheck}
        />
      </>
    );
  }
  return (
    <>
      {error ? <p role="alert" className="rk-onboarding__error">{error}</p> : null}
      <ControlsStep
        mac={platform === "macos"}
        accessibilityCopy={permissions.accessibilityCopy}
        accessibilityAction={permissions.accessibilityAction}
        accessibilityBusyLabel={permissions.accessibilityBusyLabel}
        accessibilityState={permissions.accessibilityState}
        permissionBusy={permissions.permissionBusy}
        platform={platform}
        liveChordKeys={recording.liveChordKeys}
        activeShortcut={recording.activeShortcut}
        shortcutSeen={recording.shortcutSeen}
        ctrlHeld={recording.ctrlHeld}
        controlSeen={recording.controlSeen}
        middleClickPulse={recording.middleClickPulse}
        permissionStatus={permissions.permissionStatus}
        onRunAccessibilityFlow={() => void permissions.runAccessibilityFlow()}
      />
    </>
  );
}

function TrialFlowStep({ platform }: Pick<FlowProps, "platform">) {
  const [error, setError] = useState("");
  const trial = useOnboardingTrial({ platform, setError });
  return <>{error ? <p role="alert" className="rk-onboarding__error">{error}</p> : null}<TrialStep {...trial} /></>;
}

function RecognitionFlowStep({ context, recognitionReady }: Pick<FlowProps, "context" | "recognitionReady">) {
  const [error, setError] = useState("");
  const setup = useOnboardingRecognitionSetup({ active: true, setError });
  const { setReady } = context;
  useEffect(() => {
    recognitionReady.current = setup.setupComplete;
    setReady(setup.setupComplete);
  }, [recognitionReady, setReady, setup.setupComplete]);
  return <>{error ? <p role="alert" className="rk-onboarding__error">{error}</p> : null}<RecognitionSetupStep setup={setup} /></>;
}

function AppStep({ context, platform, recognitionReady, id }: FlowProps) {
  if (id === "welcome") return <WelcomeStep />;
  if (id === "hardware" || id === "controls") return <HardwareStep id={id} context={context} platform={platform} recognitionReady={recognitionReady} />;
  if (id === "commands") return <CommandsStep platform={platform} />;
  if (id === "trial") return <TrialFlowStep platform={platform} />;
  if (id === "app-lives") return <AppLivesStep trayName={platform === "macos" ? "menu bar" : "system tray"} dockName={platform === "macos" ? "Dock" : "taskbar"} />;
  return <RecognitionFlowStep context={context} recognitionReady={recognitionReady} />;
}

/** What the owner supplies to the setup. Everything else comes from the host configuration (`configureDictationHost`). */
export interface DictationOnboardingProps {
  /** Called once setup has finished and the hub may open. */
  onComplete: () => void;
  /** The agreement shown first. The app name is the host's; `trustLine` defaults to the privacy line of the setup. */
  legal: Omit<LegalStepConfig, "appName" | "trustLine"> & { trustLine?: string };
  /** The acceptance the stored receipt must match: its version and document hash, and whether the stored `legal_*` settings are current. */
  acceptance: { version: string; documentsHash: string; isCurrent: (values: SettingsBlob) => boolean };
  /** Strip content at the frame's right edge (the Windows caption buttons). */
  titlebar?: ReactNode;
}

export function DictationOnboarding({ onComplete, legal, acceptance, titlebar }: DictationOnboardingProps) {
  const platform = usePlatform();
  const settings = useMemo(() => dictationClient().settings, []);
  const models = useMemo(createModelsBackend, []);
  const legalReceipt = useMemo(() => createLegalReceipt(acceptance.isCurrent), [acceptance.isCurrent]);
  const recognitionReady = useRef(false);
  // The model download starts once per mount, even when the step unmounts on Back and Next.
  const modelDownloadStarted = useRef(false);
  // Setup leaves the model step by itself once, when the download installs; Back to it later stays put.
  const modelAutoAdvanced = useRef(false);
  const steps = useMemo<readonly OnboardingStep[]>(() => {
    // Each app step draws its own hero heading (the approved v5 design); an empty title hides the frame's repeat of it.
    // The description carries "Step N of M" over the visible steps (the frame's own count includes the hidden agreement).
    const appStep = (id: string, label: string): OnboardingStep => {
      const blockedHint = blockedHintFor(platform, id);
      return {
        id, label, title: "", description: stepProgressLabel(id),
        // Microphone, Shortcuts and Voice check hold Continue until the step reports ready.
        ...(stepHoldsContinue(id) ? { canContinue: (context: OnboardingStepContext) => context.ready[id] === true } : {}),
        ...(blockedHint ? { blockedHint } : {}),
        render: (context: OnboardingStepContext) => <AppStep id={id} context={context} platform={platform} recognitionReady={recognitionReady} />,
      };
    };
    // Confirm the model is installed, select it, and start the engine.
    const activateModel = async (context: OnboardingStepContext) => {
      const { id, label } = onboardingModel();
      const status = await speechClient().localAsrModelStatus(id);
      if (!status.installed) throw new Error(`Install ${label} before continuing setup.`);
      await context.settings.patch({ asr_backend: id, dictation_language: "auto" });
      void speechClient().prewarmEngine().catch(() => undefined);
    };
    const advanceFromModel = async (context: OnboardingStepContext, isActive: () => boolean) => {
      if (modelAutoAdvanced.current || !isActive()) return;
      modelAutoAdvanced.current = true;
      try {
        await activateModel(context);
      } catch {
        // Not installed after all: stay here; the Continue button reports the reason.
        modelAutoAdvanced.current = false;
        return;
      }
      // The user may have gone Back or clicked the rail while the activation ran: leave them where they went.
      if (!isActive()) {
        modelAutoAdvanced.current = false;
        return;
      }
      await context.setState(modelAdvancePatch(context.state.completed));
    };
    const download = modelsStep(models, {
      // One step: what the model is (intro), then its download row. The intro draws its own heading.
      label: "Model",
      title: "",
      description: stepProgressLabel(MODEL_STEP_ID),
      intro: <ModelIntro />,
      required: true,
      // Start the download when the step opens; an already-installed model is left alone.
      autoStartDownload: true,
      startGuard: modelDownloadStarted,
    });
    const modelStep: OnboardingStep = {
      ...download,
      // The model is required, so this step has no "Skip step", and setup has no "Set up later" either.
      skippable: false,
      // Leaving the model step by Continue.
      onContinue: activateModel,
      render: (context: OnboardingStepContext) => (
        <div className="grid w-full justify-items-center gap-3">
          {download.render(context)}
          <ModelDownloadExtras models={models} onInstalled={(isActive) => void advanceFromModel(context, isActive)} />
        </div>
      ),
    };
    return ONBOARDING_STEPS.map(([id, label]) => (id === MODEL_STEP_ID ? modelStep : appStep(id, label)));
  }, [models, platform]);

  async function finish() {
    if (!recognitionReady.current) {
      throw new Error("Complete recognition setup before finishing onboarding.");
    }
    const native = onboardingClient();
    await native.applySettingsPatch({
      onboarding_complete: true,
      onboarding_version: hostOnboarding().version ?? 3,
      transcript_cleanup: "aggressive",
      pill_visibility: "always",
    });
    const finishAside = Promise.allSettled([native.setShortcutsActive(false), native.revealPill()]);
    const hotkeysReady = await dictationClient().registerHotkeys().then(() => true, () => false);
    await finishAside;
    if (hotkeysReady) await native.parkIdleHub().catch(() => undefined);
    onComplete();
  }

  return (
    <RightKitOnboarding
      settings={settings}
      legal={{ ...legal, appName: hostAppName(), trustLine: legal.trustLine ?? PRIVACY_LINE }}
      legalReceipt={legalReceipt}
      requiredVersion={acceptance.version}
      requiredDocumentsHash={acceptance.documentsHash}
      documentsHash={acceptance.documentsHash}
      receiptDocuments={legal.documents}
      steps={steps}
      onComplete={finish}
      allowSkip={false}
      className="hr-rightkit-onboarding"
      // Frameless window: the strip is the drag handle on every OS. macOS keeps its native traffic lights;
      // Windows gets Minimise and Close (no Maximise) at the strip's right edge, outside the drag target.
      dragRegion
      legalDragRegion
      titlebar={titlebar}
    />
  );
}
