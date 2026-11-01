import { useCallback, useEffect, useRef, useState } from "react";
import { usePersistedState } from "@rightkit/react-shell/react";
import {
  getProviderKeyStatus,
  isTauriHost,
  DAEMON_CONFIG_KEY,
  daemonConfigFromEnv,
  restartDaemon,
  saveProviderKey,
  removeProviderKey,
  startDaemon,
} from "@rightkit/agent-ui-core/lib/daemon";
import type { ProviderKeyStatus } from "@rightkit/agent-ui-core/lib/daemon";
import type { DaemonConfig, HealthResponse } from "@rightkit/agent-ui-core/lib/types";
import { createConnectionAttemptState } from "@rightkit/agent-ui-core/lib/connection/connectionAttemptState";
import {
  projectConnectionError,
  type ConnectionErrorProjection,
} from "@rightkit/agent-ui-core/lib/connection/connectionErrorProjection";
import { checkKeyFormat, SETTINGS_PROVIDER_PRESETS } from "@rightkit/agent-ui-core/lib/providerPresets";
import type { KeyFormatCheck } from "@rightkit/agent-ui-core/lib/providerPresets";
import { classifyStatusForToast } from "@rightkit/agent-ui-core/lib/statusToast";
import { toast } from "@rightkit/react-shell";

export const PROVIDER_OPTIONS = [
  {
    id: "openrouter",
    label: "OpenRouter",
    env: "OPENROUTER_API_KEY",
    required: false,
  },
  {
    id: "minimax",
    label: "MiniMax",
    env: "MINIMAX_API_KEY",
    required: false,
  },
  {
    id: "tinyfish",
    label: "TinyFish Search",
    env: "TINYFISH_API_KEY",
    required: false,
  },
];

export const FIRST_RUN_PROVIDER_IDS = new Set(["openrouter", "minimax"]);

export const ONBOARDING_MODEL_KEY = "coderight.onboarding.preferredModel";
export const CONDUCTOR_MODEL_KEY = "coderight.conductorModel";

const STEADY_LABELS = new Set(["ready", "preview mode"]);
function isSteadyLabel(label: string) {
  return STEADY_LABELS.has(label.trim().toLowerCase());
}

function isStaticPreview() {
  return !isTauriHost() && new URLSearchParams(window.location.search).get("qa") === "1";
}

function staticPreviewStatus() {
  const state = new URLSearchParams(window.location.search).get("qaState");
  if (state === "disconnected") return "Local engine unavailable";
  if (state === "loading") return "Starting local engine...";
  if (state === "stale") return "Connection restored — data may be stale";
  return "Preview mode";
}

export function hasProviderCredential(
  statuses: ProviderKeyStatus[],
  drafts: Record<string, string> = {},
): boolean {
  return Array.from(FIRST_RUN_PROVIDER_IDS).some(
    (providerId) =>
      statuses.some((row) => row.provider === providerId && row.configured) ||
      Boolean((drafts[providerId] ?? "").trim()),
  );
}

interface UseDaemonConnectionOptions {
  formatError: (error: unknown, fallback: string) => string;
  refresh: (config: DaemonConfig) => Promise<void>;
}

export function useDaemonConnection({ formatError, refresh }: UseDaemonConnectionOptions) {
  // The Tauri host issues its bearer token per launch and keeps it in memory only (no storage key);
  // the browser host remembers the user-entered connection across reloads (RightKit usePersistedState).
  // A build-time dev/QA connection wins and is never stored.
  const [envConfig] = useState(() => (isTauriHost() || isStaticPreview() ? null : daemonConfigFromEnv()));
  const [config, setConfig] = usePersistedState<DaemonConfig | null>(
    isTauriHost() || isStaticPreview() || envConfig ? null : DAEMON_CONFIG_KEY,
    envConfig,
  );
  const [health, setHealth] = useState<HealthResponse | null>(null);
  const [connectionProjection, setConnectionProjection] =
    useState<ConnectionErrorProjection | null>(null);
  const [urlDraft, setUrlDraft] = useState(config?.url ?? "http://127.0.0.1:5057");
  const [tokenDraft, setTokenDraft] = useState(config?.token ?? "");
  const [status, setStatusRaw] = useState(
    isStaticPreview()
      ? staticPreviewStatus()
      : config
        ? "Connecting..."
        : "Starting local engine...",
  );
  const [providerKeyStatuses, setProviderKeyStatuses] = useState<ProviderKeyStatus[]>([]);
  const [providerKeyDrafts, setProviderKeyDrafts] = useState<Record<string, string>>({});
  const [providerKeySaving, setProviderKeySaving] = useState<string | null>(null);
  const [providerKeyValidation, setProviderKeyValidation] = useState<
    Record<string, KeyFormatCheck>
  >({});
  const [onboardingModelDraft, setOnboardingModelDraft] = useState(() => {
    try {
      return localStorage.getItem(ONBOARDING_MODEL_KEY) ?? "";
    } catch {
      return "";
    }
  });
  const [showAdvancedConnect, setShowAdvancedConnect] = useState(
    new URLSearchParams(window.location.search).get("advanced") === "1",
  );
  const autoStartAttempted = useRef(false);
  const connectionAttempts = useRef(createConnectionAttemptState<void, DaemonConfig | void>());

  function beginConnectionAttempt() {
    connectionAttempts.current.invalidate();
    setConnectionProjection(null);
    return connectionAttempts.current.startAttempt();
  }

  function connectionTarget(nextConfig: DaemonConfig): "local" | "remote" {
    try {
      const hostname = new URL(nextConfig.url).hostname.toLowerCase();
      return hostname === "127.0.0.1" || hostname === "localhost" || hostname === "::1"
        ? "local"
        : "remote";
    } catch {
      return "local";
    }
  }

  async function refreshWithAttempt(nextConfig: DaemonConfig) {
    const attempt = beginConnectionAttempt();
    const refreshPromise = Promise.resolve().then(() => refresh(nextConfig));
    if (!connectionAttempts.current.setPromise(attempt, refreshPromise)) return;
    try {
      await refreshPromise;
      if (connectionAttempts.current.isCurrentAttempt(attempt)) {
        setConnectionProjection(null);
      }
    } catch (err) {
      if (!connectionAttempts.current.isCurrentAttempt(attempt)) return;
      const projection = projectConnectionError(err, connectionTarget(nextConfig));
      setConnectionProjection(projection);
      setStatus(projection.message);
    } finally {
      connectionAttempts.current.clearPromiseForAttempt(attempt);
    }
  }

  // biome-ignore lint/correctness/useExhaustiveDependencies: initial daemon boot should run once; refresh is supplied by the app shell and reads current state.
  useEffect(() => {
    if (isStaticPreview()) {
      setStatus(staticPreviewStatus());
      return () => {
        connectionAttempts.current.invalidate();
      };
    }

    if (config) {
      void refreshWithAttempt(config);
      return () => {
        connectionAttempts.current.invalidate();
      };
    }

    if (autoStartAttempted.current) return;
    autoStartAttempted.current = true;

    if (isTauriHost()) {
      void launchDaemon();
    } else {
      setStatus(staticPreviewStatus());
    }
    return () => {
      connectionAttempts.current.invalidate();
    };
  }, []);

  // biome-ignore lint/correctness/useExhaustiveDependencies: provider key refresh is derived only from the active daemon config.
  useEffect(() => {
    if (isTauriHost() && config) {
      void refreshProviderKeyStatus(config);
    }
  }, [config]);

  function applyConfig(next: DaemonConfig) {
    connectionAttempts.current.invalidate();
    setConnectionProjection(null);
    setConfig(next);
    setUrlDraft(next.url);
    setTokenDraft(next.token);
  }

  function connectTo(next: DaemonConfig) {
    applyConfig(next);
    void refreshWithAttempt(next);
  }

  function connect() {
    connectTo({ url: urlDraft.trim(), token: tokenDraft.trim() });
  }

  async function launchDaemon() {
    const attempt = beginConnectionAttempt();
    try {
      setStatus("Starting local engine...");
      const startPromise = Promise.resolve().then(() => startDaemon());
      if (!connectionAttempts.current.setPromise(attempt, startPromise)) return;
      const next = await startPromise;
      if (!connectionAttempts.current.isCurrentAttempt(attempt)) return;
      applyConfig(next);
      void refreshProviderKeyStatus(next);
      await refreshWithAttempt(next);
    } catch (err) {
      if (!connectionAttempts.current.isCurrentAttempt(attempt)) return;
      setHealth(null);
      const projection = projectConnectionError(err, "local");
      setConnectionProjection(projection);
      setStatus(projection.message);
    } finally {
      connectionAttempts.current.clearPromiseForAttempt(attempt);
    }
  }

  async function maybeLaunchAfterRefreshFailure() {
    if (!isTauriHost() || autoStartAttempted.current) return false;
    autoStartAttempted.current = true;
    await launchDaemon();
    return true;
  }

  async function refreshProviderKeyStatus(nextConfig = config) {
    if (!nextConfig) return;
    try {
      setProviderKeyStatuses(
        await Promise.all(
          PROVIDER_OPTIONS.map((provider) => getProviderKeyStatus(nextConfig, provider.id)),
        ),
      );
    } catch {
      setProviderKeyStatuses([]);
    }
  }

  /** Instant offline format check, run on every keystroke — no network call. */
  function validateProviderKeyDraft(providerId: string, value: string) {
    const preset = SETTINGS_PROVIDER_PRESETS.find((row) => row.id === providerId);
    if (!preset) return;
    setProviderKeyValidation((current) => ({
      ...current,
      [providerId]: checkKeyFormat(preset, value),
    }));
  }

  const setOnboardingModel = useCallback((modelId: string) => {
    setOnboardingModelDraft(modelId);
    try {
      localStorage.setItem(ONBOARDING_MODEL_KEY, modelId);
      localStorage.setItem(CONDUCTOR_MODEL_KEY, modelId);
    } catch {
      // localStorage unavailable (private mode / static preview) — non-fatal, model
      // preference just won't persist across reloads.
    }
  }, []);

  /** Save whatever keys were entered on the "keys" step, then advance to model selection.
   * Resolves true on success or when there is nothing to save, false when saving failed. */
  async function saveProviderKeysAndContinue(): Promise<boolean> {
    if (!config) {
      setStatus("Local engine unavailable");
      return false;
    }

    const providersToSave = PROVIDER_OPTIONS.map((provider) => ({
      provider: provider.id,
      key: (providerKeyDrafts[provider.id] ?? "").trim(),
    })).filter(({ key }) => key.length > 0);

    if (providersToSave.length === 0) {
      // Nothing new typed — either already configured (skip-through) or genuinely empty.
      return true;
    }

    try {
      setProviderKeySaving("onboarding");
      const saved = await Promise.all(
        providersToSave.map(({ provider, key }) => saveProviderKey(config, provider, key)),
      );
      setProviderKeyStatuses((rows) => [
        ...rows.filter((row) => !saved.some((savedRow) => savedRow.provider === row.provider)),
        ...saved,
      ]);
      // Re-check format against the now-persisted value so the card reflects the saved
      // state, not just the last keystroke.
      for (const { provider, key } of providersToSave) {
        validateProviderKeyDraft(provider, key);
      }
      setProviderKeyDrafts({});
      return true;
    } catch (err) {
      setStatus(formatError(err, "Could not save provider key"));
      return false;
    } finally {
      setProviderKeySaving(null);
    }
  }

  async function saveProviderKeyDraft(provider: string) {
    if (!config) {
      setStatus("Local engine unavailable");
      return;
    }
    const key = (providerKeyDrafts[provider] ?? "").trim();
    const preset = SETTINGS_PROVIDER_PRESETS.find((row) => row.id === provider);
    if (!preset || !key) {
      setStatus("Enter a provider key first");
      return;
    }
    const check = checkKeyFormat(preset, key);
    setProviderKeyValidation((current) => ({ ...current, [provider]: check }));
    if (check.status !== "valid") {
      setStatus(check.message || "Provider key is invalid");
      return;
    }

    try {
      setProviderKeySaving(provider);
      const saved = await saveProviderKey(config, provider, key);
      setProviderKeyStatuses((rows) => [...rows.filter((row) => row.provider !== provider), saved]);
      setProviderKeyDrafts((drafts) => ({ ...drafts, [provider]: "" }));
      setStatus(`${preset.label} key saved securely`);
    } catch (err) {
      setStatus(formatError(err, `Could not save ${preset.label} key`));
    } finally {
      setProviderKeySaving(null);
    }
  }

  async function removeProviderKeyDraft(provider: string) {
    if (!config) {
      setStatus("Local engine unavailable");
      return false;
    }
    const preset = SETTINGS_PROVIDER_PRESETS.find((row) => row.id === provider);
    try {
      setProviderKeySaving(provider);
      const cleared = await removeProviderKey(config, provider);
      setProviderKeyStatuses((rows) => [
        ...rows.filter((row) => row.provider !== provider),
        cleared,
      ]);
      setProviderKeyDrafts((drafts) => ({ ...drafts, [provider]: "" }));
      setStatus(`${preset?.label ?? provider} key removed`);
    } catch (err) {
      setStatus(formatError(err, `Could not remove ${preset?.label ?? provider} key`));
    } finally {
      setProviderKeySaving(null);
    }
  }

  // Transient event statuses ("Session deleted", "Conductor model set to …",
  // "Session created", etc.) auto-clear back to the steady "Ready" label after
  // <=5s. Steady labels (Ready, Preview mode, Connecting..., starting/error
  // states) and the user's explicit typing are left alone. The pending timer
  // is reset on every status write so a fresh event gets its own 5s window.
  const transientTimer = useRef<number | null>(null);
  // Guard against duplicate identical statuses to avoid toast/timer spam and
  // unnecessary renders when a failing effect calls setStatus repeatedly.
  const lastStatusRef = useRef<string | null>(null);
  const setStatus = useCallback((next: string) => {
    if (lastStatusRef.current === next) return;
    lastStatusRef.current = next;
    setStatusRaw(next);
    const toastVariant = classifyStatusForToast(next);
    if (toastVariant) toast(next, toastVariant);
    if (typeof window === "undefined") return;
    if (transientTimer.current !== null) {
      window.clearTimeout(transientTimer.current);
      transientTimer.current = null;
    }
    if (isSteadyLabel(next)) return;
    // Don't auto-clear explicit in-progress / failure labels the boot path sets.
    const lower = next.trim().toLowerCase();
    if (
      lower.includes("connecting") ||
      lower.includes("starting") ||
      lower.includes("reloading") ||
      lower.includes("failed") ||
      lower.includes("unavailable") ||
      lower.includes("error") ||
      lower.includes("choose a workspace") ||
      lower.includes("add at least one provider")
    ) {
      return;
    }
    transientTimer.current = window.setTimeout(() => {
      setStatusRaw("Ready");
      transientTimer.current = null;
    }, 5000);
  }, []);
  useEffect(() => {
    return () => {
      if (transientTimer.current !== null && typeof window !== "undefined") {
        window.clearTimeout(transientTimer.current);
      }
    };
  }, []);

  return {
    config,
    applyConfig,
    health,
    setHealth,
    connectionProjection,
    status,
    setStatus,
    urlDraft,
    setUrlDraft,
    tokenDraft,
    setTokenDraft,
    showAdvancedConnect,
    setShowAdvancedConnect,
    providerKeyStatuses,
    providerKeyDrafts,
    setProviderKeyDrafts,
    providerKeySaving,
    providerKeyValidation,
    validateProviderKeyDraft,
    onboardingModelDraft,
    setOnboardingModel,
    connect,
    connectTo,
    launchDaemon,
    maybeLaunchAfterRefreshFailure,
    saveProviderKeysAndContinue,
    saveProviderKeyDraft,
    removeProviderKeyDraft,
  };
}
