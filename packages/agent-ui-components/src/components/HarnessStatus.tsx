/**
 * HarnessStatus — surfaces the latest `HarnessAttestation` for the
 * current provider so the user can see exactly what binary is in
 * scope, what its SHA-256 is, how old the attestation is, and which
 * isolation mode the host app is enforcing.
 *
 * The component is provider-agnostic. It reads the row from the
 * helper in `lib/cliProviders` and renders it; the caller decides
 * which provider id is "current" (typically the root conductor of
 * the selected session).
 */
import { useCallback, useEffect, useMemo, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import {
  CLI_PROVIDER_IDS,
  formatAgeLabel,
  getProviderDisplay,
  renderHarnessAttestation,
  shortDigest,
  type CliHarnessAttestationRowWire,
  type CliProviderId,
} from "@rightkit/agent-ui-core/lib/cliProviders";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";
import {
  getCliProviderAttestation,
  listCliProviders,
  mutateCliProvider,
  type DaemonCliProvider,
} from "@rightkit/agent-ui-core/lib/daemon/cli-providers";
import { hostAppName } from "../appName.js";

export interface HarnessStatusProps {
  /** Provider id of the binding whose attestation should be shown.
   *  Defaults to the first registered provider when omitted. */
  providerId?: CliProviderId | null;
  config: DaemonConfig | null;
  /** Optional callback when the user requests a refresh. */
  onRefresh?: () => void;
  /** Optional freshness threshold (seconds). Older attestations are
   *  flagged as stale. */
  staleAfterSeconds?: number;
}

function isolationLabelFor(mode: string): string {
  if (mode === "exclusive_coderight_gateway") return `${hostAppName()} gateway only`;
  if (mode === "native_read_only_compatibility") return "Read-only compatibility";
  return mode;
}

export default function HarnessStatus({
  providerId,
  config,
  onRefresh,
  staleAfterSeconds = 24 * 60 * 60,
}: HarnessStatusProps) {
  const [rows, setRows] = useState<CliHarnessAttestationRowWire[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [provider, setProvider] = useState<DaemonCliProvider | null>(null);

  const target = providerId ?? CLI_PROVIDER_IDS[0];
  const refresh = useCallback(async () => {
    if (!config) return;
    setLoading(true);
    try {
      const snapshot = await listCliProviders(config);
      const current = snapshot.providers.find((item) => item.providerId === target) ?? null;
      setProvider(current);
      const attestation = current?.harness.attestationId
        ? await getCliProviderAttestation(config, target)
        : null;
      setRows(attestation ? [{ attestationId: attestation.id, ...attestation }] : []);
      setError(null);
    } catch (err) {
      setRows([]);
      setError(err instanceof Error ? err.message : "unavailable");
    } finally {
      setLoading(false);
    }
    onRefresh?.();
  }, [config, onRefresh, target]);

  const verify = async () => {
    if (!config) return;
    setLoading(true);
    try {
      const snapshot = await mutateCliProvider(config, target, "verify-harness");
      setProvider(snapshot.providers.find((item) => item.providerId === target) ?? null);
      const current = snapshot.providers.find((item) => item.providerId === target) ?? null;
      const attestation = current?.harness.attestationId
        ? await getCliProviderAttestation(config, target)
        : null;
      setRows(attestation ? [{ attestationId: attestation.id, ...attestation }] : []);
      setError(null);
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : "Harness verification failed");
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const row = useMemo(() => rows.find((r) => r.providerId === target) ?? null, [rows, target]);
  const display = getProviderDisplay(target);
  const rendered = row ? renderHarnessAttestation(row) : null;

  // Age derived from verifiedAt. The `attestation` is content-
  // addressed; the displayed age is purely diagnostic.
  const ageMs = rendered ? Date.now() - Date.parse(rendered.attestation.verifiedAt) : null;
  const ageLabel = ageMs !== null ? formatAgeLabel(ageMs) : null;
  const isStale = ageMs !== null && ageMs > staleAfterSeconds * 1000;

  return (
    <section
      className="flex flex-col gap-3 rounded-xl border border-line bg-panel p-4"
      data-testid={`harness-status-${target}`}
    >
      <header className="flex items-start justify-between gap-3">
        <div className="min-w-0">
          <strong className="block text-[13px] font-semibold text-text">
            {display.displayName} harness
          </strong>
          <p className="mt-0.5 text-[12px] text-text-3">
            Attestation is content-addressed by the binary SHA-256, protocol version, profile hash,
            policy hash, and tool schema hash.
          </p>
        </div>
        <button
          type="button"
          onClick={() => void refresh()}
          aria-label="Refresh harness status"
          title="Refresh"
          data-testid="harness-status-refresh"
          className="cr-control-pill inline-flex items-center gap-1.5 rounded-md border border-line bg-panel-2 px-2 py-1 text-[11px] font-semibold text-text-2 hover:text-text"
        >
          <Icon name="refresh" size={11} />
          {loading ? "Refreshing…" : "Refresh"}
        </button>
        <button
          type="button"
          onClick={() => void verify()}
          disabled={loading || !provider?.installed}
          className="cr-control-pill inline-flex items-center gap-1.5 rounded-md border border-line bg-panel-2 px-2 py-1 text-[11px] font-semibold text-text-2 hover:text-text disabled:opacity-50"
        >
          {loading ? "Working…" : "Verify harness"}
        </button>
      </header>

      {error ? (
        <p className="rounded-md border border-warning/40 bg-warning/10 p-2 text-[12px] text-warning">
          {error}
        </p>
      ) : null}
      <p className="text-[11px] text-text-3">
        {provider
          ? `Daemon eligibility: ${provider.harness.eligibility}`
          : "Provider has not been discovered by the daemon."}
      </p>

      <dl className="grid grid-cols-2 gap-x-4 gap-y-1 text-[12px]">
        <dt className="text-text-3">Binary SHA-256</dt>
        <dd
          className="truncate font-mono text-text-2"
          data-testid="harness-status-digest"
          title={rendered?.attestation.executableSha256 ?? ""}
        >
          {rendered ? shortDigest(rendered.attestation.executableSha256, 16) : "—"}
        </dd>
        <dt className="text-text-3">Binary version</dt>
        <dd className="truncate font-mono text-text-2">
          {rendered?.attestation.binaryVersion ?? "—"}
        </dd>
        <dt className="text-text-3">Protocol version</dt>
        <dd className="truncate font-mono text-text-2">
          {rendered?.attestation.protocolVersion ?? "—"}
        </dd>
        <dt className="text-text-3">Isolation mode</dt>
        <dd>
          <span
            className={`inline-flex items-center gap-1 rounded-full border px-2 py-0.5 text-[10px] font-semibold uppercase tracking-wide ${
              rendered?.attestation.isolationMode === "exclusive_coderight_gateway"
                ? "border-success/40 text-success"
                : "border-warning/40 text-warning"
            }`}
            data-testid="harness-status-isolation"
          >
            {rendered ? isolationLabelFor(rendered.attestation.isolationMode) : "—"}
          </span>
        </dd>
        <dt className="text-text-3">Attestation age</dt>
        <dd
          className={`text-text-2 ${isStale ? "text-warning" : ""}`}
          data-testid="harness-status-age"
        >
          {ageLabel ?? "—"}
          {isStale ? " (stale — re-qualify before launching)" : null}
        </dd>
        <dt className="text-text-3">Probes</dt>
        <dd className="text-text-2">
          {rendered
            ? rendered.qualified
              ? `All ${rendered.attestation.passedProbes.length} passed`
              : `${rendered.attestation.failedProbes.length} failed`
            : "—"}
        </dd>
      </dl>

      {rendered && !rendered.qualified ? (
        <p className="rounded-md border border-danger/40 bg-danger/10 p-2 text-[12px] text-danger">
          Failing probes:{" "}
          {rendered.attestation.failedProbes.join(", ") || "see Diagnostics for the full list"}. The
          provider is not launchable until re-qualification succeeds.
        </p>
      ) : null}
    </section>
  );
}
