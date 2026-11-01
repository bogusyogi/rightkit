import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { Badge, Button, ConfirmDialog, Input, SegmentedControl, useShortcuts } from "@rightkit/react-shell/react";
import { ArrowDown, ArrowUp, ChevronDown, ChevronRight } from "lucide-react";
import { api, bytes, isWindows, type ProcessInspection, type ProcessRow, type Status } from "@rightkit/system-ui/api";
import { useNotch, type NotchState } from "./Settings.js";
import { AreaChart, levelColor, type Series } from "@rightkit/system-ui/components/Chart";
import { memPercent, useMetrics, type Sample } from "@rightkit/system-ui/metrics";
import { clearVisibleInterval, visibleInterval } from "@rightkit/system-ui/visible";
import { useActivityLease } from "@rightkit/system-ui/activityLease";

type Level = "ok" | "warn" | "bad";
type SortKey = "memory" | "cpu" | "gpu" | "name" | "threads" | "started";
type SortDir = "asc" | "desc";

/** What the Memory column counts: macOS reports the physical footprint, Windows the working set. */
const MEMORY_TITLE = isWindows
  ? "Memory this process is using in RAM (working set)"
  : "Memory this process is responsible for, including compressed and swapped memory";

const SORTS: { key: SortKey; label: string; defaultDir: SortDir }[] = [
  { key: "memory", label: "Memory", defaultDir: "desc" },
  { key: "cpu", label: "CPU", defaultDir: "desc" },
  // Offered only when a row reports a GPU share (Windows, with Per-process GPU on); see `gpuShown`.
  { key: "gpu", label: "GPU", defaultDir: "desc" },
  { key: "name", label: "Name", defaultDir: "asc" },
  { key: "threads", label: "Threads", defaultDir: "desc" },
  { key: "started", label: "Started", defaultDir: "desc" },
];

/** Words for the caption, e.g. "sorted by cpu, highest first". */
const SORT_WORDS: Record<SortKey, string> = {
  memory: "memory",
  cpu: "cpu",
  gpu: "gpu",
  name: "name",
  threads: "threads",
  started: "start time",
};

// Protected processes (macOS, and Windows' kernel-level ones that cannot be ended). They never get Quit or Force Quit from here.
const PROTECTED_NAMES = new Set([
  "kernel_task", "windowserver", "launchd", "loginwindow",
  "secure system", "system", "system idle process", "system interrupts", "registry", "memory compression",
]);
// the host app itself. Quitting it from its own window would close the hub.
const PULSE_NAMES = new Set(["pulse", "pulse-hub", "pulse hub"]);

/** The Inspect panel: one process, read on request. `result` is null until the first read ends. */
interface InspectState {
  row: ProcessRow;
  result: ProcessInspection | null;
  error: string | null;
  loading: boolean;
}

/** A long path with its middle cut, so both ends stay readable. The full path goes in the title. */
function middleTruncate(text: string, max = 72): string {
  if (text.length <= max) return text;
  const keep = max - 1;
  const head = Math.ceil(keep / 2);
  const tail = Math.floor(keep / 2);
  return `${text.slice(0, head)}…${text.slice(text.length - tail)}`;
}

/**
 * One poll of one app: CPU percent, memory bytes, and disk bytes per second (null when unknown).
 * `total` is the lifetime read plus written bytes the rate is taken from; `members` names the processes
 * it covers, so a change in the member set drops the rate; `at` is when the poll was read (ms).
 */
interface ProcSample {
  cpu: number;
  memory: number;
  disk: number | null;
  total: number | null;
  members: string;
  at: number;
}

/** Samples kept per process: 120 polls, about six minutes at the 3 s poll. */
const HISTORY_SAMPLES = 120;
/** Processes tracked at once; the oldest entries are dropped first. */
const HISTORY_PROCESSES = 2000;

/** Identity of one process: pid plus start time, so a reused pid starts with no history. */
function historyKey(p: ProcessRow): string {
  return `${p.lead.pid}:${p.lead.start_time}`;
}

/** The processes an app row covers, by pid and start time. */
function memberSignature(p: ProcessRow): string {
  return p.members.map((m) => `${m.identity.pid}:${m.identity.start_time}`).join(",");
}

/**
 * Adds one poll to each app's history, drops apps that are gone, and caps the map.
 * The disk rate is the change in the app's lifetime total since its previous poll, over the seconds
 * between this hub's own polls. It is dropped (null) when there is no previous reading, the total went
 * backwards (a pid reused or a member gone), the member set changed, the gap is over 10 s, or disk_io is absent.
 */
function recordHistory(history: Map<string, ProcSample[]>, rows: ProcessRow[], now: number): void {
  const live = new Set<string>();
  for (const p of rows) {
    const id = historyKey(p);
    live.add(id);
    const samples = history.get(id) ?? [];
    const io = p.disk_io;
    const total = io ? io.total_read_bytes + io.total_written_bytes : null;
    const members = memberSignature(p);
    const prev = samples[samples.length - 1];
    const seconds = prev ? (now - prev.at) / 1000 : 0;
    // Narrowed inline: the rate needs the previous total and this one, and both must agree on the members.
    const disk =
      prev && prev.total != null && total != null && prev.members === members && total >= prev.total && seconds > 0 && seconds <= 10
        ? (total - prev.total) / seconds
        : null;
    samples.push({ cpu: p.cpu_usage_percent, memory: p.memory_bytes, disk, total, members, at: now });
    if (samples.length > HISTORY_SAMPLES) samples.splice(0, samples.length - HISTORY_SAMPLES);
    history.set(id, samples);
  }
  for (const id of [...history.keys()]) if (!live.has(id)) history.delete(id);
  for (const id of [...history.keys()]) {
    if (history.size <= HISTORY_PROCESSES) break;
    history.delete(id);
  }
}

/** The notch's System readings beyond CPU and memory (`system` in its state file). */
export interface SystemReadings {
  network?: { interface: string; kind: string; down: number; up: number };
  battery?: {
    percent: number;
    charging: boolean;
    cycles?: number;
    health?: number;
    /** Battery temperature in °C; absent when the Mac does not publish a plausible value. */
    celsius?: number;
    /** Signed watts: positive charging, negative discharging. */
    watts?: number;
    /** Minutes to empty, or to full when `toFull` is true. */
    minutes?: number;
    toFull?: boolean;
  };
  fans?: { name: string; rpm?: number; percent?: number }[];
  temperatures?: { name: string; celsius: number }[];
}

/** The System readings from the notch's state, or null while the notch has none. */
export function systemReadings(state: NotchState | null): SystemReadings | null {
  return (state as (NotchState & { system?: SystemReadings }) | null)?.system ?? null;
}

function pressureLevel(pressure: string): Level {
  const s = pressure.toLowerCase();
  if (/critical|urgent/.test(s)) return "bad";
  if (/warn|elevated/.test(s)) return "warn";
  return "ok";
}

/** System and critical rows: refused by the backend, not quittable, or protected by name. */
function isSystem(p: ProcessRow): boolean {
  const n = p.name.toLowerCase();
  return !p.can_act || p.refusal != null || PROTECTED_NAMES.has(n) || PULSE_NAMES.has(n);
}

function matchesQuery(p: ProcessRow, q: string): boolean {
  if (p.name.toLowerCase().includes(q)) return true;
  if ((p.bundle_id ?? "").toLowerCase().includes(q)) return true;
  return p.members.some((m) => m.name.toLowerCase().includes(q));
}

function sortValue(p: ProcessRow, sort: SortKey): number | string | null {
  switch (sort) {
    case "cpu":
      return p.cpu_usage_percent;
    case "gpu":
      return p.gpu_percent ?? null;
    case "name":
      return p.name;
    case "threads":
      return p.threads;
    case "started":
      return p.started_at;
    default:
      return p.memory_bytes;
  }
}

/** Stable comparator. Unknown values sort last in both directions. */
function comparator(sort: SortKey, dir: SortDir): (a: ProcessRow, b: ProcessRow) => number {
  return (a, b) => {
    const x = sortValue(a, sort);
    const y = sortValue(b, sort);
    if (x == null || y == null) return x == null && y == null ? 0 : x == null ? 1 : -1;
    const order =
      typeof x === "string" && typeof y === "string"
        ? x.localeCompare(y, undefined, { sensitivity: "base" })
        : Number(x) - Number(y);
    return dir === "asc" ? order : -order;
  };
}

/** "3 h", "12 min", "2 d": the age of a start time; "unknown" when unreadable. */
function ageText(startedAt: number | null, nowSeconds: number): string {
  if (startedAt == null) return "unknown";
  const seconds = Math.max(0, nowSeconds - startedAt);
  if (seconds < 60) return "just now";
  if (seconds < 3600) return `${Math.floor(seconds / 60)} min`;
  if (seconds < 86400) return `${Math.floor(seconds / 3600)} h`;
  return `${Math.floor(seconds / 86400)} d`;
}

function startedTitle(startedAt: number | null): string {
  return startedAt == null ? "Start time unknown" : new Date(startedAt * 1000).toLocaleString();
}

/** One sortable column header. The arrow and aria-sort show the current order. */
function SortHead({
  label,
  column,
  sort,
  dir,
  end,
  title,
  onPick,
}: {
  label: string;
  column: SortKey;
  sort: SortKey;
  dir: SortDir;
  end?: boolean;
  title?: string;
  onPick: (key: SortKey) => void;
}) {
  const active = sort === column;
  return (
    <span
      role="columnheader"
      aria-sort={active ? (dir === "asc" ? "ascending" : "descending") : "none"}
      className={end ? "mon-num" : undefined}
      title={title}
    >
      <Button variant="ghost" className="mon-sort" aria-current={active ? "true" : undefined} onClick={() => onPick(column)}>
        {label}
        {active && (dir === "asc" ? <ArrowUp size={11} aria-hidden="true" /> : <ArrowDown size={11} aria-hidden="true" />)}
      </Button>
    </span>
  );
}

export function Monitor() {
  // Live-activity lease (#448): the notch's session monitors poll every 1 s while this page is visible.
  useActivityLease();
  const [status, setStatus] = useState<Status | null>(null);
  const [procs, setProcs] = useState<ProcessRow[]>([]);
  const [open, setOpen] = useState<string | null>(null);
  const [notes, setNotes] = useState<Record<string, string>>({});
  const [stuck, setStuck] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState<string | null>(null);
  const [forcing, setForcing] = useState<ProcessRow | null>(null);
  const [query, setQuery] = useState("");
  const [sort, setSort] = useState<SortKey>("memory");
  const [dir, setDir] = useState<SortDir>("desc");
  const [sensorsOpen, setSensorsOpen] = useState(false);
  const [inspect, setInspect] = useState<InspectState | null>(null);
  // Per-process samples for the sparklines. A ref, not state: each poll updates it without a second render.
  const procHistory = useRef(new Map<string, ProcSample[]>());
  const notch = useNotch();
  const system = systemReadings(notch.state);

  const note = (key: string, text: string) => setNotes((n) => ({ ...n, [key]: text }));

  // A header click on the sorted column flips the direction; any other choice takes that column's default.
  const chooseSort = (key: SortKey, fromHeader: boolean) => {
    if (fromHeader && key === sort) {
      setDir(dir === "asc" ? "desc" : "asc");
      return;
    }
    setSort(key);
    setDir(SORTS.find((s) => s.key === key)?.defaultDir ?? "desc");
  };

  const reveal = async (r: ProcessRow) => {
    if (!r.executable) return;
    try {
      await api.reveal(r.executable);
    } catch (e) {
      note(r.key, String(e));
    }
  };

  const quit = async (r: ProcessRow) => {
    setBusy(r.key);
    note(r.key, "Asking to quit…");
    try {
      const outcome = await api.quit(r);
      if (outcome === "quit") note(r.key, "Quit.");
      else {
        note(r.key, "Still running. Force Quit is available.");
        setStuck((s) => new Set(s).add(r.key));
      }
    } catch (e) {
      note(r.key, String(e));
    } finally {
      setBusy(null);
    }
  };

  const force = async (r: ProcessRow) => {
    setForcing(null);
    setBusy(r.key);
    try {
      note(r.key, (await api.forceQuit(r)) === "quit" ? "Force quit." : "Still running.");
    } catch (e) {
      note(r.key, String(e));
    } finally {
      setBusy(null);
    }
  };

  // One read of one process, for the row that is open. Results for a row that was closed meanwhile are dropped.
  const readInspect = async (r: ProcessRow) => {
    try {
      const result = await api.inspectProcess(r);
      setInspect((cur) => (cur?.row.key === r.key ? { ...cur, result, error: null, loading: false } : cur));
    } catch (e) {
      setInspect((cur) => (cur?.row.key === r.key ? { ...cur, error: String(e), loading: false } : cur));
    }
  };

  const openInspect = (r: ProcessRow) => {
    setInspect({ row: r, result: null, error: null, loading: true });
    void readInspect(r);
  };

  const refreshInspect = () => {
    if (!inspect) return;
    setInspect({ ...inspect, error: null, loading: true });
    void readInspect(inspect.row);
  };

  // The Windows notch reads the GPU temperature only while someone looks at the readings.
  useEffect(() => {
    if (!isWindows) return;
    const say = () => void invoke("notch_command", { command: { command: "watchSensors" } }).catch(() => {});
    say();
    const handle = visibleInterval(say, 8000);
    return () => clearVisibleInterval(handle);
  }, []);

  useEffect(() => {
    let alive = true;
    // A tick that finds the previous read still running is skipped, never queued behind it.
    let reading = false;
    const tick = async () => {
      if (reading) return;
      reading = true;
      try {
        const [s, p] = await Promise.all([api.status(), api.processRows()]).catch(() => [null, null]);
        if (!alive) return;
        if (s) setStatus(s as Status);
        if (p) {
          setProcs(p as ProcessRow[]);
          recordHistory(procHistory.current, p as ProcessRow[], Date.now());
        }
      } finally {
        reading = false;
      }
    };
    tick();
    const handle = visibleInterval(tick, 3000);
    return () => {
      alive = false;
      clearVisibleInterval(handle);
    };
  }, []);

  // Escape closes the Inspect panel, unless a field or the Force Quit dialog already handled it.
  // The shared shortcut registry skips a key another handler already claimed. Escape also works in a
  // field and during composition, with extra modifiers held, and leaves the browser default alone.
  useShortcuts([
    {
      id: "monitor.closeInspect",
      chord: "esc",
      label: "Close the Inspect panel",
      hidden: true,
      allowInInput: true,
      allowComposing: true,
      ignoreExtraModifiers: true,
      preventDefault: false,
      when: () => inspect !== null && !forcing,
      run: () => setInspect(null),
    },
  ]);

  // Top 3 by memory are flagged regardless of the current sort.
  const topMemory = useMemo(
    () => new Set([...procs].sort((a, b) => b.memory_bytes - a.memory_bytes).slice(0, 3).map((p) => p.key)),
    [procs],
  );

  // The GPU column and sort exist only where a row carries a GPU share (Windows, Per-process GPU on).
  const gpuShown = useMemo(() => procs.some((p) => p.gpu_percent != null), [procs]);
  const trimmed = query.trim();
  const visible = useMemo(() => {
    const q = trimmed.toLowerCase();
    const rows = q ? procs.filter((p) => matchesQuery(p, q)) : [...procs];
    return rows.sort(comparator(sort, dir));
  }, [procs, trimmed, sort, dir]);

  if (!status) return <div className="view muted">Reading…</div>;

  const cpuRaw = status.cpu_usage_percent.value;
  const swapUsed = status.swap_used_bytes.value;
  const swapTotal = status.swap_total_bytes.value;
  const pressure = status.memory_pressure.value ? capitalize(status.memory_pressure.value) : null;
  // Only an elevated or critical reading earns a box; normal and unknown stay one calm line.
  const calm = !pressure || pressureLevel(pressure) === "ok";
  const leader = [...procs].sort(comparator(sort === "cpu" ? "cpu" : "memory", "desc"))[0];
  const orderText =
    sort === "name"
      ? dir === "asc" ? "A to Z" : "Z to A"
      : sort === "started"
        ? dir === "desc" ? "newest first" : "oldest first"
        : dir === "desc" ? "highest first" : "lowest first";
  const nowSeconds = Date.now() / 1000;

  const network = system?.network;
  // Only fans with an rpm or percent reading are rendered, so only those are counted.
  const fanCount = (system?.fans ?? []).filter((f) => f.rpm != null || f.percent != null).length;
  const sensorCount = fanCount + (system?.temperatures?.length ?? 0) + (system?.battery ? 1 : 0);

  return (
    <div className="view monitor-view">
      <div className={`monitor-notice ${calm ? "calm" : pressureLevel(pressure ?? "")}`} role="status">
        <h2>
          Memory pressure:{" "}
          {pressure ? <span className={pressureLevel(pressure)}>{pressure}</span> : "not reported"}
        </h2>
        <p>
          {leader
            ? `${leader.name} · ${sort === "cpu" ? `${Math.round(leader.cpu_usage_percent)}% CPU` : `${bytes(leader.memory_bytes)} memory`}`
            : "No process rows reported."}
        </p>
      </div>

      <MonitorCharts
        cpu={cpuRaw}
        swapText={swapTotal == null ? "Swap unknown" : swapTotal === 0 ? "No swap space" : `Swap ${bytes(swapUsed)} of ${bytes(swapTotal)}`}
        network={network ? { down: network.down, up: network.up, kind: network.kind } : null}
        notchDown={notch.error != null}
      />

      {(sensorCount > 0 || isWindows) && (
        <div className="mon-sensors">
          <Button variant="ghost"
            className="mon-disclosure"
            aria-expanded={sensorsOpen}
            aria-controls="mon-sensors-list"
            onClick={() => setSensorsOpen((open) => !open)}
          >
            {sensorsOpen ? <ChevronDown size={12} aria-hidden="true" /> : <ChevronRight size={12} aria-hidden="true" />}
            <span className="mon-label">Sensors</span>
            <span className="muted small">
              {sensorCount === 0
                ? "not available on this PC"
                : [
                    fanCount ? `${fanCount} fan${fanCount === 1 ? "" : "s"}` : null,
                    system?.temperatures?.length ? `${system.temperatures.length} temperature${system.temperatures.length === 1 ? "" : "s"}` : null,
                    system?.battery ? "battery" : null,
                  ]
                    .filter(Boolean)
                    .join(" · ")}
            </span>
          </Button>
          {sensorsOpen && (
            <div id="mon-sensors-list" className="mon-sensors-list">
              {sensorCount === 0 && <SensorRow label="Sensors" value="Not available on this PC" />}
              {system?.battery && (
                <SensorRow label="Battery" value={batteryText(system.battery)} />
              )}
              {isWindows && sensorCount > 0 && !(system?.temperatures ?? []).some((t) => /cpu|core|package/i.test(t.name)) && (
                <SensorRow label="CPU temperature" value="Not available on this PC" />
              )}
              {system?.fans?.map((f, i) =>
                f.rpm != null ? (
                  <SensorRow key={`fan-${i}`} label={f.name} value={`${Math.round(f.rpm).toLocaleString()} rpm`} />
                ) : f.percent != null ? (
                  <SensorRow key={`fan-${i}`} label={f.name} value={`${Math.round(f.percent)}%`} />
                ) : null
              )}
              {[...(system?.temperatures ?? [])]
                .sort((a, b) => a.name.localeCompare(b.name))
                .map((t, i) => (
                  <SensorRow key={`temp-${i}`} label={t.name} value={`${t.celsius.toFixed(1)} °C`} />
                ))}
            </div>
          )}
        </div>
      )}

      <div className="mon-toolbar">
        <Input
          type="search"
          className="mon-search"
          value={query}
          placeholder="Search apps and processes"
          aria-label="Search apps and processes"
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Escape" && query) {
              e.preventDefault();
              setQuery("");
            }
          }}
        />
        <SegmentedControl
          label="Sort by"
          value={sort}
          options={SORTS.filter((s) => s.key !== "gpu" || gpuShown).map((s) => ({ value: s.key, label: s.label }))}
          onChange={(key) => chooseSort(key, false)}
        />
      </div>

      <div className="mon-section">
        <h2 className="monitor-h2">Top consumers</h2>
        <span>
          {trimmed
            ? `${visible.length} matching “${trimmed}”`
            : `Apps and processes, sorted by ${SORT_WORDS[sort]}, ${orderText}`}
        </span>
        <span className="mon-legend">
          <span className="mon-dot" aria-hidden="true" />
          Above 90% CPU, or top 3 by memory
        </span>
      </div>

      <div className="mon-list">
        <div className="mon-cols mon-head">
          <SortHead label="Name" column="name" sort={sort} dir={dir} onPick={(k) => chooseSort(k, true)} />
          <SortHead label="CPU %" column="cpu" sort={sort} dir={dir} end onPick={(k) => chooseSort(k, true)} />
          <SortHead
            label="Memory"
            column="memory"
            sort={sort}
            dir={dir}
            end
            title={MEMORY_TITLE}
            onPick={(k) => chooseSort(k, true)}
          />
          <SortHead label="Threads" column="threads" sort={sort} dir={dir} end onPick={(k) => chooseSort(k, true)} />
          <SortHead label="Started" column="started" sort={sort} dir={dir} end onPick={(k) => chooseSort(k, true)} />
          <span className="mon-num">Actions</span>
        </div>

        {visible.length === 0 && (
          <div className="mon-empty muted">
            {trimmed ? `No apps or processes match “${trimmed}”.` : "No apps or processes to show yet."}
          </div>
        )}

        {visible.map((p) => {
          const system = isSystem(p);
          const expanded = open === p.key;
          const hasMembers = p.members.length > 1;
          const reasons: string[] = [];
          if (p.cpu_usage_percent > 90) reasons.push("Above 90% CPU");
          if (topMemory.has(p.key)) reasons.push("Top 3 by memory");
          const reasonText = reasons.join(", ");
          const isStuck = stuck.has(p.key);
          const title = p.refusal ?? p.app_path ?? p.name;

          return (
            <div key={p.key} className="mon-group">
              <div className="mon-cols mon-row">
                <div className="mon-name">
                  {reasons.length > 0 && (
                    <span className="mon-dot" role="img" aria-label={reasonText} title={reasonText} />
                  )}
                  {hasMembers ? (
                    <Button variant="ghost"
                      className="mon-disclosure"
                      aria-expanded={expanded}
                      aria-controls={`mon-members-${p.key}`}
                      title={title}
                      onClick={() => setOpen(expanded ? null : p.key)}
                    >
                      {expanded ? <ChevronDown size={12} aria-hidden="true" /> : <ChevronRight size={12} aria-hidden="true" />}
                      <span className="mon-label">{p.name}</span>
                    </Button>
                  ) : (
                    <span className="mon-label" title={title}>
                      {p.name}
                    </span>
                  )}
                  {system && <Badge>System</Badge>}
                  <span className="muted small monitor-meta">
                    {p.members.length} process{p.members.length === 1 ? "" : "es"} · {system ? "Protected system process" : "Running"}
                  </span>
                </div>
                <span className="mon-num">
                  {Math.round(p.cpu_usage_percent)}%
                  {p.gpu_percent != null && (
                    <span className="muted small" title="Share of the busiest GPU engine this app used since the last reading">
                      {` · GPU ${Math.round(p.gpu_percent)}%`}
                    </span>
                  )}
                </span>
                <span className="mon-num">{bytes(p.memory_bytes)}</span>
                <span className={p.threads == null ? "mon-num muted" : "mon-num"}>{p.threads ?? "unknown"}</span>
                <span className={p.started_at == null ? "mon-num muted" : "mon-num"} title={startedTitle(p.started_at)}>
                  {ageText(p.started_at, nowSeconds)}
                </span>
                <span className="mon-actions">
                  <span
                    title={
                      p.executable == null
                        ? "The executable path is unknown"
                        : isWindows
                          ? "Show the executable in File Explorer"
                          : "Show the executable in Finder"
                    }
                  >
                    <Button size="sm" variant="ghost" disabled={p.executable == null} onClick={() => reveal(p)}>
                      Reveal
                    </Button>
                  </span>
                  <Button
                    size="sm"
                    variant="ghost"
                    aria-label={`Inspect ${p.name}`}
                    aria-expanded={inspect?.row.key === p.key}
                    onClick={() => (inspect?.row.key === p.key ? setInspect(null) : openInspect(p))}
                  >
                    Inspect
                  </Button>
                  {!system && p.can_act && (
                    <>
                      <Button size="sm" variant="secondary" disabled={busy === p.key} onClick={() => quit(p)}>
                        Quit
                      </Button>
                      <span className={isStuck ? undefined : "mon-danger"}>
                        <Button
                          size="sm"
                          variant={isStuck ? "danger" : "ghost"}
                          disabled={busy === p.key}
                          onClick={() => setForcing(p)}
                        >
                          Force Quit
                        </Button>
                      </span>
                    </>
                  )}
                </span>
              </div>
              {notes[p.key] && <div className="mon-note muted small">{notes[p.key]}</div>}
              {inspect?.row.key === p.key && (
                <InspectPanel
                  state={inspect}
                  samples={procHistory.current.get(historyKey(inspect.row)) ?? []}
                  onRefresh={refreshInspect}
                  onClose={() => setInspect(null)}
                />
              )}
              {expanded && hasMembers && (
                <div id={`mon-members-${p.key}`}>
                  {p.members.map((m) => (
                    <div key={`${m.identity.pid}-${m.identity.start_time}`} className="mon-cols mon-member">
                      <span className="mon-label muted">{m.name}</span>
                      <span className="mon-num muted small">{Math.round(m.cpu_usage_percent)}%</span>
                      <span className="mon-num">{bytes(m.memory_bytes)}</span>
                      <span className="mon-num muted small">{m.threads ?? "unknown"}</span>
                      <span className="mon-num muted small" title={startedTitle(m.started_at)}>
                        {ageText(m.started_at, nowSeconds)}
                      </span>
                      <span />
                    </div>
                  ))}
                </div>
              )}
            </div>
          );
        })}
      </div>

      <p className="caption muted small">CPU: 100% = one core. System CPU above is 0–100% across all cores.</p>

      {forcing && (
        <ConfirmDialog
          danger
          title={`Force Quit ${forcing.name}?`}
          description="This ends it immediately without letting it save. Unsaved work in it will be lost."
          confirmLabel="Force Quit"
          onConfirm={() => force(forcing)}
          onCancel={() => setForcing(null)}
        />
      )}
    </div>
  );
}

/** The Inspect panel under a row: listening ports and open files, read when asked and never refreshed alone. */
function InspectPanel({
  state,
  samples,
  onRefresh,
  onClose,
}: {
  state: InspectState;
  samples: ProcSample[];
  onRefresh: () => void;
  onClose: () => void;
}) {
  const { row, result, error, loading } = state;
  const ports = result?.listening ?? null;
  const files = result?.open_files ?? null;
  const connections = result?.connections ?? null;
  const unknownText = result == null ? (loading ? "Reading…" : "Unknown") : "Unknown. The note below says why.";

  return (
    <section className="mon-inspect" aria-label={`Inspect ${row.name}`}>
      <div className="mon-inspect-head">
        <div>
          <h3 className="mon-inspect-title">{row.name}</h3>
          <span className="muted small">Read when you asked. Not refreshed on its own.</span>
          {row.resident_bytes != null && (
            <span className="muted small"> · In RAM now: {bytes(row.resident_bytes)}</span>
          )}
        </div>
        <span className="mon-inspect-actions">
          <Button size="sm" variant="secondary" disabled={loading} onClick={onRefresh}>
            Refresh
          </Button>
          <Button size="sm" variant="ghost" onClick={onClose}>
            Close
          </Button>
        </span>
      </div>
      {error && (
        <div className="mon-note small" role="alert">
          {error}
        </div>
      )}
      <div className="mon-inspect-grid">
        <div>
          <h4 className="mon-inspect-h">Listening ports</h4>
          {ports == null ? (
            <p className="muted small">{unknownText}</p>
          ) : ports.length === 0 ? (
            <p className="muted small">No listening ports.</p>
          ) : (
            <ul className="mon-inspect-list" aria-label="Listening ports">
              {ports.map((p) => (
                <li key={`${p.protocol}-${p.family}-${p.address}-${p.port}`} className="mon-inspect-port">
                  <span className="mon-num">{p.port}</span>
                  <span>{p.protocol.toUpperCase()}</span>
                  <span className="muted" title={p.address}>
                    {p.family}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </div>
        <div>
          <h4 className="mon-inspect-h">Open files</h4>
          {files == null ? (
            <p className="muted small">{unknownText}</p>
          ) : files.length === 0 ? (
            <p className="muted small">No open files listed.</p>
          ) : (
            <ul className="mon-inspect-list" aria-label="Open files">
              {files.map((path) => (
                <li key={path} className="mon-inspect-path" title={path}>
                  {middleTruncate(path)}
                </li>
              ))}
            </ul>
          )}
        </div>
      </div>
      <div>
        <h4 className="mon-inspect-h">Network hosts</h4>
        {connections == null ? (
          <p className="muted small">{unknownText}</p>
        ) : connections.length === 0 ? (
          <p className="muted small">No established connections.</p>
        ) : (
          <ul className="mon-inspect-list" aria-label="Network hosts">
            {connections.map((c) => (
              <li key={`${c.protocol}-${c.remote_address}-${c.remote_port}`} className="mon-inspect-port">
                <span className="mon-num" title="Address as the system reports it; not looked up">
                  {c.remote_address}
                </span>
                <span>{c.remote_port}</span>
                <span className="muted">{c.protocol.toUpperCase()}</span>
              </li>
            ))}
          </ul>
        )}
      </div>
      {isWindows ? (
        <p className="muted small">
          {row.gpu_percent == null
            ? "GPU: Unavailable. Windows has no reading yet, or Per-process GPU is off in Settings."
            : `GPU: ${Math.round(row.gpu_percent)}% of its busiest engine`}
        </p>
      ) : (
        <p className="muted small">
          GPU: Unavailable. macOS has no public API that reports GPU use for one process, so Pulse shows no per-process GPU figure.
        </p>
      )}
      <ProcessHistory samples={samples} />
      {result && result.notes.length > 0 && (
        <ul className="mon-inspect-notes muted small">
          {result.notes.map((text) => (
            <li key={text}>{text}</li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** CPU, memory and disk I/O sparklines for one process, from this page's polls. Nothing is kept after the page closes. */
function ProcessHistory({ samples }: { samples: ProcSample[] }) {
  const diskSamples = samples.flatMap((s) => (s.disk == null ? [] : [s.disk]));
  return (
    <div className="mon-history">
      <div className="mon-inspect-head">
        <h4 className="mon-inspect-h">History</h4>
        <span className="muted small">History since this page was opened</span>
      </div>
      {samples.length < 2 ? (
        <p className="muted small">Collecting…</p>
      ) : (
        <div className="mon-spark-grid">
          <HistoryLine title="CPU" values={samples.map((s) => s.cpu)} format={(v) => `${Math.round(v)}%`} />
          <HistoryLine title="Memory" values={samples.map((s) => s.memory)} format={bytes} />
          {/* No line until two readings have a rate; an older engine sends no disk_io, so none is drawn. */}
          {diskSamples.length >= 2 && (
            <HistoryLine
              title={isWindows ? "I/O" : "Disk I/O"}
              values={diskSamples}
              format={(v) => `${bytes(v)}/s`}
            />
          )}
        </div>
      )}
    </div>
  );
}

/** One metric: its current, minimum and maximum as text, and a sparkline of the series. */
function HistoryLine({ title, values, format }: { title: string; values: number[]; format: (v: number) => string }) {
  return (
    <div className="mon-spark-cell">
      <div className="mon-spark-head">
        <span>{title}</span>
        <span className="mon-spark-now">{format(values[values.length - 1])}</span>
      </div>
      <Sparkline values={values} label={`${title} over the last ${values.length} readings`} />
      <div className="mon-spark-range muted small">
        Min {format(Math.min(...values))} · Max {format(Math.max(...values))}
      </div>
    </div>
  );
}

/** A plain inline SVG line, scaled from zero to the series maximum. */
function Sparkline({ values, label }: { values: number[]; label: string }) {
  const width = 160;
  const height = 32;
  const top = Math.max(...values);
  const step = values.length > 1 ? width / (values.length - 1) : width;
  const points = values
    .map((v, i) => {
      const y = top > 0 ? height - 1 - (v / top) * (height - 2) : height - 1;
      return `${(i * step).toFixed(1)},${y.toFixed(1)}`;
    })
    .join(" ");
  return (
    <svg className="chart-spark mon-spark" viewBox={`0 0 ${width} ${height}`} preserveAspectRatio="none" role="img" aria-label={label}>
      <polyline className="chart-line" points={points} />
    </svg>
  );
}

function SensorRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="mon-sensor">
      <span className="mon-label">{label}</span>
      <span>{value}</span>
    </div>
  );
}

function batteryText(b: NonNullable<SystemReadings["battery"]>): string {
  const parts = [`${b.percent}%`, b.charging ? "charging" : null];
  if (b.cycles != null) parts.push(`${b.cycles} cycles`);
  if (b.health != null) parts.push(`${Math.round(b.health * 100)}% health`);
  if (b.watts != null) parts.push(`${b.watts < 0 ? "-" : "+"}${Math.abs(b.watts).toFixed(1)} W`);
  if (b.celsius != null) parts.push(`${b.celsius.toFixed(1)} °C`);
  if (b.minutes != null) {
    const h = Math.floor(b.minutes / 60);
    const m = b.minutes % 60;
    const span = h > 0 ? `${h} h ${m} min` : `${m} min`;
    parts.push(`${span} to ${b.toFull ? "full" : "empty"}`);
  }
  return parts.filter(Boolean).join(" · ");
}

const capitalize = (s: string) => s.charAt(0).toUpperCase() + s.slice(1).toLowerCase();

const RANGES = [5, 15, 30];

/** CPU, memory with swap, and network over the last 5, 15 or 30 minutes. The hub keeps the history while its window is closed. */
function MonitorCharts({
  cpu: cpuNow,
  swapText,
  network: net,
  notchDown,
}: {
  cpu: number | null;
  swapText: string;
  network: { down: number; up: number; kind: string } | null;
  notchDown: boolean;
}) {
  const { samples, latest } = useMetrics();
  const [minutes, setMinutes] = useState(15);
  const times = useMemo(() => samples.map((s) => s.ts), [samples]);
  const column = (pick: (s: Sample) => number | null): (number | null)[] => samples.map(pick);

  const cpu: Series[] = [{ label: "CPU", color: levelColor(latest?.cpu ?? 0), values: column((s) => s.cpu) }];
  const memory: Series[] = [
    { label: "Memory", color: levelColor(latest ? memPercent(latest) : 0), values: column((s) => s.memUsed) },
    { label: "Swap", color: "var(--rk-ink-2)", values: column((s) => s.swapUsed), dashed: true },
  ];
  const network: Series[] = [
    { label: "Down", color: "var(--rk-accent)", values: column((s) => s.netDown) },
    { label: "Up", color: "var(--rk-ink-2)", values: column((s) => s.netUp), dashed: true },
  ];
  const rate = (v: number) => `${bytes(v)}/s`;
  const memTop = latest?.memTotal ?? 0;

  return (
    <section className="monitor-charts" aria-label="History">
      <div className="monitor-charts-head">
        <h2 className="monitor-h2">History, last {minutes} minutes</h2>
        <SegmentedControl
          label="Time range"
          value={String(minutes)}
          options={RANGES.map((m) => ({ value: String(m), label: `${m} min` }))}
          onChange={(v) => setMinutes(Number(v))}
        />
      </div>
      {samples.length < 2 ? (
        <div className="monitor-waiting muted">Collecting readings…</div>
      ) : (
        <div className="monitor-charts-grid">
          <ChartCard title="System CPU" now={cpuNow != null ? `${Math.round(cpuNow)}%` : latest ? `${Math.round(latest.cpu)}%` : "—"} extra="All cores, 0–100%">
            <AreaChart label="CPU" times={times} series={cpu} rangeMs={minutes * 60_000} max={100} format={(v) => `${Math.round(v)}%`} height={36} />
          </ChartCard>
          <ChartCard
            title="Memory used"
            now={latest ? `${bytes(latest.memUsed)} / ${bytes(latest.memTotal)}` : "—"}
            legend={memory}
            extra={swapText}
          >
            <AreaChart label="Memory" times={times} series={memory} rangeMs={minutes * 60_000} max={memTop || undefined} format={(v) => bytes(v)} height={36} />
          </ChartCard>
          <ChartCard
            title="Network"
            now={latest?.netDown != null ? `↓ ${rate(latest.netDown)}` : net ? `↓ ${rate(net.down)}` : "—"}
            legend={network}
            extra={latest?.netUp != null ? `↑ ${rate(latest.netUp)}` : net ? `↑ ${rate(net.up)} · ${net.kind}` : notchDown ? "Notch not running" : "No active interface"}
          >
            <AreaChart label="Network" times={times} series={network} rangeMs={minutes * 60_000} minTop={100_000} format={rate} height={36} />
          </ChartCard>
        </div>
      )}
    </section>
  );
}

function ChartCard({
  title,
  now,
  extra,
  legend,
  children,
}: {
  title: string;
  now: string;
  extra?: string;
  legend?: Series[];
  children: ReactNode;
}) {
  return (
    <div className="monitor-chart-card">
      <div className="monitor-chart-head">
        <span>{title}</span>
        <span className="monitor-chart-now">{now}</span>
        {extra && <span>{extra}</span>}
      </div>
      {children}
      {legend && (
        <div className="monitor-legend" aria-hidden="true">
          {legend.map((s) => (
            <span key={s.label}>
              <i className={s.dashed ? "dashed" : undefined} style={{ borderTopColor: s.color }} />
              {s.label}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
