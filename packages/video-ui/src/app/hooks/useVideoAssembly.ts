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
  /** Optional identity and validity check supplied by the workspace. */
  isCurrentSelection?: () => boolean;
}

/** Assemble, recover, cancel and render against the configured VideoHost. */
export function useVideoAssembly({ dir, progressController, refresh, setError: setJobError, setOutput, callbacks: events, isCurrentSelection }: VideoAssemblyOptions) {
  const { progress, begin, patch, seed } = progressController;
  const callbacks = useRef<VideoAssemblyCallbacks>({});
  callbacks.current = events ?? {};
  const [pending, setPending] = useState<PendingAssemble | null>(null);
  const [resumable, setResumable] = useState(false);
  const [renderMissing, setRenderMissing] = useState(false);
  const [assembling, setAssembling] = useState(false);
  const [rendering, setRendering] = useState(false);

  // Each operation has its own generation. The workspace callback is stable for
  // one selection and becomes false synchronously when that selection changes.
  const runGeneration = useRef(0);
  const renderGeneration = useRef(0);
  const recoveryGeneration = useRef(0);
  const followedGeneration = useRef(0);
  const current = useCallback(() => isCurrentSelection?.() ?? true, [isCurrentSelection]);
  useEffect(() => {
    if (!isCurrentSelection) return;
    setPending(null); setResumable(false); setRenderMissing(false);
    setAssembling(false); setRendering(false);
  }, [isCurrentSelection]);

  // On open: if this project has an unfinished assemble, ask the engine where it stands.
  useEffect(() => {
    if (!dir) return;
    const saved = loadPending(dir);
    if (!saved) return;
    let cancelled = false;
    const generation = ++recoveryGeneration.current;
    const followed = followedGeneration.current;
    const own = () => !cancelled && current() && recoveryGeneration.current === generation;
    const valid = () => own() && followedGeneration.current === followed;
    engineCall<AssembleStatus>("assemble.status", { dir, assemble_id: saved.assembleId })
      .then((st) => {
        if (!own()) return;
        // A stored render whose file is gone must run again, so the run is not cleared as finished.
        const missing = !!st.render && st.render_exists === false;
        if (st.status === "done" && !missing) {
          if (loadPending(dir)?.assembleId === saved.assembleId) clearPending(dir);
          return;
        }
        setPending(saved);
        setResumable(true);
        setRenderMissing(missing);
        if (valid()) {
          seed(st);
          // Let the host choose how to present the recovered run.
          callbacks.current.onRecovered?.();
        }
      })
      .catch((e) => {
        if (!own()) return;
        // not_found: the engine never recorded this run; the saved request can still be sent.
        if (errorCode(e) === "not_found") {
          setPending(saved);
          setResumable(true);
          if (valid()) callbacks.current.onRecovered?.();
        } else if (valid()) setJobError(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [dir, seed, current]);

  const run = async (record: PendingAssemble) => {
    if (!dir || !current()) return;
    const generation = ++runGeneration.current;
    const followed = ++followedGeneration.current;
    recoveryGeneration.current += 1;
    const own = () => current() && runGeneration.current === generation;
    const valid = () => own() && followedGeneration.current === followed;
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
      if (!own()) return;
      if (loadPending(dir)?.assembleId === record.assembleId) clearPending(dir);
      setPending((saved) => saved?.assembleId === record.assembleId ? null : saved);
      if (!valid()) return;
      if (result?.render?.output) setOutput(result.render.output);
      patch({ running: false, done: true });
      await refresh();
      if (valid()) callbacks.current.onSucceeded?.();
    } catch (e) {
      if (!own()) return;
      setResumable(true);
      if (!valid()) return;
      const code = errorCode(e);
      if (code === "cancelled" || code === "render_cancelled") patch({ running: false, cancelled: true });
      else {
        setJobError(errorMessage(e));
        patch({ running: false });
      }
      await refresh().catch(() => undefined);
    } finally {
      if (own()) setAssembling(false);
    }
  };

  const submit = (form: AssembleForm) => {
    if (!current()) return;
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
    if (!dir || !current()) return;
    const generation = ++renderGeneration.current;
    const followed = ++followedGeneration.current;
    const own = () => current() && renderGeneration.current === generation;
    const valid = () => own() && followedGeneration.current === followed;
    setRendering(true);
    setJobError(null);
    const tag = begin("render");
    const job = `ui-${tag}`;
    patch({ render: { job }, running: true });
    try {
      const result = await engineCall<{ output?: string; status?: string }>("render", { dir, preset, job }, tag);
      if (valid() && result?.output) setOutput(result.output);
    } catch (e) {
      if (valid()) setJobError(errorMessage(e));
    } finally {
      if (own()) setRendering(false);
      if (valid()) patch({ render: null, running: false });
    }
  };

  // `assemble.cancel` also stops the assemble's in-flight render (engine side).
  const cancel = () => {
    if (!current()) return;
    const runAtCancel = runGeneration.current;
    const renderAtCancel = renderGeneration.current;
    const report = (e: unknown) => {
      if (current() && runAtCancel === runGeneration.current && renderAtCancel === renderGeneration.current) setJobError(errorMessage(e));
    };
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
