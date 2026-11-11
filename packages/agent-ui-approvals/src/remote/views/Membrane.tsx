import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button, Card, Toggle } from "@rightkit/react-shell/react";
import { clearVisibleInterval, visibleInterval } from "@rightkit/system-ui/visible";
import { setNotchSetting } from "../api.js";
import { useNotch } from "./Settings.js";

/** Status and freshness refresh while the section is open. Logs, memory and context load only on demand. */
const REFRESH_MS = 5_000;
const LOG_LINES = 200;
const MEMORY_NOTES = 20;

/** `membrane.status` (engine/src/membrane_cmds.rs). `model` is the resident's health JSON verbatim. */
interface MembraneStatus {
  state: string;
  running: boolean;
  pid: number | null;
  uptimeSecs: number | null;
  model: unknown;
  /** `ok` usable or off by design; `temporary` clears without repair; `fault` needs a repair (#433). */
  availability: { state: "ok" | "temporary" | "fault"; reason: string } | null;
  lastError: string | null;
  health: string;
  healthError: string | null;
}

const availabilityLabel = { ok: "OK", temporary: "Temporary", fault: "Fault" } as const;

/** `membrane.freshness` (engine/src/membrane_cmds.rs). Unknown shapes are shown as text, never assumed. */
interface MembraneProject {
  root: string;
  generationId: unknown;
  complete: unknown;
  eventSequence: unknown;
  lastEvent: unknown;
}

interface MembraneFreshness {
  projects: MembraneProject[];
  watcherState: unknown;
  coverage: unknown;
  freshness: unknown;
  reason: string | null;
}

/** `membrane.logs` (engine/src/membrane_cmds.rs). */
interface MembraneLogs {
  lines: string[];
  count: number;
  truncated: boolean;
  available: boolean;
}

/** `membrane.memory` (engine/src/membrane_cmds.rs). `ts` is whatever unit the store reports; null when it reports none. */
interface MembraneNote {
  id: unknown;
  ts: number | null;
  title: string | null;
  repository: string | null;
}

interface MembraneMemory {
  available: boolean;
  reason: string | null;
  count: number;
  notes: MembraneNote[];
}

/** `membrane.context` (engine/src/membrane_cmds.rs): counts of delivery receipts, never the injected text. */
interface MembraneContext {
  available: boolean;
  reason: string | null;
  receipts: number;
  byState: Record<string, number>;
  byEvent: Record<string, number>;
  sessions: number;
  last: { event: unknown; harness: unknown; state: unknown; ts: number | null } | null;
}

const counts = (map: Record<string, number>): string =>
  Object.entries(map).map(([k, n]) => `${k} ${n}`).join(", ") || "None";

/** Any engine value as one short line of text. */
const text = (value: unknown): string => {
  if (value === null || value === undefined || value === "") return "None";
  if (typeof value === "string") return value;
  if (typeof value === "number" || typeof value === "boolean") return String(value);
  try {
    const json = JSON.stringify(value);
    return json.length > 240 ? `${json.slice(0, 240)}…` : json;
  } catch {
    return "Unreadable";
  }
};

/** The resident's model readiness: `model.state` with its `reason` when the health JSON gives them. */
const modelText = (model: unknown): string => {
  if (model && typeof model === "object") {
    const m = model as { state?: unknown; reason?: unknown };
    if (m.state !== undefined) {
      return m.reason ? `${text(m.state)}: ${text(m.reason)}` : text(m.state);
    }
  }
  return text(model);
};

const uptime = (secs: number | null): string => {
  if (secs === null || secs === undefined) return "None";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  return h > 0 ? `${h}h ${m}m` : `${m}m ${Math.floor(secs % 60)}s`;
};

const stateTone = (state: string): "ok" | "warn" | undefined =>
  state === "running" ? "ok" : state === "starting" || state === "stopped" || state === "not installed" || state === "engine starting" || state === "engine down" ? "warn" : state.startsWith("latched") ? "warn" : undefined;

/** The two history switches (#431), shared with Settings: the owner's own search, and the grant that lets
 *  a paired computer pull redacted snippets. Both are owner-only; `setNotchSetting` routes them through the engine. */
const HISTORY_SWITCHES = [
  {
    key: "ledgerTranscriptSearch",
    label: "Search ChatGPT and Codex history",
    note: "Off by default. On: Membrane indexes this computer's Codex sessions and any ChatGPT export in the chosen folder, for `pulse memory history`. Off: Membrane deletes its copy.",
  },
  {
    key: "ledgerTranscriptShare",
    label: "Share history search with my paired computers",
    note: "Off by default. On: a paired computer of yours may pull redacted snippets (about 4 KB each, no files, no paths) of the chats you allow under Remote chat read, over the authenticated device link only. Off: refused, and the other computer deletes its copy.",
  },
] as const;

function HistorySwitches() {
  const notch = useNotch();
  const [error, setError] = useState<string | null>(null);
  const settings = (notch.state?.settings ?? {}) as Record<string, unknown>;
  const shown = HISTORY_SWITCHES.filter((s) => s.key in settings);
  const set = (key: string, value: boolean) => {
    setError(null);
    setNotchSetting(key, value).catch((e) => setError(String(e)));
  };
  if (!notch.state) {
    return <p className="muted">{notch.error ?? "Reading the history switches…"}</p>;
  }
  if (shown.length === 0) {
    return <p className="muted">This computer's notch does not publish the history switches.</p>;
  }
  return (
    <>
      {shown.map((s) => (
        <div key={s.key} className="ck-membrane-row">
          <Toggle
            checked={settings[s.key] === true}
            onChange={(value: boolean) => set(s.key, value)}
            label={s.label}
          />
          <span className="muted">{s.note}</span>
        </div>
      ))}
      {error && <p className="ck-membrane-error" role="alert">The change was not saved: {error}</p>}
    </>
  );
}

export function Membrane() {
  const [status, setStatus] = useState<MembraneStatus | null>(null);
  const [statusError, setStatusError] = useState<string | null>(null);
  const [freshness, setFreshness] = useState<MembraneFreshness | null>(null);
  const [freshnessError, setFreshnessError] = useState<string | null>(null);
  const [logs, setLogs] = useState<MembraneLogs | null>(null);
  const [logsError, setLogsError] = useState<string | null>(null);
  const [logsLoading, setLogsLoading] = useState(false);
  const [memory, setMemory] = useState<MembraneMemory | null>(null);
  const [memoryError, setMemoryError] = useState<string | null>(null);
  const [memoryLoading, setMemoryLoading] = useState(false);
  const [context, setContext] = useState<MembraneContext | null>(null);
  const [contextError, setContextError] = useState<string | null>(null);
  const [contextLoading, setContextLoading] = useState(false);

  const [confirmRestart, setConfirmRestart] = useState(false);
  const [restarting, setRestarting] = useState(false);
  const [restartError, setRestartError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setStatus(await invoke<MembraneStatus>("membrane_status"));
      setStatusError(null);
    } catch (e) {
      setStatusError(String(e));
    }
    try {
      setFreshness(await invoke<MembraneFreshness>("membrane_freshness"));
      setFreshnessError(null);
    } catch (e) {
      setFreshnessError(String(e));
    }
  }, []);

  const restart = useCallback(async () => {
    setRestarting(true);
    try {
      await invoke("membrane_restart");
      setRestartError(null);
      setConfirmRestart(false);
      await refresh();
    } catch (e) {
      setRestartError(String(e));
    } finally {
      setRestarting(false);
    }
  }, [refresh]);

  const loadLogs = useCallback(async () => {
    setLogsLoading(true);
    try {
      setLogs(await invoke<MembraneLogs>("membrane_logs", { lines: LOG_LINES }));
      setLogsError(null);
    } catch (e) {
      setLogsError(String(e));
    } finally {
      setLogsLoading(false);
    }
  }, []);

  const loadMemory = useCallback(async () => {
    setMemoryLoading(true);
    try {
      setMemory(await invoke<MembraneMemory>("membrane_memory", { limit: MEMORY_NOTES }));
      setMemoryError(null);
    } catch (e) {
      setMemoryError(String(e));
    } finally {
      setMemoryLoading(false);
    }
  }, []);

  const loadContext = useCallback(async () => {
    setContextLoading(true);
    try {
      setContext(await invoke<MembraneContext>("membrane_context"));
      setContextError(null);
    } catch (e) {
      setContextError(String(e));
    } finally {
      setContextLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = visibleInterval(() => void refresh(), REFRESH_MS);
    return () => clearVisibleInterval(id);
  }, [refresh]);

  return (
    <div className="ck-membrane">
      <p className="muted">
        Membrane keeps per-project repository memory and a freshness map for Pulse. Status refreshes every 5 seconds while this page is open.
      </p>

      <Card as="section" className="ck-membrane-card" label="Status">
        <h2>Status</h2>
        {statusError && <p className="ck-membrane-error" role="alert">Status could not be loaded: {statusError}</p>}
        {!status && !statusError && <p className="muted">Loading status…</p>}
        <div className="ck-membrane-head">
          {!confirmRestart && (
            <Button size="sm" variant="secondary" onClick={() => setConfirmRestart(true)}>
              Restart Membrane
            </Button>
          )}
          {confirmRestart && (
            <>
              <span className="muted">Restart Membrane? Its current process stops and starts again.</span>
              <Button size="sm" variant="secondary" onClick={() => void restart()} disabled={restarting}>
                {restarting ? "Restarting…" : "Confirm restart"}
              </Button>
              <Button size="sm" variant="secondary" onClick={() => setConfirmRestart(false)} disabled={restarting}>
                Cancel
              </Button>
            </>
          )}
        </div>
        {restartError && <p className="ck-membrane-error" role="alert">Restart failed: {restartError}</p>}
        {status && (
          <dl className="ck-membrane-kv">
            <dt>State</dt>
            <dd><Badge tone={stateTone(status.state)}>{status.state}</Badge></dd>
            <dt>Process</dt>
            <dd>{status.running ? "Running" : "Not running"}, pid {text(status.pid)}, up {uptime(status.uptimeSecs)}</dd>
            <dt>Model</dt>
            <dd>{modelText(status.model)}</dd>
            <dt>Availability</dt>
            <dd>
              {status.availability ? (
                <>
                  <Badge tone={status.availability.state === "ok" ? "ok" : "warn"}>
                    {availabilityLabel[status.availability.state]}
                  </Badge>{" "}
                  <span className={status.availability.state === "fault" ? "ck-membrane-error" : undefined}>
                    {status.availability.reason}
                  </span>
                </>
              ) : "None"}
            </dd>
            <dt>Health</dt>
            <dd>{status.health}{status.healthError ? `: ${status.healthError}` : ""}</dd>
            <dt>Last error</dt>
            <dd>{text(status.lastError)}</dd>
          </dl>
        )}
      </Card>

      <Card as="section" className="ck-membrane-card" label="Freshness">
        <h2>Freshness</h2>
        {freshnessError && <p className="ck-membrane-error" role="alert">Freshness could not be loaded: {freshnessError}</p>}
        {freshness && (
          <>
            <dl className="ck-membrane-kv">
              <dt>Watcher</dt>
              <dd>{text(freshness.watcherState)}</dd>
              <dt>Coverage</dt>
              <dd>{text(freshness.coverage)}</dd>
              <dt>Freshness</dt>
              <dd>{text(freshness.freshness)}</dd>
            </dl>
            {freshness.reason && <p className="muted">{freshness.reason}</p>}
            {freshness.projects.length === 0 && !freshness.reason && <p className="muted">No projects indexed yet.</p>}
            <ul className="ck-membrane-list">
              {freshness.projects.map((p) => (
                <li key={p.root} className="ck-membrane-row">
                  <span className="ck-membrane-path">{p.root}</span>
                  <span className="muted">generation {text(p.generationId)}, {p.complete === true ? "complete" : "incomplete"}, event {text(p.eventSequence)}</span>
                  <span className="muted">last {text(p.lastEvent)}</span>
                </li>
              ))}
            </ul>
          </>
        )}
      </Card>

      <Card as="section" className="ck-membrane-card" label="Memory">
        <div className="ck-membrane-head">
          <h2>Recent memory</h2>
          <Button size="sm" variant="secondary" onClick={() => void loadMemory()} disabled={memoryLoading}>
            {memoryLoading ? "Loading…" : memory ? "Refresh memory" : "Load memory"}
          </Button>
        </div>
        {memoryError && <p className="ck-membrane-error" role="alert">Memory could not be loaded: {memoryError}</p>}
        {memory && !memory.available && <p className="muted">{memory.reason ?? "Memory is not available."}</p>}
        {memory && memory.available && memory.notes.length === 0 && <p className="muted">No memory notes yet.</p>}
        {memory && memory.available && (
          <ul className="ck-membrane-list">
            {memory.notes.map((n, i) => (
              <li key={`${text(n.id)}-${i}`} className="ck-membrane-row">
                <span>{n.title ?? "Untitled note"}</span>
                <span className="muted">{text(n.repository)}{n.ts !== null ? `, ${text(n.ts)}` : ""}, id {text(n.id)}</span>
              </li>
            ))}
          </ul>
        )}
        {!memory && !memoryError && <p className="muted">Memory is read on demand. Secrets are redacted.</p>}
      </Card>

      <Card as="section" className="ck-membrane-card" label="History">
        <h2>ChatGPT and Codex history</h2>
        <p className="muted">Search your own history, and let a paired computer of yours search what you allow. Both are off until you turn them on.</p>
        <HistorySwitches />
      </Card>

      <Card as="section" className="ck-membrane-card" label="Context">
        <div className="ck-membrane-head">
          <h2>Context delivered</h2>
          <Button size="sm" variant="secondary" onClick={() => void loadContext()} disabled={contextLoading}>
            {contextLoading ? "Loading…" : context ? "Refresh context" : "Load context"}
          </Button>
        </div>
        {contextError && <p className="ck-membrane-error" role="alert">Context could not be loaded: {contextError}</p>}
        {context && !context.available && <p className="muted">{context.reason ?? "Context receipts are not available."}</p>}
        {context && context.available && (
          <dl className="ck-membrane-kv">
            <dt>Receipts</dt>
            <dd>{context.receipts} across {context.sessions} session{context.sessions === 1 ? "" : "s"}</dd>
            <dt>By state</dt>
            <dd>{counts(context.byState)}</dd>
            <dt>By source</dt>
            <dd>{counts(context.byEvent)}</dd>
            <dt>Last</dt>
            <dd>{context.last ? `${text(context.last.event)} (${text(context.last.harness)}), ${text(context.last.state)}, ${text(context.last.ts)}` : "None"}</dd>
          </dl>
        )}
        {!context && !contextError && <p className="muted">Context receipts are read on demand. Only counts and sources are shown.</p>}
      </Card>

      <Card as="section" className="ck-membrane-card" label="Logs">
        <div className="ck-membrane-head">
          <h2>Recent logs</h2>
          <Button size="sm" variant="secondary" onClick={() => void loadLogs()} disabled={logsLoading}>
            {logsLoading ? "Loading…" : logs ? "Refresh logs" : "Load logs"}
          </Button>
        </div>
        {logsError && <p className="ck-membrane-error" role="alert">Logs could not be loaded: {logsError}</p>}
        {logs && !logs.available && <p className="muted">No Membrane log yet.</p>}
        {logs && logs.available && (
          <>
            <p className="muted">
              {logs.count} line{logs.count === 1 ? "" : "s"}{logs.truncated ? `, showing the last ${LOG_LINES}` : ""}. Secrets are redacted.
            </p>
            <pre className="ck-membrane-log" aria-label="Membrane log tail">{logs.lines.join("\n") || "The log is empty."}</pre>
          </>
        )}
        {!logs && !logsError && <p className="muted">Logs are read on demand.</p>}
      </Card>
    </div>
  );
}
