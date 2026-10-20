/**
 * Headless (behaviour-only) hooks behind the built-in licence and doctor sections. They render nothing: apps keep their
 * own markup and call these for state and actions. The built-in sections are thin views over the same hooks.
 */
import { useCallback, useEffect, useRef, useState } from "react";
import { LICENSE_MESSAGES, LicenseClientError, isPro, licenseMessage, tier as licenseTier, type LicenseView, type Lifecycle, type RefreshView } from "@rightkit/license";
import { errText } from "./errors.js";

/* ---- Licence -------------------------------------------------------------------------------- */

/**
 * Minimal licence client. `createLicenseClient(...)` from `@rightkit/license` 0.3 satisfies it, and so does an app-private
 * client with the same shape. Every method resolves a `LicenseView`; the current view is read with `status()`.
 */
export interface LicenseClientLike {
  activate(key: string): Promise<LicenseView>;
  deactivate(): Promise<LicenseView>;
  status?(): Promise<LicenseView>;
  /** Optional. When present and the view needs verification, the section offers "Verify purchase". */
  refresh?(): Promise<RefreshView>;
}

/** Lifecycles where a purchase check can help: the licence needs a server verification or refresh. */
const VERIFY_LIFECYCLES: readonly Lifecycle[] = ["verification_required", "migration_required", "lifetime_provisional", "clock_repair_required"];

export interface UseLicenseSectionOptions {
  client: LicenseClientLike;
  /** Awaited before `client.deactivate()`. A throw aborts the deactivation and becomes `error`. */
  beforeDeactivate?: () => Promise<void> | void;
  /** Only affects `requestDeactivate`. Default true (two steps); false makes `requestDeactivate` deactivate at once. */
  confirmDeactivate?: boolean;
  /** `error` text when the client returns a non-active view without a message code. Default: the package's `keyNotAccepted` text. */
  notAcceptedMessage?: string;
  /** Fired with the view once activate or deactivate succeeds (after a deactivation, the view the client returned). */
  onChange?: (view: LicenseView | null) => void;
}

export interface UseLicenseSectionResult {
  /** Current view; null before the first read, when the client has no reader, or when the read failed. */
  license: LicenseView | null;
  /**
   * The first read has settled (or the client cannot read, or an action already set the view). Until then a null
   * `license` means "not read yet", not "Free": render a placeholder instead of the free state.
   */
  loaded: boolean;
  /** `@rightkit/license` `isPro`: licensed (active or grace, lifecycle grants access, not busy or unresolved) at the pro tier or above. */
  pro: boolean;
  /** Tier from the view (`@rightkit/license` `tier()`), or null. */
  tier: string | null;
  /** An activate or deactivate call is in flight. */
  busy: boolean;
  /** Narrower than `busy`: the activate call specifically is in flight. */
  activating: boolean;
  /**
   * Fixed message text of the last failure (a typed `LicenseClientError` or a non-active view's message code; any other error
   * shows the package's generic text). Never server text. Cleared when the next action starts.
   */
  error: string | null;
  /**
   * Activate with a key. A blank key is ignored (resolves null, no call). Resolves the view the client returned (also when it
   * is not `active`: then `error` is the message for its code, or `notAcceptedMessage`), or null on a thrown error or blank key.
   * `onChange` fires with every returned view, active or not.
   */
  activate(key: string): Promise<LicenseView | null>;
  /** Deactivate now: `beforeDeactivate`, then `client.deactivate()`, then `license` and `onChange` take the returned view. Ignores `confirmDeactivate`. */
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
  /** Re-read the view with `client.status()`. Resolves it, or null if the client cannot read or the read fails (then `error` is set). */
  refresh(): Promise<LicenseView | null>;
  /** True when the client has `refresh()` and the view's lifecycle is one `verify()` can help with. Hide the "Verify purchase" action otherwise. */
  canVerify: boolean;
  /** The `client.refresh()` call of `verify()` is in flight. Disable the action while it is true. */
  verifying: boolean;
  /** Fixed message text of the last failed `verify()` (same rules as `error`). Cleared when the next verify starts. */
  verifyError: string | null;
  /** Run `client.refresh()` and apply the `.view` it returns (`onChange` fires with it, as for activate). Resolves that view, or null on a failure or without `refresh`. */
  verify(): Promise<LicenseView | null>;
}

const CONFIRM_MS = 4000;

const FIXED_LICENSE_TEXT: ReadonlySet<string> = new Set(Object.values(LICENSE_MESSAGES));

/** Fixed text for a caught error. Only a real `LicenseClientError` whose text is one of the package's fixed messages is shown;
 * `name` and `message` are mutable, so neither alone proves the text is safe. Anything else gets the generic text. */
function licenseErrorText(e: unknown): string {
  return e instanceof LicenseClientError && FIXED_LICENSE_TEXT.has(e.message) ? e.message : LICENSE_MESSAGES.unknown;
}

export function useLicenseSection(options: UseLicenseSectionOptions): UseLicenseSectionResult {
  const { client } = options;
  const opts = useRef(options);
  opts.current = options;
  const [license, setLicense] = useState<LicenseView | null>(null);
  const [loaded, setLoaded] = useState(!client.status);
  const [busy, setBusy] = useState(false);
  const [activating, setActivating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [confirmPending, setConfirmPending] = useState(false);
  const [verifying, setVerifying] = useState(false);
  const [verifyError, setVerifyError] = useState<string | null>(null);
  const mounted = useRef(true);
  const reads = useRef(0);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const apply = useCallback((view: LicenseView | null) => {
    reads.current += 1; // an action's result supersedes any read still in flight
    setLicense(view);
    setLoaded(true);
    opts.current.onChange?.(view);
  }, []);

  const refresh = useCallback(async (): Promise<LicenseView | null> => {
    const read = client.status;
    if (!read) {
      setLoaded(true);
      return null;
    }
    const token = ++reads.current;
    try {
      const view = await read.call(client);
      if (!mounted.current || reads.current !== token) return view;
      setLicense(view);
      setError(null);
      setLoaded(true);
      return view;
    } catch (e) {
      if (mounted.current && reads.current === token) {
        setError(licenseErrorText(e));
        setLoaded(true);
      }
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
    async (key: string): Promise<LicenseView | null> => {
      if (!key.trim()) return null;
      setBusy(true);
      setActivating(true);
      setError(null);
      try {
        const view = await client.activate(key);
        if (mounted.current) {
          apply(view);
          if (view.status !== "active") {
            const keyText = opts.current.notAcceptedMessage ?? licenseMessage("keyNotAccepted");
            setError(view.message === "none" || view.message === "keyNotAccepted" ? keyText : licenseMessage(view.message));
          }
        }
        return view;
      } catch (e) {
        if (mounted.current) setError(licenseErrorText(e));
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
      const view = await client.deactivate();
      if (mounted.current) apply(view);
    } catch (e) {
      if (mounted.current) setError(licenseErrorText(e));
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

  const verify = useCallback(async (): Promise<LicenseView | null> => {
    const refreshNow = client.refresh;
    if (!refreshNow) return null;
    setVerifying(true);
    setVerifyError(null);
    try {
      const result = await refreshNow.call(client);
      if (mounted.current) apply(result.view);
      return result.view;
    } catch (e) {
      if (mounted.current) setVerifyError(licenseErrorText(e));
      return null;
    } finally {
      if (mounted.current) setVerifying(false);
    }
  }, [client, apply]);
  const canVerify = typeof client.refresh === "function" && license !== null && VERIFY_LIFECYCLES.includes(license.lifecycle);

  return {
    license,
    loaded,
    pro: isPro(license),
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
    canVerify,
    verifying,
    verifyError,
    verify,
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

export interface DoctorCheck<P = undefined> {
  /** Stable id; becomes `data-check-id` and feeds `ids.check`. */
  id: string;
  label: string;
  /** Secondary text under the label before the check has run. */
  description?: string;
  /**
   * A throw or rejection is a failed check whose `detail` is the error message. With `UseDoctorOptions.probe`, receives
   * that probe's report (one shared call per run).
   */
  run(report: P): Promise<DoctorResult> | DoctorResult;
}

export type DoctorStatus = { state: "idle" } | { state: "running" } | { state: "done"; result: DoctorResult };

export interface UseDoctorOptions<P = undefined> {
  /** Run every check once on mount. Default false. */
  autoRun?: boolean;
  /** Called after each check finishes (not for a superseded run). */
  onResult?: (check: DoctorCheck<P>, result: DoctorResult) => void;
  /**
   * One expensive probe shared by every check (for example a single `doctor` IPC report). Concurrent checks share one
   * in-flight call; each check's `run(report)` receives its result. A rejection fails every check that awaited it.
   */
  probe?: () => Promise<P>;
}

export interface UseDoctorResult<P = undefined> {
  /** The latest settled `probe` result (null before the first one or without a probe), e.g. to show its notes. */
  report: P | null;
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
export function useDoctor<P = undefined>(
  checks: ReadonlyArray<DoctorCheck<P>>,
  options: UseDoctorOptions<P> = {},
): UseDoctorResult<P> {
  const [state, setState] = useState<Record<string, DoctorStatus>>({});
  const [lastRun, setLastRun] = useState<number | null>(null);
  const tokens = useRef<Record<string, number>>({});
  const mounted = useRef(true);
  const checksRef = useRef(checks);
  checksRef.current = checks;
  const onResult = useRef(options.onResult);
  onResult.current = options.onResult;
  const probe = useRef(options.probe);
  probe.current = options.probe;
  const inflight = useRef<Promise<P> | null>(null);
  const [report, setReport] = useState<P | null>(null);
  // One probe call serves every check that starts while it is in flight.
  const getReport = useCallback((): Promise<P> => {
    const run = probe.current;
    if (!run) return Promise.resolve(undefined as P);
    if (!inflight.current) {
      const p = run();
      inflight.current = p;
      p.then(
        (r) => {
          if (mounted.current) setReport(r);
        },
        () => {},
      ).finally(() => {
        if (inflight.current === p) inflight.current = null;
      });
    }
    return inflight.current;
  }, []);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const runCheck = useCallback(async (check: DoctorCheck<P>): Promise<DoctorResult | null> => {
    const token = (tokens.current[check.id] = (tokens.current[check.id] ?? 0) + 1);
    setState((m) => ({ ...m, [check.id]: { state: "running" } }));
    let result: DoctorResult;
    try {
      // No probe: call run() synchronously, exactly as before.
      result = await check.run(probe.current ? await getReport() : (undefined as P));
    } catch (e) {
      result = { ok: false, detail: errText(e) };
    }
    if (!mounted.current || tokens.current[check.id] !== token) return null;
    setState((m) => ({ ...m, [check.id]: { state: "done", result } }));
    setLastRun(Date.now());
    onResult.current?.(check, result);
    return result;
  }, [getReport]);
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
    report,
    status,
    summary: { passed: all.filter((s) => s.state === "done" && s.result.ok).length, total: checks.length, lastRun },
    running: all.some((s) => s.state === "running"),
    runAll,
    runOne,
  };
}
