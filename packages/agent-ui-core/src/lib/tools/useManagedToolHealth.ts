import { useCallback, useEffect, useRef, useState } from "react";
import {
  managedToolErrorMessage,
  projectManagedToolHealth,
  type ManagedToolHealth,
  type ManagedToolProbe,
} from "./managedToolHealth";

export interface UseManagedToolHealthOptions {
  toolId: string;
  probe: ManagedToolProbe;
  enabled?: boolean;
  onNotInstalled?: (health: ManagedToolHealth) => void;
}

/**
 * Runs one real managed-tool version probe on mount and on demand. Stale
 * results from an unmounted or replaced probe are ignored.
 */
export function useManagedToolHealth({
  toolId,
  probe,
  enabled = true,
  onNotInstalled,
}: UseManagedToolHealthOptions) {
  const [health, setHealth] = useState<ManagedToolHealth | null>(null);
  const [checking, setChecking] = useState(enabled);
  const [error, setError] = useState<string | null>(null);
  const generation = useRef(0);

  const check = useCallback(async () => {
    const request = ++generation.current;
    setChecking(true);
    setError(null);
    try {
      const result = projectManagedToolHealth(toolId, await probe());
      if (request !== generation.current) return null;
      setHealth(result);
      if (!result.installed) onNotInstalled?.(result);
      return result;
    } catch (cause) {
      if (request !== generation.current) return null;
      setError(managedToolErrorMessage(cause));
      setHealth(null);
      return null;
    } finally {
      if (request === generation.current) setChecking(false);
    }
  }, [onNotInstalled, probe, toolId]);

  useEffect(() => {
    if (!enabled) {
      setChecking(false);
      return;
    }
    void check();
    return () => {
      generation.current += 1;
    };
  }, [check, enabled]);

  return { health, checking, error, check };
}

export type UseManagedToolHealthReturn = ReturnType<typeof useManagedToolHealth>;
