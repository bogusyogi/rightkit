// Input-device list for a microphone picker: reads through the host's `list`, refreshes on device change and
// window focus, and ignores a late result after unmount. Imports only `react`.
import { useCallback, useEffect, useRef, useState } from "react";
import type { MicDeviceLike, MicDeviceSelection, MicSelectionLabels } from "./mic-device-options.js";
import { micSelectionLabel } from "./mic-device-options.js";

export interface UseMicDevices<D> {
  devices: D[];
  /** True until the first read settles; later refreshes keep the current list. */
  loading: boolean;
  error: string;
  refresh: () => void;
}

/**
 * @param list Reads the input devices (the host's command).
 * @param refreshToken Changing it re-reads (a host passes e.g. a permission revision).
 */
export function useMicDevices<D>(list: () => Promise<D[]>, refreshToken: number | string = 0): UseMicDevices<D> {
  const [devices, setDevices] = useState<D[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [refreshRevision, setRefreshRevision] = useState(0);
  const loadedOnce = useRef(false);
  const listRef = useRef(list);
  listRef.current = list;
  const refreshKey = `${refreshToken}:${refreshRevision}`;

  const refresh = useCallback(() => {
    setRefreshRevision((current) => current + 1);
  }, []);

  useEffect(() => {
    void refreshKey;
    let active = true;
    setLoading(!loadedOnce.current);
    setError("");
    void (async () => {
      try {
        const next = await listRef.current();
        if (active) setDevices(next);
      } catch (e) {
        if (active) setError(String(e));
      } finally {
        if (active) {
          loadedOnce.current = true;
          setLoading(false);
        }
      }
    })();
    return () => {
      active = false;
    };
  }, [refreshKey]);

  useEffect(() => {
    const mediaDevices = navigator.mediaDevices;
    mediaDevices?.addEventListener?.("devicechange", refresh);
    window.addEventListener("focus", refresh);
    return () => {
      mediaDevices?.removeEventListener?.("devicechange", refresh);
      window.removeEventListener("focus", refresh);
    };
  }, [refresh]);

  return { devices, loading, error, refresh };
}

/**
 * Reports the resolved microphone line (`micSelectionLabel`) to `onResolved` after each render where it, or the callback,
 * changed. Does nothing without a callback.
 */
export function useResolvedMicLabel<D extends MicDeviceLike>(
  selection: MicDeviceSelection<D>,
  labels: MicSelectionLabels,
  onResolved: ((label: string) => void) | undefined,
): void {
  const label = micSelectionLabel(selection, labels);
  useEffect(() => {
    onResolved?.(label);
  }, [label, onResolved]);
}
