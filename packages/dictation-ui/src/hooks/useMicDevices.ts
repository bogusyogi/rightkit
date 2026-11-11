// useMicDevices - enumerate input devices via the `list_mic_devices` command.
// Includes an unmount guard so a late async result never sets state after the
// component is gone.

import { useCallback, useEffect, useRef, useState } from "react";
import { dictationClient, type MicDevice } from "../host.js";

export type { MicDevice };

export interface UseMicDevices {
  devices: MicDevice[];
  loading: boolean;
  error: string;
  refresh: () => void;
}

export function useMicDevices(refreshToken = 0): UseMicDevices {
  const [devices, setDevices] = useState<MicDevice[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [refreshRevision, setRefreshRevision] = useState(0);
  const loadedOnce = useRef(false);
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
        const list = await dictationClient().micDevices();
        if (active) setDevices(list);
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
