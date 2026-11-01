import { useEffect, useState } from "react";
import type { DisplayJob, EngineLifecycleEvent, JobEvent } from "../contract";
import { formatInvokeError } from "../lib/errorMessage";
import { invoke, listen } from "../lib/ipc";
import { lifecycleFromState } from "../lib/ipc/streamEvents";
import type { EngineStreamHandlers } from "../lib/ipc/streamEvents";
import { subscribeEngine } from "../lib/ipc/subscribe";
import { notifyReady } from "../lib/notify";
import { applyJobEvent, bufferEarlyEvent } from "../lib/jobState";

type SetJobs = React.Dispatch<React.SetStateAction<DisplayJob[]>>;
type SetErr = React.Dispatch<React.SetStateAction<string | null>>;
type JobsRef = React.MutableRefObject<DisplayJob[]>;
type EarlyEventsRef = React.MutableRefObject<Map<string, JobEvent>>;
type TouchedRef = React.MutableRefObject<Set<string>>;

export function useEngineEvents(args: {
  jobsRef: JobsRef;
  setJobs: SetJobs;
  setErr: SetErr;
  retryAfterSignin: (j: DisplayJob, options?: { skipExport?: boolean }) => Promise<void>;
  /** Events for jobs not yet inserted (add response in flight), keyed by id. */
  earlyEvents: EarlyEventsRef;
  /** Ids mutated by live events/inserts — the snapshot-merge barrier. */
  touched: TouchedRef;
  /** Starts a snapshot flight: resets the merge barrier and returns its sequence. */
  beginSnapshot: () => number;
  /** Merges the subscribe snapshot, dropping it when a newer flight superseded it. */
  applySnapshot: (seq: number, persisted: DisplayJob[]) => void;
}): EngineLifecycleEvent | null {
  const {
    jobsRef,
    setJobs,
    setErr,
    retryAfterSignin,
    earlyEvents,
    touched,
    beginSnapshot,
    applySnapshot,
  } = args;

  const [lifecycle, setLifecycle] = useState<EngineLifecycleEvent | null>(null);

  // One subscription replaces the job://update, job://kind and engine://state
  // listeners plus the boot-time get_jobs (#178). The shell attaches before it
  // reads the snapshot, so no event is lost between the two.
  useEffect(() => {
    let closed = false;
    let close: (() => void) | null = null;
    const seq = beginSnapshot();
    const handlers: EngineStreamHandlers = {
      onJobUpdate: (ev) => {
        touched.current.add(ev.id);
        setJobs((js) => {
          const idx = js.findIndex((j) => j.id === ev.id);
          if (idx === -1) {
            // Event beat the add response — buffer it; insertAccepted folds it
            // into the row when the accepted job lands.
            bufferEarlyEvent(earlyEvents.current, ev);
            return js;
          }
          const next = js.slice();
          next[idx] = applyJobEvent(js[idx], ev);
          jobsRef.current = next;
          return next;
        });
        if (ev.stage === "done") {
          const job = jobsRef.current.find((j) => j.id === ev.id);
          void notifyReady(`${job?.title ?? job?.input ?? "Job"} is ready`);
        }
      },
      // Relabel a card mid-run (e.g. /p/ -> carousel).
      onJobKind: (ev) => {
        touched.current.add(ev.id);
        setJobs((js) => {
          const idx = js.findIndex((j) => j.id === ev.id);
          if (idx === -1) return js;
          const next = js.slice();
          next[idx] = { ...js[idx], kind: ev.kind };
          return next;
        });
      },
      onEngineState: setLifecycle,
      // The shell's receiver lagged and dropped events. The snapshot is newer than
      // every queued event, so it replaces the rows instead of merging into them.
      onResync: (snapshot) => {
        // Supersedes in-flight get_jobs flights so a stale response cannot merge back in.
        beginSnapshot();
        earlyEvents.current.clear();
        const jobs = snapshot.jobs as DisplayJob[];
        jobsRef.current = jobs;
        setJobs(jobs);
        setLifecycle(lifecycleFromState(snapshot.engine_state));
      },
    };
    subscribeEngine(handlers)
      .then((subscription) => {
        if (closed) {
          subscription.close();
          return;
        }
        close = subscription.close;
        const { snapshot } = subscription;
        if (!snapshot) return;
        applySnapshot(seq, snapshot.jobs as DisplayJob[]);
        // A state event that already arrived is newer than the snapshot value.
        setLifecycle((current) => current ?? lifecycleFromState(snapshot.engine_state));
      })
      .catch((e) => {
        if (closed) return;
        setErr(formatInvokeError(e));
        // Without the stream the queue still needs its first snapshot.
        invoke<DisplayJob[]>("get_jobs")
          .then((jobs) => applySnapshot(seq, jobs))
          .catch(() => undefined);
      });
    return () => {
      closed = true;
      close?.();
    };
  }, [jobsRef, setJobs, setErr, earlyEvents, touched, beginSnapshot, applySnapshot]);

  // Auto-continue after sign-in: re-run every job waiting on the platform.
  useEffect(() => {
    const un = listen<string>("auth-signed-in", (e) => {
      const platform = e.payload;
      for (const j of jobsRef.current) {
        if (j.needs_auth === platform) void retryAfterSignin(j, { skipExport: true });
      }
    });
    return () => {
      un.then((f) => f());
    };
  }, [jobsRef, retryAfterSignin]);

  // Headed sign-in browser launch failure surfaces as an event.
  useEffect(() => {
    const un = listen<string>("auth-signin-error", (e) => setErr(e.payload));
    return () => {
      un.then((f) => f());
    };
  }, [setErr]);

  return lifecycle;
}
