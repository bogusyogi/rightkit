import { useCallback, useEffect, useState, type ReactNode } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  buildProviderCardSnapshots,
  getProviderDisplay,
  readyOnlyChildPolicy,
  shortLabelForProvider,
  type CliProviderCardSnapshot,
} from "@rightkit/agent-ui-core/lib/cliProviders";
import type { DaemonConfig, OrchestrationSettings } from "@rightkit/agent-ui-core/lib/types";
import type { CliDelegationPolicy } from "@rightkit/agent-ui-core/lib/types/cliProviders";
import {
  getCliDelegationPolicy,
  listCliProviders,
  mutateCliProvider,
  setCliDelegationPolicy,
  type CliProviderMutation,
} from "@rightkit/agent-ui-core/lib/daemon/cli-providers";
import ChildProviderPolicy from "./ChildProviderPolicy";
import ApiKeyAccounts from "./ApiKeyAccounts";
import type { CatalogGroup, DirectConnection, LoadSnapshot } from "@rightkit/agent-ui-core/lib/types/inference";
import {
  createDirectConnection,
  deleteDirectConnection,
  getInferenceLoad,
  listInferenceCatalog,
  listInferenceConnections,
  probeInferenceConnection,
} from "@rightkit/agent-ui-core/lib/daemon/inference";
import { hostAppName } from "../../../appName";

function Disclosure({ label, children }: { label: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  return (
    <div className="rounded-xl border border-line">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="flex w-full items-center justify-between gap-2 px-4 py-2.5 text-[13px] font-semibold text-text-2 cursor-pointer border-0 bg-transparent hover:text-text focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
      >
        <span>{label}</span>
        <span
          aria-hidden="true"
          className={`transition-transform duration-150 ${open ? "rotate-180" : ""}`}
        >
          <Icon name="chevron" size={12} />
        </span>
      </button>
      {open ? <div className="flex flex-col gap-2 border-t border-line p-4">{children}</div> : null}
    </div>
  );
}

interface ProvidersTabProps {
  settings: OrchestrationSettings | null;
  config?: DaemonConfig | null;
  sessionId?: string | null;
  onLoginCliProvider?: (provider: string) => void;
  onRefreshAttestations?: () => void;
}

function eligibilityBadgeFor(snapshot: CliProviderCardSnapshot): { label: string; tone: string } {
  switch (snapshot.eligibility.kind) {
    case "ready":
      return { label: "Ready", tone: "border-success/40 text-success" };
    case "compatibility_read_only":
      return { label: "Compatibility", tone: "border-warning/40 text-warning" };
    case "login_required":
      return { label: "Login required", tone: "border-warning/40 text-warning" };
    case "harness_verification_required":
      return { label: "Harness verification", tone: "border-warning/40 text-warning" };
    case "version_unsupported":
      return { label: "Version unsupported", tone: "border-danger/40 text-danger" };
    case "unqualified":
      return { label: "Unqualified", tone: "border-danger/40 text-danger" };
    default:
      return { label: "Unknown", tone: "border-line-2 text-text-3" };
  }
}

function isolationLabelFor(mode: string): string {
  if (mode === "exclusive_coderight_gateway") return `${hostAppName()} gateway`;
  if (mode === "native_read_only_compatibility") return "Read-only compatibility";
  return mode;
}

function eligibilityReason(snapshot: CliProviderCardSnapshot): string | null {
  if (snapshot.eligibility.kind === "unqualified") {
    return snapshot.eligibility.reasons.join("; ");
  }
  if (snapshot.eligibility.kind === "compatibility_read_only") {
    return `Native vendor is read-only; ${hostAppName()} owns every stateful effect.`;
  }
  if (snapshot.eligibility.kind === "login_required") {
    return "Sign in via the vendor CLI to attach this provider.";
  }
  if (snapshot.eligibility.kind === "harness_verification_required") {
    return "Harness attestation pending — re-qualify once the binary is installed.";
  }
  return null;
}

export default function ProvidersTab({
  settings,
  config,
  sessionId,
  onLoginCliProvider,
}: ProvidersTabProps) {
  const [providers, setProviders] = useState<
    Awaited<ReturnType<typeof listCliProviders>>["providers"]
  >([]);
  const [attestationError, setAttestationError] = useState<string | null>(null);
  const [pendingProvider, setPendingProvider] = useState<string | null>(null);
  const [delegationPolicy, setDelegationPolicyState] = useState<CliDelegationPolicy | null>(null);
  const [connections, setConnections] = useState<DirectConnection[]>([]);
  const [catalogGroups, setCatalogGroups] = useState<CatalogGroup[]>([]);
  const [load, setLoad] = useState<LoadSnapshot | null>(null);

  useEffect(() => {
    let cancelled = false;
    const load = async () => {
      if (!config) return;
      try {
        const snapshot = await listCliProviders(config);
        if (!cancelled) {
          setProviders(snapshot.providers);
          setAttestationError(null);
        }
        if (!sessionId) {
          if (!cancelled) setDelegationPolicyState(null);
          return;
        }
        try {
          const policy = await getCliDelegationPolicy(config, sessionId);
          if (!cancelled) setDelegationPolicyState(policy);
        } catch (error) {
          if (!cancelled) {
            setDelegationPolicyState(null);
            setAttestationError(
              error instanceof Error ? error.message : "Delegation policy unavailable",
            );
          }
        }
      } catch (err) {
        if (!cancelled) {
          setAttestationError(err instanceof Error ? err.message : "unavailable");
        }
      }
    };
    void load();
    return () => {
      cancelled = true;
    };
  }, [config, sessionId]);

  const refreshInference = useCallback(async () => {
    if (!config) return;
    const [nextConnections, nextCatalog, nextLoad] = await Promise.all([
      listInferenceConnections(config),
      listInferenceCatalog(config),
      getInferenceLoad(config),
    ]);
    setConnections(nextConnections);
    setCatalogGroups(nextCatalog);
    setLoad(nextLoad);
  }, [config]);
  useEffect(() => {
    void refreshInference().catch((error) =>
      setAttestationError(
        error instanceof Error ? error.message : "Inference authority unavailable",
      ),
    );
  }, [refreshInference]);
  const addConnection = async () => {
    if (!config) return;
    try {
      await createDirectConnection(config, {
        name: "New Direct connection",
        protocol: "openai_compatible",
        normalizedBaseUrl: "http://127.0.0.1:11434",
        authKind: "none",
        authHeaderName: null,
        publicHeaders: {},
        manualModelIds: [],
        trustPrivateNetwork: false,
      });
      await refreshInference();
    } catch (error) {
      setAttestationError(
        error instanceof Error ? error.message : "Could not add Direct connection",
      );
    }
  };

  const updateDelegationPolicy = async (policy: CliDelegationPolicy) => {
    if (!config || !sessionId) return;
    policy = readyOnlyChildPolicy(policy, providers);
    const previous = delegationPolicy;
    setDelegationPolicyState(policy);
    try {
      setDelegationPolicyState(await setCliDelegationPolicy(config, sessionId, policy));
      setAttestationError(null);
    } catch (error) {
      setDelegationPolicyState(previous);
      setAttestationError(
        error instanceof Error ? error.message : "Delegation policy update failed",
      );
    }
  };

  const runProviderAction = async (
    provider: CliProviderCardSnapshot["providerId"],
    action: CliProviderMutation,
  ) => {
    if (!config) return;
    setPendingProvider(provider);
    setAttestationError(null);
    try {
      const snapshot = await mutateCliProvider(config, provider, action);
      setProviders(snapshot.providers);
      if (action === "login") onLoginCliProvider?.(provider);
    } catch (error) {
      setAttestationError(error instanceof Error ? error.message : "Provider action failed");
    } finally {
      setPendingProvider(null);
    }
  };

  const snapshots = buildProviderCardSnapshots({ providers });
  const configuredCount = snapshots.filter((s) => s.auth.launchReady).length;
  const detectedCount = snapshots.filter((s) => s.auth.detectedSignedIn).length;
  const summaryParts = [`${configuredCount} of ${snapshots.length} launch-ready`];
  if (detectedCount > 0)
    summaryParts.push(`${detectedCount} ambient sign-in${detectedCount === 1 ? "" : "s"} detected`);
  if (snapshots.length - configuredCount > 0) {
    summaryParts.push(
      `${snapshots.length - configuredCount} need${snapshots.length - configuredCount === 1 ? "s" : ""} routing or verification`,
    );
  }

  const providerCounts = new Map<string, number>();
  if (settings) {
    for (const model of settings.models) {
      providerCounts.set(model.provider, (providerCounts.get(model.provider) ?? 0) + 1);
    }
  }

  return (
    <section className="flex flex-col gap-5 p-6">
      <div className="flex flex-col gap-1">
        <h3 className="text-[16px] font-bold text-text">Providers</h3>
        <p className="text-[13px] font-semibold text-text">{summaryParts.join(" · ")}</p>
        <p className="text-[12px] text-text-3">
          Keep each CLI&apos;s authentication inside its own vendor login. {hostAppName()} stores only
          non-secret profile metadata. API keys go straight to the system credential store.
        </p>
        {attestationError ? (
          <p className="text-[11px] text-text-3">
            Harness attestations unavailable: {attestationError}
          </p>
        ) : null}
      </div>
      <section
        className="rounded-xl border border-line bg-panel p-4"
        aria-label="Inference authorities"
      >
        <div className="mb-3 flex items-center justify-between gap-3">
          <div>
            <h4 className="text-[13px] font-semibold text-text">Direct Connections</h4>
            <p className="text-[12px] text-text-3">
              OpenAI- & Anthropic-compatible endpoints. Keys remain in system keychain.
            </p>
          </div>
          <button
            type="button"
            onClick={() => void addConnection()}
            className="rounded-lg border border-line px-3 py-2 text-[12px] text-text hover:bg-panel-2"
          >
            Add connection
          </button>
        </div>
        <div className="flex flex-col gap-2">
          {connections.map((connection) => (
            <article
              key={connection.id}
              className="flex items-center gap-3 rounded-lg border border-line/70 px-3 py-2"
            >
              <div className="min-w-0 flex-1">
                <strong className="block truncate text-[12px] text-text">{connection.name}</strong>
                <span className="block truncate text-[11px] text-text-3">
                  {connection.protocol.replace("_", " ")} · {connection.normalizedBaseUrl} ·{" "}
                  {connection.credentialConfigured ? "key configured" : "no key"}
                </span>
              </div>
              <button
                type="button"
                className="rounded border border-line px-2 py-1 text-[11px] text-text-2"
                onClick={() =>
                  config &&
                  void probeInferenceConnection(config, "direct", connection.id)
                    .then(refreshInference)
                    .catch((error) =>
                      setAttestationError(error instanceof Error ? error.message : "Probe failed"),
                    )
                }
              >
                Probe
              </button>
              <button
                type="button"
                className="rounded border border-danger/40 px-2 py-1 text-[11px] text-danger"
                onClick={() =>
                  config &&
                  void deleteDirectConnection(config, connection.id)
                    .then(refreshInference)
                    .catch((error) =>
                      setAttestationError(error instanceof Error ? error.message : "Delete failed"),
                    )
                }
              >
                Delete
              </button>
            </article>
          ))}
        </div>
        {load ? (
          <p className="mt-3 text-[12px] text-text-3" aria-live="polite">
            {load.activeAgents}/{load.activeLimit} active · {load.queuedAgents} queued
          </p>
        ) : null}
      </section>
      <ApiKeyAccounts config={config} />
      <div className="grid gap-3">
        {snapshots.map((snapshot) => (
          <ProviderCard
            key={snapshot.providerId}
            snapshot={snapshot}
            pending={pendingProvider === snapshot.providerId}
            onAction={runProviderAction}
          />
        ))}
      </div>
      <Disclosure label="Advanced: model → provider mapping">
        {providerCounts.size > 0 ? (
          [...providerCounts.entries()].map(([name, count]) => (
            <article
              className="grid grid-cols-[minmax(0,1fr)_240px] items-center gap-3 px-1 h-9 border-b border-line/60 last:border-b-0"
              key={name}
            >
              <div className="flex flex-col min-w-0">
                <strong className="text-[13px] font-semibold text-text truncate">
                  {shortLabelForProvider(name)}
                </strong>
              </div>
              <div className="flex items-center gap-2 text-[12px] text-text-3">
                <span className="truncate">
                  {count} model{count !== 1 ? "s" : ""} configured
                </span>
                <code className="text-[11px] font-mono text-accent-2">configured</code>
              </div>
            </article>
          ))
        ) : (
          <p className="text-[13px] text-text-3">No providers configured.</p>
        )}
      </Disclosure>
      {delegationPolicy ? (
        <Disclosure label="Child provider policy">
          <ChildProviderPolicy
            policy={delegationPolicy}
            onChange={(policy) => void updateDelegationPolicy(policy)}
          />
        </Disclosure>
      ) : null}
    </section>
  );
}

function ProviderCard({
  snapshot,
  pending,
  onAction,
}: {
  snapshot: CliProviderCardSnapshot;
  pending: boolean;
  onAction: (provider: CliProviderCardSnapshot["providerId"], action: CliProviderMutation) => void;
}) {
  const display = getProviderDisplay(snapshot.providerId);
  const badge = eligibilityBadgeFor(snapshot);
  const reason = eligibilityReason(snapshot);
  const status = snapshot.auth.launchReady
    ? "Launch-ready"
    : snapshot.auth.detectedSignedIn
      ? `Signed in outside ${hostAppName()}`
      : "No vendor sign-in detected";
  const versionLabel = snapshot.attestation?.attestation.binaryVersion ?? "—";
  const isolatedMode = snapshot.attestation?.attestation.isolationMode ?? display.isolationMode;

  return (
    <article
      className={`rounded-xl border p-4 ${snapshot.eligibility.kind === "ready" ? "border-accent/50 bg-panel" : "border-line/70 bg-panel/70"}`}
      key={snapshot.providerId}
      data-testid={`provider-card-${snapshot.providerId}`}
    >
      <div className="mb-3 flex items-start justify-between gap-3">
        <div className="min-w-0">
          <strong className="text-[13px] font-semibold text-text">{display.displayName}</strong>
          <p className="mt-0.5 truncate text-[12px] text-text-3">{status}</p>
          {snapshot.profile?.accountFingerprint ? (
            <p className="mt-0.5 text-[11px] font-mono text-text-3">
              fingerprint · {snapshot.profile.accountFingerprint}
            </p>
          ) : null}
        </div>
        <div className="flex flex-col items-end gap-1">
          <span
            className={`rounded-full border px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide ${badge.tone}`}
            data-testid={`provider-card-${snapshot.providerId}-eligibility`}
          >
            {badge.label}
          </span>
          <span className="rounded-full border border-line-2 px-2 py-0.5 text-[10px] font-mono text-text-3">
            {isolationLabelFor(isolatedMode)}
          </span>
        </div>
      </div>
      <dl className="mb-3 grid grid-cols-2 gap-x-4 gap-y-1 text-[12px]">
        <dt className="text-text-3">Binary version</dt>
        <dd className="truncate font-mono text-text-2">{versionLabel}</dd>
        <dt className="text-text-3">Harness status</dt>
        <dd className="truncate text-text-2">
          {snapshot.attestation
            ? snapshot.attestation.qualified
              ? `Verified ${snapshot.attestation.ageLabel ?? "recently"}`
              : `Failed probes (${snapshot.attestation.attestation.failedProbes.length})`
            : "Awaiting first attestation"}
        </dd>
      </dl>
      {reason ? (
        <p className="mb-3 rounded-md border border-line-2 bg-bg/40 p-2 text-[12px] text-text-2">
          {reason}
        </p>
      ) : null}
      <button
        className={
          snapshot.eligibility.kind === "ready"
            ? "rounded-lg bg-accent px-3 py-2 text-[12px] font-semibold text-on-accent disabled:opacity-40"
            : "rounded-lg border border-line px-3 py-2 text-[12px] font-semibold text-text-2 hover:bg-panel hover:text-text disabled:opacity-40"
        }
        disabled={pending || !snapshot.binaryInstalled}
        onClick={() =>
          onAction(
            snapshot.providerId,
            snapshot.auth.state === "unqualified" ? "verify-harness" : "login",
          )
        }
        type="button"
        data-testid={`provider-card-${snapshot.providerId}-sign-in`}
      >
        {pending
          ? "Working…"
          : snapshot.auth.state === "unqualified"
            ? "Verify harness"
            : `Sign in with ${display.shortLabel}`}
      </button>
    </article>
  );
}
