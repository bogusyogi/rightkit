/**
 * Headless (behaviour-only) hooks behind the built-in licence and doctor sections. They render nothing: apps keep their
 * own markup and call these for state and actions. The built-in sections are thin views over the same hooks.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { isPro, tier as licenseTier, type StoredLicense } from "@rightkit/license";
import { errText } from "./errors.js";

/* ---- Licence -------------------------------------------------------------------------------- */

/**
 * Minimal licence client. `createLicenseClient(...)` satisfies it; so does an app-private client (IPC commands, a native
 * store). The current licence is read with `status()` (or `load()`, the name `createLicenseClient` uses); neither means "none".
 */
export interface LicenseClientLike {
  activate(key: string): Promise<StoredLicense>;
  deactivate(): Promise<unknown>;
  status?(): Promise<StoredLicense | null>;
  load?(): Promise<StoredLicense | null>;
}

export interface UseLicenseSectionOptions {
  client: LicenseClientLike;
  /** Awaited before `client.deactivate()`. A throw aborts the deactivation and becomes `error`. */
  beforeDeactivate?: () => Promise<void> | void;
  /** Only affects `requestDeactivate`. Default true (two steps); false makes `requestDeactivate` deactivate at once. */
  confirmDeactivate?: boolean;
  /** Days an expired term licence still counts as `pro`. Default 7 (trial licences get none). */
  graceDays?: number;
  /** `error` text when the client returns a non-active licence without a `message`. Default "The key was not accepted." */
  notAcceptedMessage?: string;
  /** Fired with the new licence (null after deactivation) once activate or deactivate succeeds. */
  onChange?: (lic: StoredLicense | null) => void;
}

export interface UseLicenseSectionResult {
  /** Current licence; null before the first read, when the client has none, or after deactivation. */
  license: StoredLicense | null;
  /** `@rightkit/license` `isPro`: the licence is active, still valid (within `graceDays`) and its tier is at least "pro". */
  pro: boolean;
  /** Tier name from the licence (`@rightkit/license` `tier()`), or null. */
  tier: string | null;
  /** An activate or deactivate call is in flight. */
  busy: boolean;
  /** Narrower than `busy`: the activate call specifically is in flight. */
  activating: boolean;
  /** Last failure or rejection message (read, activate, deactivate, `beforeDeactivate`, or a non-active activation result). Cleared when the next action starts. */
  error: string | null;
  /**
   * Activate with a key. A blank key is ignored (resolves null, no call). Resolves the licence the client returned (also when it
   * is not `active`: then `error` is the client's `message`, or "The key was not accepted."), or null on a thrown error / blank key.
   * `onChange` fires with every returned licence, active or not.
   */
  activate(key: string): Promise<StoredLicense | null>;
  /** Deactivate now: `beforeDeactivate`, then `client.deactivate()`, then `license = null` and `onChange(null)`. Ignores `confirmDeactivate`. */
  deactivate(): Promise<void>;
  /**
   * Confirmation-aware deactivation, the two-step API for a Deactivate button. With `confirmDeactivate` true (default) the first
   * call sets `confirmPending` (it disarms itself after 4 seconds, or on `cancelDeactivate()`), and the second call while
   * pending runs `deactivate()`. With `confirmDeactivate: false` the first call runs `deactivate()`.
   */
  requestDeactivate(): void;
  /** True between the first and second `requestDeactivate()` call. Show "Confirm deactivate" while it is true. */
  confirmPending: boolean;
  /** Disarm a pending confirmation. */
  cancelDeactivate(): void;
  /** Re-read the licence from the client (`status()` or `load()`). Resolves it, or null if the client cannot read or the read fails (then `error` is set). */
  refresh(): Promise<StoredLicense | null>;
}

const CONFIRM_MS = 4000;

export function useLicenseSection(options: UseLicenseSectionOptions): UseLicenseSectionResult {
  const { client } = options;
  const opts = useRef(options);
  opts.current = options;
  const [license, setLicense] = useState<StoredLicense | null>(null);
  const [busy, setBusy] = useState(false);
  const [activating, setActivating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmPending, setConfirmPending] = useState(false);
  const mounted = useRef(true);
  const reads = useRef(0);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const apply = useCallback((l: StoredLicense | null) => {
    reads.current += 1; // an action's result supersedes any read still in flight
    setLicense(l);
    opts.current.onChange?.(l);
  }, []);

  const refresh = useCallback(async (): Promise<StoredLicense | null> => {
    const read = client.status ?? client.load;
    if (!read) return null;
    const token = ++reads.current;
    try {
      const l = (await read.call(client)) ?? null;
      if (!mounted.current || reads.current !== token) return l;
      setLicense(l);
      setError(null);
      return l;
    } catch (e) {
      if (mounted.current && reads.current === token) setError(errText(e));
      return null;
    }
  }, [client]);
  useEffect(() => {
    void refresh();
    return () => {
      reads.current += 1;
    };
  }, [refresh]);

  const activate = useCallback(
    async (key: string): Promise<StoredLicense | null> => {
      if (!key.trim()) return null;
      setBusy(true);
      setActivating(true);
      setError(null);
      try {
        const l = await client.activate(key);
        if (mounted.current) {
          apply(l);
          if (l.status !== "active") setError(l.message ?? opts.current.notAcceptedMessage ?? "The key was not accepted.");
        }
        return l;
      } catch (e) {
        if (mounted.current) setError(errText(e));
        return null;
      } finally {
        if (mounted.current) {
          setBusy(false);
          setActivating(false);
        }
      }
    },
    [client, apply],
  );

  const deactivate = useCallback(async (): Promise<void> => {
    setConfirmPending(false);
    setBusy(true);
    setError(null);
    try {
      await opts.current.beforeDeactivate?.();
      await client.deactivate();
      if (mounted.current) apply(null);
    } catch (e) {
      if (mounted.current) setError(errText(e));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }, [client, apply]);

  useEffect(() => {
    if (!confirmPending) return;
    const t = setTimeout(() => setConfirmPending(false), CONFIRM_MS);
    return () => clearTimeout(t);
  }, [confirmPending]);
  const requestDeactivate = useCallback(() => {
    if (opts.current.confirmDeactivate === false || confirmPending) void deactivate();
    else setConfirmPending(true);
  }, [confirmPending, deactivate]);
  const cancelDeactivate = useCallback(() => setConfirmPending(false), []);

  const grace = options.graceDays ?? 7;
  return {
    license,
    pro: isPro(license, grace),
    tier: licenseTier(license) ?? null,
    busy,
    activating,
    error,
    activate,
    deactivate,
    requestDeactivate,
    confirmPending,
    cancelDeactivate,
    refresh,
  };
}

/* ---- Doctor --------------------------------------------------------------------------------- */

export interface DoctorResult {
  ok: boolean;
  /** Short explanation shown next to the check (why it failed, or what was found). */
  detail?: string;
  /** What to do about a failure. Shown only when `ok` is false. */
  fix?: string;
}

export interface DoctorCheck {
  /** Stable id; becomes `data-check-id` and feeds `ids.check`. */
  id: string;
  label: string;
  /** Secondary text under the label before the check has run. */
  description?: string;
  /** A throw or rejection is a failed check whose `detail` is the error message. */
  run(): Promise<DoctorResult> | DoctorResult;
}

export type DoctorStatus = { state: "idle" } | { state: "running" } | { state: "done"; result: DoctorResult };

export interface UseDoctorOptions {
  /** Run every check once on mount. Default false. */
  autoRun?: boolean;
  /** Called after each check finishes (not for a superseded run). */
  onResult?: (check: DoctorCheck, result: DoctorResult) => void;
}

export interface UseDoctorResult {
  /** One entry per current check id; `{ state: "idle" }` until it has run. */
  status: Record<string, DoctorStatus>;
  summary: {
    /** Checks whose latest result is ok. */
    passed: number;
    /** Number of checks supplied. */
    total: number;
    /** `Date.now()` when a check last finished, or null if none has finished yet. */
    lastRun: number | null;
  };
  /** Any check is running. */
  running: boolean;
  /** Run every check in parallel. Resolves when all have finished. */
  runAll(): Promise<void>;
  /** Run one check by id. Resolves its result, or null for an unknown id or a superseded or unmounted run. */
  runOne(id: string): Promise<DoctorResult | null>;
}

/**
 * Per-check state for app-supplied checks. A throw or rejection is a failed check (`detail` = the message). Starting a check
 * again while it runs supersedes the earlier run: only the newest run writes status or calls `onResult`.
 */
export function useDoctor(checks: ReadonlyArray<DoctorCheck>, options: UseDoctorOptions = {}): UseDoctorResult {
  const [state, setState] = useState<Record<string, DoctorStatus>>({});
  const [lastRun, setLastRun] = useState<number | null>(null);
  const tokens = useRef<Record<string, number>>({});
  const mounted = useRef(true);
  const checksRef = useRef(checks);
  checksRef.current = checks;
  const onResult = useRef(options.onResult);
  onResult.current = options.onResult;
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const runCheck = useCallback(async (check: DoctorCheck): Promise<DoctorResult | null> => {
    const token = (tokens.current[check.id] = (tokens.current[check.id] ?? 0) + 1);
    setState((m) => ({ ...m, [check.id]: { state: "running" } }));
    let result: DoctorResult;
    try {
      result = await check.run();
    } catch (e) {
      result = { ok: false, detail: errText(e) };
    }
    if (!mounted.current || tokens.current[check.id] !== token) return null;
    setState((m) => ({ ...m, [check.id]: { state: "done", result } }));
    setLastRun(Date.now());
    onResult.current?.(check, result);
    return result;
  }, []);
  const runOne = useCallback(
    async (id: string) => {
      const check = checksRef.current.find((c) => c.id === id);
      return check ? runCheck(check) : null;
    },
    [runCheck],
  );
  const runAll = useCallback(async () => {
    await Promise.all(checksRef.current.map(runCheck));
  }, [runCheck]);
  useEffect(() => {
    if (options.autoRun) void runAll();
    // Once on mount only.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const status: Record<string, DoctorStatus> = {};
  for (const c of checks) status[c.id] = state[c.id] ?? { state: "idle" };
  const all = Object.values(status);
  return {
    status,
    summary: { passed: all.filter((s) => s.state === "done" && s.result.ok).length, total: checks.length, lastRun },
    running: all.some((s) => s.state === "running"),
    runAll,
    runOne,
  };
}
