import { useCallback, useEffect, useRef, useState } from "react";
import { engineCall, errorCode, errorMessage } from "../../lib/engine.js";
import { type AssembleForm, type AssembleInput, type AssembleStatus, type PendingAssemble, buildAssembleInput, clearPending, loadPending, newAssembleId, savePending } from "../lib/assemble.js";
import type { useProgress } from "./useProgress.js";

/** Lifecycle notifications let the host choose navigation, copy and presentation. */
export interface VideoAssemblyCallbacks {
  onRecovered?: () => void;
  onStarted?: () => void;
  onSucceeded?: () => void;
}

export interface VideoAssemblyOptions {
  dir: string | null;
  progressController: ReturnType<typeof useProgress>;
  refresh: () => Promise<void>;
  setError: (message: string | null) => void;
  setOutput: (output: string | null) => void;
  callbacks?: VideoAssemblyCallbacks;
}

/** Assemble, recover, cancel and render against the configured VideoHost. */
export function useVideoAssembly({ dir, progressController, refresh, setError: setJobError, setOutput, callbacks: events }: VideoAssemblyOptions) {
  const { progress, begin, patch, seed } = progressController;
  const callbacks = useRef<VideoAssemblyCallbacks>({});
  callbacks.current = events ?? {};
  const [pending, setPending] = useState<PendingAssemble | null>(null);
  const [resumable, setResumable] = useState(false);
  const [renderMissing, setRenderMissing] = useState(false);
  const [assembling, setAssembling] = useState(false);
  const [rendering, setRendering] = useState(false);

  // On open: if this project has an unfinished assemble, ask the engine where it stands.
  useEffect(() => {
    if (!dir) return;
    const saved = loadPending(dir);
    if (!saved) return;
    let cancelled = false;
    engineCall<AssembleStatus>("assemble.status", { dir, assemble_id: saved.assembleId })
      .then((st) => {
        if (cancelled) return;
        // A stored render whose file is gone must run again, so the run is not cleared as finished.
        const missing = !!st.render && st.render_exists === false;
        if (st.status === "done" && !missing) {
          clearPending(dir);
          return;
        }
        setPending(saved);
        setResumable(true);
        setRenderMissing(missing);
        seed(st);
        // Let the host choose how to present the recovered run.
        callbacks.current.onRecovered?.();
      })
      .catch((e) => {
        if (cancelled) return;
        // not_found: the engine never recorded this run; the saved request can still be sent.
        if (errorCode(e) === "not_found") {
          setPending(saved);
          setResumable(true);
          callbacks.current.onRecovered?.();
        } else setJobError(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [dir, seed]);

  const run = async (record: PendingAssemble) => {
    if (!dir) return;
    setAssembling(true);
    setResumable(false);
    setRenderMissing(false);
    setJobError(null);
    callbacks.current.onStarted?.();
    const tag = begin("assemble", record.assembleId);
    try {
      const result = await engineCall<{ render?: { output?: string } }>(
        "assemble",
        { dir, input: record.input },
        tag,
      );
      if (result?.render?.output) setOutput(result.render.output);
      patch({ running: false, done: true });
      clearPending(dir);
      setPending(null);
      await refresh();
      callbacks.current.onSucceeded?.();
    } catch (e) {
      const code = errorCode(e);
      if (code === "cancelled" || code === "render_cancelled") patch({ running: false, cancelled: true });
      else {
        setJobError(errorMessage(e));
        patch({ running: false });
      }
      setResumable(true);
      await refresh().catch(() => undefined);
    } finally {
      setAssembling(false);
    }
  };

  const submit = (form: AssembleForm) => {
    // The id is fixed here and persisted with the exact input: re-sending both resumes.
    const assembleId = newAssembleId();
    const input: AssembleInput = buildAssembleInput(form, assembleId);
    const record: PendingAssemble = { assembleId, input, startedAt: Date.now() };
    if (dir) savePending(dir, record);
    setPending(record);
    void run(record);
  };

  const resume = () => {
    if (pending) void run(pending);
  };

  const render = async (preset: "youtube" | "reels" | "tiktok" | "square") => {
    if (!dir) return;
    setRendering(true);
    setJobError(null);
    const tag = begin("render");
    const job = `ui-${tag}`;
    patch({ render: { job }, running: true });
    try {
      const result = await engineCall<{ output?: string; status?: string }>("render", { dir, preset, job }, tag);
      if (result?.output) setOutput(result.output);
    } catch (e) {
      setJobError(errorMessage(e));
    } finally {
      setRendering(false);
      patch({ render: null, running: false });
    }
  };

  // `assemble.cancel` also stops the assemble's in-flight render (engine side).
  const cancel = () => {
    const report = (e: unknown) => setJobError(errorMessage(e));
    const aid = progress.assembleId;
    if (aid) {
      void engineCall("assemble.cancel", { assemble_id: aid }).catch(report);
    } else if (progress.render?.job) {
      void engineCall("render.cancel", { job: progress.render.job }).catch(report);
    }
  };

  const clearRecovery = useCallback(() => {
    setPending(null);
    setResumable(false);
    setRenderMissing(false);
  }, []);
  return { pending, resumable, renderMissing, assembling, rendering, submit, resume, render, cancel, clearRecovery };
}
