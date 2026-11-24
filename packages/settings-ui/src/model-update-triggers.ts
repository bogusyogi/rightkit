// What opens the model-update prompt: an "available" event, a navigation request naming the model, and a
// one-shot flag the backend leaves for a webview created after it was set. None of them starts the download.
// Event names, the payload field names and the one-shot command are inputs. Imports only `react`.
// Moved from HeardRight `components/model-update/ModelUpdatePrompt.tsx`.
import { useEffect, useRef } from "react";

export interface ModelUpdateAvailablePayload {
  backend?: string;
  model_label?: string;
  download_bytes?: number;
  reason?: string;
}

export interface UseModelUpdateTriggersOptions {
  /** The backend this prompt serves; an "available" payload naming another backend is ignored. */
  backend: string;
  availableEvent: string;
  navigateEvent: string;
  /** Field of the navigate payload that names the model to download. */
  navigateField: string;
  /** Subscribes to a host event; resolves to the unsubscribe. */
  listen<T>(event: string, handler: (payload: T) => void): Promise<() => void>;
  /** Reads and clears the one-shot flag (HeardRight: `take_retired_model_download_request`). */
  takeRequest(): Promise<boolean>;
  /** The store's `open`. */
  open(info?: { modelLabel?: string; downloadBytes?: number }): void;
}

export function useModelUpdateTriggers(options: UseModelUpdateTriggersOptions): void {
  const ref = useRef(options);
  ref.current = options;
  const { availableEvent, navigateEvent } = options;

  useEffect(() => {
    const un = ref.current.listen<ModelUpdateAvailablePayload>(availableEvent, (p) => {
      const o = ref.current;
      if (p?.backend && p.backend !== o.backend) return;
      o.open({ modelLabel: p?.model_label, downloadBytes: p?.download_bytes });
    });
    return () => void un.then((f) => f()).catch(() => undefined);
  }, [availableEvent]);

  useEffect(() => {
    const un = ref.current.listen<Record<string, string | undefined>>(navigateEvent, (p) => {
      const o = ref.current;
      if (p?.[o.navigateField] === o.backend) o.open();
    });
    return () => void un.then((f) => f()).catch(() => undefined);
  }, [navigateEvent]);

  // The one-shot flag survives a webview created after the backend set it. It opens the prompt; it never
  // starts the download.
  useEffect(() => {
    void ref.current
      .takeRequest()
      .then((requested) => {
        if (requested) ref.current.open();
      })
      .catch(() => undefined);
  }, []);
}
