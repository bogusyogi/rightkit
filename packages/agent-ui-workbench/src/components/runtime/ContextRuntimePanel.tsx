import Icon from "@rightkit/agent-ui-core/components/Icon";
import { EmptyState } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useState } from "react";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";
import RuntimeStat from "./RuntimeStat.js";

type ContextState = "available" | "advertised" | "degraded" | "unavailable";
type ContextComponent = { name: string; state: ContextState; reason?: string };
type ResourceMetrics = {
  membraneBinding?: {
    bound?: boolean;
    mode?: string;
    serviceId?: string;
    installationId?: string;
    cortexStoreId?: string;
    releaseGeneration?: string;
    startupGeneration?: number;
    runtimeOrigin?: string;
    subsystems?: string[];
    note?: string;
  };
  daemon?: { rssBytes?: number };
  turnLatency?: { available?: boolean; samples?: number; p95Ms?: number | null };
  roundTripsPerTask?: { available?: boolean; tasks?: number; total?: number };
  membraneEvidence?: {
    h4?: { toolSchemaObservations?: number; proceduralAssetObservations?: number };
    h8?: { remainingContextCeilings?: unknown[] };
  };
};
type HubSection = {
  state?: string;
  reason?: string;
  items?: Array<Record<string, unknown>>;
  observedAtUnixMs?: number;
  cacheAgeMs?: number;
};
type HubSnapshot = {
  observedAtUnixMs?: number;
  sections?: Record<string, HubSection>;
  membraneState?: string;
  subsystems?: Record<string, { state?: string; reason?: string }>;
  admission?: {
    decisionsTotal?: number;
    omissionsTotal?: number;
    budgetPressureTotal?: number;
    lastPullStatus?: string;
    lastPullReason?: string;
  };
};
type ContextStatusResponse = {
  state?: string;
  reason?: string;
  snapshot?: HubSnapshot;
};
type AdaptStatus = {
  contract?: string;
  qualified?: boolean;
  lanes?: {
    review?: { pending_count?: number; reason?: string };
    insights?: { workload?: string; coverage_windows_seen?: number; reason?: string };
    admission?: { reachable?: boolean; owner?: string };
  };
  observed_at_ms?: number;
};

type PreferenceManifest = {
  schema_version: string;
  records: unknown[];
  manifest_sha256: string;
  installation_id?: string;
  canonical_pool_sha256?: string;
};

type AdaptAdmissionReceipt = {
  manifest: PreferenceManifest;
  manifestPath: string;
  statusFingerprint: string;
  sourceIdentity: string;
};

export function preferenceManifest(value: Record<string, unknown>): PreferenceManifest | null {
  return typeof value.schema_version === "string" &&
    Array.isArray(value.records) &&
    typeof value.manifest_sha256 === "string"
    ? (value as unknown as PreferenceManifest)
    : null;
}

function adaptStatusFingerprint(status: AdaptStatus | null): string {
  return JSON.stringify({
    qualified: status?.qualified ?? null,
    reachable: status?.lanes?.admission?.reachable ?? null,
  });
}

export function validateApplyResponse(payload: Record<string, unknown>): void {
  const response = payload.response;
  if (!response || typeof response !== "object" || Array.isArray(response)) {
    throw new Error("Apply response omitted ApplyResponse");
  }
  const apply = response as Record<string, unknown>;
  if (apply.valid !== true) {
    const errors = Array.isArray(apply.errors) ? apply.errors.join("; ") : "manifest rejected";
    throw new Error(errors);
  }
  const accepted = apply.accepted_record_ids;
  if (!Array.isArray(accepted)) throw new Error("Apply response omitted accepted_record_ids");
  if (accepted.length > 0 && !payload.cortex_receipt) {
    throw new Error("Apply response omitted Cortex admission receipt");
  }
}

const COMPONENTS: Array<[string, string]> = [
  ["pull", "Pull"],
  ["cortex", "Cortex"],
  ["blueprint", "Blueprint"],
  ["ledger", "Ledger"],
  ["adapt", "Adapt"],
];

function stateLabel(state: string) {
  return state[0].toUpperCase() + state.slice(1);
}

function formatBytes(bytes: number | undefined) {
  if (!Number.isFinite(bytes)) return "Unavailable";
  if ((bytes ?? 0) < 1024 * 1024) return `${Math.round((bytes ?? 0) / 1024)} KiB`;
  return `${((bytes ?? 0) / (1024 * 1024)).toFixed(1)} MiB`;
}

function componentRows(metrics: ResourceMetrics | null): ContextComponent[] {
  const binding = metrics?.membraneBinding;
  if (!binding?.bound) {
    const reason = binding?.note ?? "canonical context binding unavailable";
    return COMPONENTS.map(([, name]) => ({ name, state: "unavailable", reason }));
  }
  const advertised = new Set(binding.subsystems ?? []);
  return COMPONENTS.map(([id, name]) =>
    advertised.has(id)
      ? { name, state: "advertised", reason: "capability advertised; runtime health not exposed" }
      : { name, state: "unavailable", reason: "subsystem evidence omitted" },
  );
}

export default function ContextRuntimePanel({ config }: { config: DaemonConfig | null }) {
  const [metrics, setMetrics] = useState<ResourceMetrics | null>(null);
  const [hubStatus, setHubStatus] = useState<ContextStatusResponse | null>(null);
  const [adaptStatus, setAdaptStatus] = useState<AdaptStatus | null>(null);
  const [adaptResult, setAdaptResult] = useState<Record<string, unknown> | null>(null);
  const [lastAdaptOperation, setLastAdaptOperation] = useState<string | null>(null);
  const [reviewManifest, setReviewManifest] = useState<PreferenceManifest | null>(null);
  const [admissionReceipt, setAdmissionReceipt] = useState<AdaptAdmissionReceipt | null>(null);
  const [adaptError, setAdaptError] = useState<string | null>(null);
  const [adaptBusy, setAdaptBusy] = useState<string | null>(null);
  const [reviewInput, setReviewInput] = useState("");
  const [reviewInstallation, setReviewInstallation] = useState("");
  const [reviewCreatedAt, setReviewCreatedAt] = useState("");
  const [reviewPoolHash, setReviewPoolHash] = useState("");
  const [adjudicateManifest, setAdjudicateManifest] = useState("");
  const [adjudicateDecisions, setAdjudicateDecisions] = useState("");
  const [adjudicateValidatedAt, setAdjudicateValidatedAt] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);

  const load = useCallback(async () => {
    if (!config) {
      setMetrics(null);
      setError("Daemon connection is unavailable");
      return;
    }
    setLoading(true);
    setError(null);
    try {
      const headers = config.token ? { authorization: `Bearer ${config.token}` } : undefined;
      const [metricsResponse, statusResponse] = await Promise.all([
        fetch(`${config.url.replace(/\/+$/, "")}/runtime/resource-metrics`, { headers }),
        fetch(`${config.url.replace(/\/+$/, "")}/runtime/context/status`, { headers }),
      ]);
      if (!metricsResponse.ok)
        throw new Error(`Resource metrics returned ${metricsResponse.status}`);
      setMetrics((await metricsResponse.json()) as ResourceMetrics);
      setHubStatus(
        statusResponse.ok
          ? ((await statusResponse.json()) as ContextStatusResponse)
          : { state: "unavailable", reason: `Context status returned ${statusResponse.status}` },
      );
      const adaptResponse = await fetch(
        `${config.url.replace(/\/+$/, "")}/runtime/context/adapt/status?scope=global`,
        { headers },
      );
      setAdaptStatus(adaptResponse.ok ? ((await adaptResponse.json()) as AdaptStatus) : null);
    } catch (reason) {
      setMetrics(null);
      setError(reason instanceof Error ? reason.message : "Resource metrics unavailable");
    } finally {
      setLoading(false);
    }
  }, [config]);

  const runAdapt = useCallback(
    async (operation: string, path: string, body: Record<string, unknown>) => {
      if (!config) return;
      setAdaptBusy(operation);
      setAdaptError(null);
      try {
        const response = await fetch(`${config.url.replace(/\/+$/, "")}${path}`, {
          method: "POST",
          headers: {
            "content-type": "application/json",
            ...(config.token ? { authorization: `Bearer ${config.token}` } : {}),
          },
          body: JSON.stringify(body),
        });
        const payload = (await response.json()) as Record<string, unknown>;
        if (!response.ok)
          throw new Error(String(payload.reason ?? payload.error ?? response.status));
        if (payload.ok === false || payload.error) {
          throw new Error(String(payload.reason ?? payload.error ?? "Adapt operation rejected"));
        }
        const manifest = preferenceManifest(payload);
        if ((operation === "review" || operation === "adjudicate") && !manifest) {
          throw new Error("Adapt response omitted PreferenceManifestV1");
        }
        if (operation === "apply") validateApplyResponse(payload);
        if (operation === "review" && manifest) {
          if (manifest.installation_id && manifest.installation_id !== reviewInstallation) {
            throw new Error("Reviewed manifest installation does not match source identity");
          }
          if (
            reviewPoolHash &&
            manifest.canonical_pool_sha256 &&
            manifest.canonical_pool_sha256 !== reviewPoolHash
          ) {
            throw new Error("Reviewed manifest pool hash does not match source identity");
          }
          setReviewManifest(manifest);
          setAdmissionReceipt(null);
        }
        if (operation === "adjudicate" && manifest) {
          if (!reviewManifest || manifest.manifest_sha256 !== reviewManifest.manifest_sha256) {
            throw new Error("Adjudication response is not the reviewed manifest");
          }
          setAdmissionReceipt({
            manifest,
            manifestPath: adjudicateManifest,
            statusFingerprint: adaptStatusFingerprint(adaptStatus),
            sourceIdentity: `${reviewInstallation}\u0000${reviewPoolHash}`,
          });
        }
        setAdaptResult(payload);
        setLastAdaptOperation(operation);
      } catch (reason) {
        setAdaptError(reason instanceof Error ? reason.message : "Adapt operation failed");
      } finally {
        setAdaptBusy(null);
      }
    },
    [adaptStatus, adjudicateManifest, config, reviewInstallation, reviewManifest, reviewPoolHash],
  );

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    const fingerprint = adaptStatusFingerprint(adaptStatus);
    setAdmissionReceipt((receipt) =>
      receipt && receipt.statusFingerprint === fingerprint ? receipt : null,
    );
  }, [adaptStatus]);

  const binding = metrics?.membraneBinding;
  const snapshot = hubStatus?.snapshot;
  const sections = snapshot?.sections ?? {};
  const rows = componentRows(metrics);
  const contextState: ContextState = !binding?.bound
    ? "unavailable"
    : rows.some((row) => row.state === "unavailable")
      ? "degraded"
      : rows.some((row) => row.state === "degraded")
        ? "degraded"
        : rows.some((row) => row.state === "advertised")
          ? "advertised"
          : "available";

  return (
    <div className="flex-1 min-h-0 overflow-y-auto">
      <div className="flex items-center justify-between border-b border-line px-5 py-3">
        <div>
          <h3 className="text-[14px] font-semibold text-text">Context status</h3>
          <p className="mt-1 text-[11px] text-text-3">
            Authenticated native context binding and recorded runtime evidence.
          </p>
        </div>
        <button
          type="button"
          className="cr-control-pill rounded-md border border-line px-2.5 py-1 text-[11px] text-text-2 hover:bg-panel"
          onClick={() => void load()}
          disabled={loading}
        >
          {loading ? "Refreshing" : "Refresh"}
        </button>
      </div>

      {error ? (
        <div
          className="m-5 rounded-md border border-danger/40 bg-danger/10 p-3 text-[12px] text-danger"
          role="alert"
        >
          <strong>Context metrics unavailable.</strong>
          <p className="mt-1">{error}. Check daemon connection, then refresh.</p>
        </div>
      ) : null}

      {!error && !metrics ? (
        <EmptyState icon={<Icon name="activity" size={18} />} title="No context metrics">{"Recorded metrics appear after the daemon responds."}</EmptyState>
      ) : null}

      {metrics ? (
        <>
          <div className="grid grid-cols-3 gap-3 border-b border-line px-5 py-3">
            <RuntimeStat label="Context" value={stateLabel(hubStatus?.state ?? contextState)} />
            <RuntimeStat label="Daemon RSS" value={formatBytes(metrics.daemon?.rssBytes)} />
            <RuntimeStat
              label="Turn p95"
              value={
                metrics.turnLatency?.available
                  ? `${metrics.turnLatency.p95Ms ?? "—"} ms`
                  : "Unavailable"
              }
            />
          </div>
          <section className="border-b border-line px-5 py-4">
            <div className="mb-2 flex items-center justify-between">
              <h4 className="text-[11px] font-semibold uppercase text-text-3">
                Five context components
              </h4>
              <span className="text-[11px] text-text-3">{stateLabel(contextState)}</span>
            </div>
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-2">
              {rows.map((row) => (
                <div className="rounded-md border border-line bg-panel px-3 py-2" key={row.name}>
                  <div className="flex items-center justify-between gap-2">
                    <strong className="text-[12px] text-text">{row.name}</strong>
                    <span className="text-[11px] text-text-3">{stateLabel(row.state)}</span>
                  </div>
                  {row.reason ? <p className="mt-1 text-[10px] text-text-3">{row.reason}</p> : null}
                </div>
              ))}
            </div>
          </section>
          <section className="border-b border-line px-5 py-4">
            <h4 className="mb-2 text-[11px] font-semibold uppercase text-text-3">
              Identity and evidence
            </h4>
            {binding?.bound ? (
              <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-[11px]">
                <dt className="text-text-3">Service</dt>
                <dd className="truncate text-text-2">{binding.serviceId || "Unavailable"}</dd>
                <dt className="text-text-3">Installation</dt>
                <dd className="truncate text-text-2">{binding.installationId || "Unavailable"}</dd>
                <dt className="text-text-3">Cortex store</dt>
                <dd className="truncate text-text-2">{binding.cortexStoreId || "Unavailable"}</dd>
                <dt className="text-text-3">Startup</dt>
                <dd className="text-text-2">{binding.startupGeneration || "Unavailable"}</dd>
              </dl>
            ) : (
              <p className="text-[12px] text-text-3">
                {binding?.note ?? "No authenticated binding identity"}
              </p>
            )}
            <div className="mt-3 grid grid-cols-2 gap-2 text-[11px] text-text-3">
              <span>
                Tool-schema observations:{" "}
                {metrics.membraneEvidence?.h4?.toolSchemaObservations ?? "Unavailable"}
              </span>
              <span>
                Context ceilings:{" "}
                {metrics.membraneEvidence?.h8?.remainingContextCeilings?.length ?? "Unavailable"}
              </span>
              <span>
                Round trips:{" "}
                {metrics.roundTripsPerTask?.available
                  ? (metrics.roundTripsPerTask.total ?? 0)
                  : "Unavailable"}
              </span>
              <span>Runtime origin: {binding?.runtimeOrigin ?? "Unavailable"}</span>
            </div>
          </section>
          <section className="border-b border-line px-5 py-4">
            <h4 className="mb-2 text-[11px] font-semibold uppercase text-text-3">
              Repository freshness and index
            </h4>
            <div className="grid grid-cols-2 gap-2 text-[11px] text-text-3">
              <span>Repository freshness: {sections.repositories?.state ?? "Unavailable"}</span>
              <span>Ledger index: {sections.repositories?.reason ?? "Unavailable"}</span>
            </div>
            <p className="mt-2 text-[11px] text-text-3">
              Observed {sections.repositories?.observedAtUnixMs ?? "Unavailable"}; cache age{" "}
              {sections.repositories?.cacheAgeMs ?? "Unavailable"} ms.
            </p>
          </section>
          <section className="border-b border-line px-5 py-4">
            <h4 className="mb-2 text-[11px] font-semibold uppercase text-text-3">
              Hub sections and admission
            </h4>
            <div className="grid grid-cols-2 gap-2 text-[11px] text-text-3">
              <span>Memory: {sections.memory?.state ?? "Unavailable"}</span>
              <span>Alerts: {sections.alerts?.state ?? "Unavailable"}</span>
              <span>Pull: {snapshot?.subsystems?.pull?.state ?? "Unavailable"}</span>
              <span>Adapt: {snapshot?.subsystems?.adapt?.state ?? "Unavailable"}</span>
              <span>Decisions: {snapshot?.admission?.decisionsTotal ?? "Unavailable"}</span>
              <span>Omissions: {snapshot?.admission?.omissionsTotal ?? "Unavailable"}</span>
            </div>
            <p className="mt-2 text-[11px] text-text-3">
              {snapshot?.admission?.lastPullReason ?? hubStatus?.reason ?? "No admission receipt"}
            </p>
          </section>
          <section className="px-5 py-4">
            <h4 className="text-[12px] font-semibold text-text">Adapt review and admission</h4>
            <div className="mt-2 grid grid-cols-2 gap-2 text-[11px] text-text-3">
              <span>
                Review queue: {adaptStatus?.lanes?.review?.pending_count ?? "Unavailable"}
              </span>
              <span>Insights: {adaptStatus?.lanes?.insights?.workload ?? "Unavailable"}</span>
              <span>
                Admission: {adaptStatus?.lanes?.admission?.reachable ? "Reachable" : "Unavailable"}
              </span>
              <span>Qualified: {adaptStatus?.qualified === true ? "Yes" : "No"}</span>
            </div>
            <div className="mt-3 grid gap-2">
              <input
                className="cr-control-input"
                aria-label="Proposed input path"
                placeholder="Proposed input path"
                value={reviewInput}
                onChange={(event) => {
                  setReviewInput(event.target.value);
                  setReviewManifest(null);
                  setAdmissionReceipt(null);
                }}
              />
              <input
                className="cr-control-input"
                aria-label="Installation ID"
                placeholder="Installation ID"
                value={reviewInstallation}
                onChange={(event) => {
                  setReviewInstallation(event.target.value);
                  setReviewManifest(null);
                  setAdmissionReceipt(null);
                }}
              />
              <input
                className="cr-control-input"
                aria-label="Created at"
                placeholder="Created at (RFC3339)"
                value={reviewCreatedAt}
                onChange={(event) => {
                  setReviewCreatedAt(event.target.value);
                  setReviewManifest(null);
                  setAdmissionReceipt(null);
                }}
              />
              <input
                className="cr-control-input"
                aria-label="Canonical pool SHA256"
                placeholder="Canonical pool SHA256 (optional)"
                value={reviewPoolHash}
                onChange={(event) => {
                  setReviewPoolHash(event.target.value);
                  setReviewManifest(null);
                  setAdmissionReceipt(null);
                }}
              />
              <button
                type="button"
                className="cr-control-pill w-fit rounded-md border border-line px-2.5 py-1 text-[11px] text-text-2 hover:bg-panel disabled:opacity-40"
                disabled={
                  adaptBusy !== null || !reviewInput || !reviewInstallation || !reviewCreatedAt
                }
                onClick={() =>
                  void runAdapt("review", "/runtime/context/adapt/review-taste", {
                    input: reviewInput,
                    installationId: reviewInstallation,
                    canonicalPoolSha256: reviewPoolHash || null,
                    createdAt: reviewCreatedAt,
                  })
                }
              >
                {adaptBusy === "review" ? "Reviewing" : "Review proposed content"}
              </button>
              <input
                className="cr-control-input"
                aria-label="Manifest path"
                placeholder="Manifest path (optional with inline review)"
                value={adjudicateManifest}
                onChange={(event) => {
                  setAdjudicateManifest(event.target.value);
                  setAdmissionReceipt(null);
                }}
              />
              <input
                className="cr-control-input"
                aria-label="Decision file path"
                placeholder="Decision file path"
                value={adjudicateDecisions}
                onChange={(event) => {
                  setAdjudicateDecisions(event.target.value);
                  setAdmissionReceipt(null);
                }}
              />
              <input
                className="cr-control-input"
                aria-label="Validated at"
                placeholder="Validated at (RFC3339)"
                value={adjudicateValidatedAt}
                onChange={(event) => {
                  setAdjudicateValidatedAt(event.target.value);
                  setAdmissionReceipt(null);
                }}
              />
              <button
                type="button"
                className="cr-control-pill w-fit rounded-md border border-line px-2.5 py-1 text-[11px] text-text-2 hover:bg-panel disabled:opacity-40"
                disabled={
                  adaptBusy !== null ||
                  !reviewManifest ||
                  !adjudicateDecisions ||
                  !adjudicateValidatedAt
                }
                onClick={() =>
                  void runAdapt("adjudicate", "/runtime/context/adapt/adjudicate-taste", {
                    manifest: adjudicateManifest || null,
                    manifestValue: reviewManifest,
                    decisions: adjudicateDecisions,
                    validatedAt: adjudicateValidatedAt,
                  })
                }
              >
                {adaptBusy === "adjudicate" ? "Adjudicating" : "Submit review decisions"}
              </button>
              <button
                type="button"
                className="cr-control-pill w-fit rounded-md border border-danger/40 px-2.5 py-1 text-[11px] text-danger hover:bg-danger/10 disabled:opacity-40"
                disabled={
                  adaptBusy !== null ||
                  lastAdaptOperation !== "adjudicate" ||
                  !admissionReceipt ||
                  admissionReceipt.statusFingerprint !== adaptStatusFingerprint(adaptStatus) ||
                  admissionReceipt.sourceIdentity !== `${reviewInstallation}\u0000${reviewPoolHash}`
                }
                onClick={() =>
                  void runAdapt("apply", "/runtime/context/adapt/apply-taste", {
                    manifest: admissionReceipt?.manifestPath || null,
                    manifestValue: admissionReceipt?.manifest,
                    dryRun: false,
                  })
                }
              >
                {adaptBusy === "apply" ? "Applying" : "Admit reviewed Taste"}
              </button>
            </div>
            {adaptError ? <p className="mt-2 text-[11px] text-danger">{adaptError}</p> : null}
            {adaptResult ? (
              <div className="mt-3">
                <p className="mb-1 text-[10px] uppercase text-text-3">
                  {lastAdaptOperation} output — inspect before next decision
                </p>
                <pre className="max-h-52 overflow-auto rounded-md border border-line bg-panel p-2 text-[10px] text-text-3">
                  {JSON.stringify(adaptResult, null, 2)}
                </pre>
              </div>
            ) : null}
          </section>
        </>
      ) : null}
    </div>
  );
}
