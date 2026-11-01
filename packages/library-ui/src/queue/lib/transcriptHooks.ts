import { useEffect, useReducer, useRef } from "react";
import type { TranscriptDetail } from "../contract";

export interface TranscriptState {
  detail: TranscriptDetail | null;
  loading: boolean;
  error: string | null;
}

export type TranscriptAction =
  | { type: "reset" }
  | { type: "start" }
  | { type: "loaded"; detail: TranscriptDetail }
  | { type: "failed"; message: string };

export const INITIAL_TRANSCRIPT_STATE: TranscriptState = {
  detail: null,
  loading: false,
  error: null,
};

export function transcriptReducer(state: TranscriptState, action: TranscriptAction): TranscriptState {
  switch (action.type) {
    case "reset":
      return { detail: null, loading: false, error: null };
    case "start":
      return { ...state, error: null, loading: true };
    case "loaded":
      return { detail: action.detail, loading: false, error: null };
    case "failed":
      return { detail: null, loading: false, error: action.message };
  }
}

/**
 * Loads a transcript when `id` is set and `ready` holds; clears it otherwise.
 * The loader is injected (see `loadTranscriptDetail`) so the hook has no Tauri import.
 */
export function useTranscript(
  id: string | null | undefined,
  ready: boolean,
  loader: (id: string) => Promise<TranscriptDetail>,
): TranscriptState {
  const [state, dispatch] = useReducer(transcriptReducer, INITIAL_TRANSCRIPT_STATE);
  // Inline loaders change identity on every render, so the latest one is read through a ref
  // instead of re-running the effect (and refetching) on each render.
  const loaderRef = useRef(loader);
  loaderRef.current = loader;
  useEffect(() => {
    if (!id || !ready) {
      dispatch({ type: "reset" });
      return;
    }
    let cancelled = false;
    dispatch({ type: "start" });
    loaderRef.current(id)
      .then((detail) => {
        if (!cancelled) dispatch({ type: "loaded", detail });
      })
      .catch((e) => {
        if (!cancelled) dispatch({ type: "failed", message: String(e) });
      });
    return () => {
      cancelled = true;
    };
    // The loader is a stable injected function; only the job identity and readiness drive reloads.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [id, ready]);
  return state;
}
