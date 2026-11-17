import { useEffect, useLayoutEffect, useRef, useState, useSyncExternalStore } from "react";
import { compactDatabase, compactionStatus, type CompactionResult, type CompactionStatus } from "../lib/ipc/system.js";

// One database per webview: reserve through confirmation, execution and settlement.
type Phase = "idle" | "confirming" | "working";
let state: { phase: Phase; revision: number } = { phase: "idle", revision: 0 };
const listeners = new Set<() => void>();
function subscribe(listener: () => void) {
  listeners.add(listener);
  return () => { listeners.delete(listener); };
}
function setPhase(next: Phase) {
  state = { phase: next, revision: state.revision + (next === "idle" ? 1 : 0) };
  for (const listener of listeners) listener();
}
const snapshot = () => state;
const errorText = (error: unknown) => error instanceof Error ? error.message : String(error);

/** Storage status and explicitly confirmed compaction; host owns presentation. */
export function useStorageSettingsModel({ active, confirm, onError }: {
  active: boolean;
  confirm: () => Promise<boolean>;
  onError: (message: string) => void;
}) {
  const current = useSyncExternalStore(subscribe, snapshot, snapshot);
  const [statusRecord, setStatus] = useState<{ revision: number; owner: object; value: CompactionStatus } | null>(null);
  const status = statusRecord?.revision === current.revision ? statusRecord.value : null;
  const [result, setResult] = useState<CompactionResult | null>(null);
  const owner = useRef<object | null>(null);
  const mounted = useRef<object | null>(null);
  useLayoutEffect(() => {
    const token = {};
    mounted.current = token;
    return () => { if (mounted.current === token) mounted.current = null; };
  }, []);
  const onErrorRef = useRef(onError);
  useLayoutEffect(() => { onErrorRef.current = onError; }, [onError]);
  useLayoutEffect(() => {
    if (!active) { owner.current = null; return; }
    const token = {};
    owner.current = token;
    setStatus(null);
    return () => { if (owner.current === token) owner.current = null; };
  }, [active]);
  useEffect(() => {
    const token = owner.current;
    if (!active || !token || current.phase !== "idle") return;
    let cancelled = false;
    const readRevision = current.revision;
    compactionStatus().then((value) => {
      if (!cancelled && owner.current === token && state.phase === "idle" && state.revision === readRevision) setStatus({ revision: readRevision, owner: token, value });
    }).catch((error) => {
      if (!cancelled && owner.current === token && state.phase === "idle" && state.revision === readRevision) onErrorRef.current(errorText(error));
    });
    return () => { cancelled = true; };
  }, [active, current]);

  const compact = async () => {
    const token = owner.current;
    const mount = mounted.current;
    if (!active || !token || !mount || state.phase !== "idle" || statusRecord?.revision !== state.revision || statusRecord.owner !== token || !statusRecord.value.eligible) return;
    setPhase("confirming");
    try {
      if (!(await confirm()) || owner.current !== token) return;
      setPhase("working");
      setResult(null);
      try {
        const value = await compactDatabase();
        if (mounted.current === mount) setResult(value);
      } catch (error) {
        if (owner.current === token) onErrorRef.current(errorText(error));
      }
    } catch (error) {
      if (owner.current === token) onErrorRef.current(errorText(error));
    } finally {
      setPhase("idle");
    }
  };
  return { status, busy: current.phase === "working", pending: current.phase !== "idle", result, compact };
}
