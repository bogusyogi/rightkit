import { useEffect } from "react";
import { semanticIndexStatus } from "../lib/ipc";
import { meaningAvailable } from "../lib/semanticStatus";
import { useAppStore } from "../store";

const POLL_MS = 60_000;

/** Keeps `meaningReady` in the store in step with the semantic index status. */
export function useMeaningReady() {
  const setMeaningReady = useAppStore((s) => s.setMeaningReady);

  useEffect(() => {
    let cancelled = false;
    const check = () => {
      semanticIndexStatus()
        .then((s) => {
          if (!cancelled) setMeaningReady(meaningAvailable(s));
        })
        .catch(() => {
          if (!cancelled) setMeaningReady(false);
        });
    };
    check();
    const id = window.setInterval(check, POLL_MS);
    return () => {
      cancelled = true;
      window.clearInterval(id);
    };
  }, [setMeaningReady]);
}
