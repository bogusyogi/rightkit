import { useCallback, useState } from "react";
import { getRuntimeMiddleware, getRuntimeObservations, getRuntimeSpans } from "@rightkit/agent-ui-core/lib/daemon";
import type {
  DaemonConfig,
  MiddlewareRegistrySnapshot,
  RuntimeObservation,
  RuntimeSpan,
} from "@rightkit/agent-ui-core/lib/types";

const EMPTY_MIDDLEWARE: MiddlewareRegistrySnapshot = {
  middleware: [],
  spans: [],
  observations: [],
};

interface UseMiddlewareObservationsRuntimeParams {
  config: DaemonConfig | null;
  sessionId: string | null;
}

export function useMiddlewareObservationsRuntime({
  config,
  sessionId,
}: UseMiddlewareObservationsRuntimeParams) {
  const [runtimeMiddlewareSnapshot, setRuntimeMiddlewareSnapshot] =
    useState<MiddlewareRegistrySnapshot>(EMPTY_MIDDLEWARE);
  const [runtimeSpans, setRuntimeSpans] = useState<RuntimeSpan[]>([]);
  const [runtimeObservations, setRuntimeObservations] = useState<RuntimeObservation[]>([]);

  const loadMiddlewareObservations = useCallback(async () => {
    if (!config) return;
    const [mw, spans, observations] = await Promise.all([
      getRuntimeMiddleware(config),
      getRuntimeSpans(config, sessionId ?? undefined),
      getRuntimeObservations(config, sessionId ?? undefined),
    ]);
    setRuntimeMiddlewareSnapshot(mw);
    setRuntimeSpans(spans);
    setRuntimeObservations(observations);
  }, [config, sessionId]);

  return {
    runtimeMiddlewareSnapshot,
    runtimeSpans,
    runtimeObservations,
    loadMiddlewareObservations,
  };
}
