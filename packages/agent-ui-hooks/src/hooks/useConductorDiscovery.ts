import { useEffect, useMemo, useState } from "react";

import { listCliProviders, type DaemonCliProvider } from "@rightkit/agent-ui-core/lib/daemon/cli-providers";
import { buildConductorOptions, type ConductorOption } from "@rightkit/agent-ui-core/lib/conductorOptions";
import type { CliProviderId } from "@rightkit/agent-ui-core/lib/types/cliProviders";
import type { DaemonConfig } from "@rightkit/agent-ui-core/lib/types";

/**
 * Live CLI provider availability for composer's root-conductor picker.
 *
 * One daemon snapshot is authoritative for all six built-in providers. It
 * includes install/auth/harness state plus each provider's current catalog;
 * the hook deliberately supplies no local model fallback.
 */
export function useConductorDiscovery(
  config: DaemonConfig | null,
  selectedModelIds?: Partial<Record<CliProviderId, string>>,
): {
  options: ConductorOption[];
  loading: boolean;
} {
  const [providers, setProviders] = useState<DaemonCliProvider[]>([]);
  const [loading, setLoading] = useState(Boolean(config));

  useEffect(() => {
    let cancelled = false;
    if (!config) {
      setProviders([]);
      setLoading(false);
      return;
    }
    setLoading(true);
    void listCliProviders(config)
      .then((snapshot) => {
        if (!cancelled) setProviders(snapshot.providers);
      })
      .catch(() => {
        // A failed snapshot is represented by typed, disabled provider rows.
        if (!cancelled) setProviders([]);
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [config]);

  const options = useMemo(
    () => buildConductorOptions({ providers, loading, selectedModelIds }),
    [providers, loading, selectedModelIds],
  );

  return { options, loading };
}
