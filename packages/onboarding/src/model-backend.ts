// A single-model `ModelsBackend` over native status, install and event commands, plus the "install, then select and
// start it, then move on once" activation the model step needs. Ids, labels, commands and wording are inputs.
import type { ModelEntry, ModelsBackend } from "./types.js";

/** What the host says about the one model. */
export interface SingleModelConfig {
  id: string;
  label: string;
  /** Row description when the event carries no message. */
  description?: string;
  /** Error message when a failed event carries none. Default "Model download failed.". */
  failedMessage?: string;
}

/** The native download event payload the mapping reads. Extra fields are ignored. */
export interface ModelDownloadEvent {
  phase: string;
  received: number;
  total: number;
  message?: string | null;
}

/** The row for the model from a status answer. `installed` is the native readiness result, wherever the files live. */
export function modelEntryFromStatus(config: SingleModelConfig, status: { installed: boolean }): ModelEntry {
  return {
    id: config.id,
    label: config.label,
    description: config.description,
    state: status.installed ? "installed" : "missing",
  };
}

/**
 * The row for a download event. Phases: `downloading` and `preparing` are downloading; `extracting` and `verifying`
 * are verifying (they report no byte total, so no progress and the step keeps the bar's place); `error` is an error;
 * `installed` is installed; any other phase is missing.
 */
export function modelEntryFromEvent(config: SingleModelConfig, payload: ModelDownloadEvent): ModelEntry {
  const { phase, received, total, message } = payload;
  const downloading = phase === "downloading" || phase === "preparing";
  const verifying = phase === "extracting" || phase === "verifying";
  const failed = phase === "error";
  return {
    id: config.id,
    label: config.label,
    description: message ?? config.description,
    sizeBytes: total > 0 ? total : undefined,
    state: phase === "installed" ? "installed" : failed ? "error" : verifying ? "verifying" : downloading ? "downloading" : "missing",
    progress: total > 0 ? received / total : undefined,
    error: failed ? { code: "download", message: message ?? config.failedMessage ?? "Model download failed." } : undefined,
  };
}

export interface InvokeModelsBackendOptions extends SingleModelConfig {
  /** Native readiness check for the model. */
  status(): Promise<{ installed: boolean }>;
  /** Starts the native download. */
  install(): Promise<unknown>;
  /** Registers a handler for download events and resolves the unsubscribe. */
  subscribe(handler: (payload: ModelDownloadEvent & { backend?: string }) => void): Promise<() => void>;
  /** True for events that belong to this model. Default: `payload.backend === id` (events without `backend` pass). */
  matches?(payload: ModelDownloadEvent & { backend?: string }): boolean;
}

/**
 * `ModelsBackend` for one model. There is no `cancel`: a native download with no cancel command draws no Pause button.
 * `list` and the event handler use `modelEntryFromStatus` and `modelEntryFromEvent`.
 */
export function createSingleModelBackend(options: InvokeModelsBackendOptions): ModelsBackend {
  const matches = options.matches ?? ((p) => p.backend === undefined || p.backend === options.id);
  return {
    async list() {
      return [modelEntryFromStatus(options, await options.status())];
    },
    async install() {
      await options.install();
    },
    async subscribe(handler) {
      return options.subscribe((payload) => {
        if (!matches(payload)) return;
        handler(modelEntryFromEvent(options, payload));
      });
    },
  };
}

export interface ModelActivationOptions {
  /** Native readiness check. */
  isInstalled(): Promise<boolean>;
  /** Thrown message when the model is not installed. */
  notInstalledMessage: string;
  /** Selects the model (a settings patch). */
  select(): Promise<unknown>;
  /** Starts the engine after selecting; a failure is ignored. */
  prewarm?(): Promise<unknown>;
}

/** Confirms the model is installed, selects it, then starts the engine without waiting for it. Throws when not installed. */
export async function activateInstalledModel(options: ModelActivationOptions): Promise<void> {
  if (!(await options.isInstalled())) throw new Error(options.notInstalledMessage);
  await options.select();
  void options.prewarm?.().catch(() => undefined);
}

/**
 * The model step's one-time automatic advance, for `onInstalledInSession`. Create it once per setup mount. The returned
 * function activates the model and then calls `advance`, once: if the activation throws (not installed after all) or
 * the user left the step meanwhile (`isActive()` false) it stays re-armable, and a second call after success does nothing.
 */
export function createModelAutoAdvance<C>(options: {
  activate(context: C): Promise<void>;
  advance(context: C): Promise<unknown>;
}): (context: C, isActive: () => boolean) => Promise<void> {
  let advanced = false;
  return async (context, isActive) => {
    if (advanced || !isActive()) return;
    advanced = true;
    try {
      await options.activate(context);
    } catch {
      advanced = false;
      return;
    }
    if (!isActive()) {
      advanced = false;
      return;
    }
    await options.advance(context);
  };
}
