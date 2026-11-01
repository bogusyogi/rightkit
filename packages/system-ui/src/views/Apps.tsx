import { useEffect, useMemo, useRef, useState } from "react";
import { Button, Checkbox, ConfirmDialog, Notice, SegmentedControl, Input } from "@rightkit/react-shell/react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { AppWindow, ChevronLeft, Search } from "lucide-react";
import {
  ago,
  api,
  appsApi,
  bytes,
  isWindows,
  type AppEntry,
  type AppUpdate,
  type BackgroundEntry,
  type LeftoverPart,
  type LeftoversEvent,
  type RelatedItem,
  type StartupItem,
  type StartupReport,
  type UninstallResult,
  type UpdateJob,
  type UpdateReport,
} from "../api";
import { readKept, writeKept } from "../kept";
import { upTo } from "./storageEstimate";
import "./apps.css";

const DAY = 86_400;
const UNUSED_DAYS = 90;
/** The app whose detail was open when the hub closed; the web view is destroyed on window close. */
const KEPT_APP = "pulse.hub.apps.open";
/** The open app's ticked uninstall items, kept across a hub close (#302): `{ app, picked, touched }`. */
const KEPT_PICKED = "pulse.hub.apps.picked";
interface KeptPicked {
  app: string;
  picked: string[];
  touched: string[];
}
/** Uninstalls that left failed paths, kept after Back and across a hub close until dismissed. */
const KEPT_PENDING = "pulse.hub.apps.pending";
interface PendingUninstall {
  path: string;
  name: string;
  result: UninstallResult;
}

/** Which apps the list shows: every app, ones unused for 90+ days, or ones with an update. */
type AppFilter = "all" | "unused" | "updates";

function lastUsedText(epoch: number | null): string {
  if (epoch == null) return "Unknown";
  const days = Math.floor((Date.now() / 1000 - epoch) / DAY);
  if (days <= 0) return "Today";
  if (days === 1) return "Yesterday";
  if (days < 60) return `${days} days ago`;
  return `${Math.floor(days / 30)} months ago`;
}

/** One `apps-icon` event. A null data URL means the app has no usable icon. */
interface IconEvent {
  path: string;
  data_url: string | null;
}

function byPath(apps: AppUpdate[]): Record<string, AppUpdate> {
  return Object.fromEntries(apps.map((a): [string, AppUpdate] => [a.path, a]));
}

interface Retained {
  picked: Set<string>;
  touched: Set<string>;
}

function upsertApp(list: AppEntry[], row: AppEntry): AppEntry[] {
  const index = list.findIndex((a) => a.path === row.path);
  if (index < 0) return [...list, row];
  const next = list.slice();
  next[index] = row;
  return next;
}

export function Apps() {
  const [apps, setApps] = useState<AppEntry[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [updates, setUpdates] = useState<Record<string, AppUpdate>>({});
  const [updatesAt, setUpdatesAt] = useState<number | null>(null);
  const [checking, setChecking] = useState(false);
  const [icons, setIcons] = useState<Record<string, string | null>>({});
  const [jobs, setJobs] = useState<Record<string, UpdateJob>>({});
  const [query, setQuery] = useState("");
  const [filter, setFilter] = useState<AppFilter>("all");
  const [open, setOpen] = useState<AppEntry | null>(null);
  const [error, setError] = useState<string | null>(null);
  // Failed leftovers stay listed here after Back: the app's own detail is gone once it is left.
  const [pending, setPending] = useState<PendingUninstall[]>(() => readKept<PendingUninstall[]>(KEPT_PENDING) ?? []);
  const iconsAsked = useRef(new Set<string>());
  // Per-app leftover choices survive Back and reopening.
  const retained = useRef(new Map<string, Retained>());

  /** Opening or closing an app is kept, so reopening the hub returns to that app. */
  const openApp = (entry: AppEntry | null) => {
    setOpen(entry);
    writeKept(KEPT_APP, entry?.path ?? null);
  };

  const refreshList = () => {
    setRefreshing(true);
    setError(null);
    appsApi.refresh().catch((e) => {
      setRefreshing(false);
      setError(String(e));
    });
  };

  const checkUpdates = (force: boolean) => {
    setChecking(true);
    appsApi.updatesRefresh(force).catch((e) => {
      setChecking(false);
      setError(String(e));
    });
  };

  const setJob = (path: string, job: UpdateJob) => setJobs((prev) => ({ ...prev, [path]: job }));

  useEffect(() => {
    writeKept(KEPT_PENDING, pending.length ? pending : null);
  }, [pending]);

  const keepFailed = (entry: AppEntry, result: UninstallResult) => {
    if (result.failed.length === 0) return;
    setPending((prev) => [
      ...prev.filter((p) => p.path !== entry.path),
      { path: entry.path, name: entry.name, result },
    ]);
  };

  const startUpdate = (path: string) => {
    setJob(path, { path, state: "running", message: "Starting…" });
    appsApi
      .update(path)
      .then((outcome) => {
        if (outcome === "running") return;
        setJob(path, {
          path,
          state: "done",
          message: outcome === "store" ? "Opened in the App Store." : "Opened. Use the app's own updater.",
        });
      })
      .catch((e) => setJob(path, { path, state: "failed", message: String(e) }));
  };

  // Listeners first, then the saved data, then the background refreshes.
  useEffect(() => {
    const subs: Promise<() => void>[] = [
      listen<AppEntry>("apps-row", (e) => setApps((prev) => upsertApp(prev, e.payload))),
      listen<AppEntry[]>("apps-inventory", (e) => {
        setApps(e.payload);
        setRefreshing(false);
        setLoaded(true);
      }),
      listen<AppUpdate>("apps-update-row", (e) =>
        setUpdates((prev) => ({ ...prev, [e.payload.path]: e.payload })),
      ),
      listen<UpdateReport>("apps-updates-done", (e) => {
        setUpdates(byPath(e.payload.apps));
        setUpdatesAt(e.payload.checked_at);
        setChecking(false);
      }),
      listen<IconEvent>("apps-icon", (e) =>
        setIcons((prev) => ({ ...prev, [e.payload.path]: e.payload.data_url })),
      ),
      listen<UpdateJob>("apps-update-job", (e) => {
        setJob(e.payload.path, e.payload);
        // Homebrew changed an app: check again so its row reflects the new version.
        if (e.payload.state === "done") checkUpdates(false);
      }),
    ];
    let alive = true;
    Promise.all(subs).then(async () => {
      const saved = await appsApi.cached().catch(() => null);
      if (alive && saved && saved.apps.length > 0) setApps(saved.apps);
      if (alive) refreshList();
      if (alive) setLoaded(true);
      const savedUpdates = await appsApi.updatesCached().catch(() => null);
      if (alive && savedUpdates) {
        setUpdates(byPath(savedUpdates.apps));
        setUpdatesAt(savedUpdates.checked_at);
      }
      if (alive) checkUpdates(false);
      // `--app <path>` opens straight to that app's detail; otherwise the app that was open last.
      const first = await invoke<string | null>("initial_app").catch(() => null);
      const target = first ?? readKept<string>(KEPT_APP);
      if (alive && target) {
        const entry = await appsApi.summary(target).catch((e) => {
          if (first) setError(String(e));
          else writeKept(KEPT_APP, null); // The kept app is gone (uninstalled or moved): show the list.
          return null;
        });
        if (alive && entry) openApp(entry);
      }
    });
    return () => {
      alive = false;
      subs.forEach((p) => p.then((off) => off()));
    };
  }, []);

  // Icons: ask once per app; cached ones come back now, the rest as events.
  useEffect(() => {
    const wanted = apps.map((a) => a.path).filter((p) => !iconsAsked.current.has(p));
    if (wanted.length === 0) return;
    wanted.forEach((p) => iconsAsked.current.add(p));
    appsApi
      .icons(wanted)
      .then((ready) => setIcons((prev) => ({ ...prev, ...ready })))
      .catch(() => {});
  }, [apps]);

  const shown = useMemo(() => {
    const now = Date.now() / 1000;
    const q = query.trim().toLowerCase();
    return apps
      .filter(
        (a) =>
          (!q || a.name.toLowerCase().includes(q) || (a.bundle_id ?? "").toLowerCase().includes(q)) &&
          // Unknown last-used is never treated as unused.
          (filter !== "unused" || (a.last_used != null && now - a.last_used >= UNUSED_DAYS * DAY)) &&
          (filter !== "updates" || updates[a.path]?.state === "available"),
      )
      .sort((a, b) => b.size_bytes - a.size_bytes);
  }, [apps, query, filter, updates]);

  const updateCount = useMemo(
    () => apps.filter((a) => updates[a.path]?.state === "available").length,
    [apps, updates],
  );

  if (open) {
    return (
      <Detail
        key={open.path}
        app={open}
        update={updates[open.path]}
        job={jobs[open.path]}
        retained={retained.current}
        onStartUpdate={() => startUpdate(open.path)}
        onUninstalled={(r) => keepFailed(open, r)}
        onBack={() => {
          openApp(null);
          refreshList();
        }}
      />
    );
  }

  const largest = Math.max(1, ...shown.map((a) => a.size_bytes));
  const filtered = query.trim() !== "" || filter !== "all";

  return (
    <div className="view ck-apps">
      <div className="ck-apps-tools">
        <label className="ck-apps-search">
          <Search size={15} strokeWidth={1.75} aria-hidden="true" />
          <Input
            type="search"
            aria-label="Search apps"
            placeholder="Search apps by name or bundle id"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
          />
        </label>
        <Button size="sm" variant="secondary" disabled={refreshing} onClick={refreshList}>
          {refreshing ? "Refreshing…" : "Refresh"}
        </Button>
        <Button size="sm" variant="secondary" disabled={checking} onClick={() => checkUpdates(true)}>
          {checking ? "Checking…" : "Check updates"}
        </Button>
      </div>
      <div className="ck-apps-filters">
        <SegmentedControl
          label="Show apps"
          value={filter}
          options={[
            { value: "all", label: "All" },
            { value: "unused", label: `Unused ${UNUSED_DAYS}+ days` },
            { value: "updates", label: updateCount > 0 ? `Updates (${updateCount})` : "Updates" },
          ]}
          onChange={(v) => setFilter(v as AppFilter)}
        />
        {apps.length > 0 && (
          <span className="ck-apps-summary" role="status">
            {shown.length} of {apps.length} apps · {bytes(shown.reduce((n, a) => n + a.size_bytes, 0))} · largest first
            {refreshing ? " · sizes updating…" : ""}
            {checking
              ? " · checking for updates…"
              : updatesAt != null
                ? ` · updates checked ${ago(updatesAt)}`
                : ""}
          </span>
        )}
      </div>
      {error && (
        <div className="ck-apps-error" role="alert">
          {error}
        </div>
      )}
      {pending.map((p) => (
        <Notice key={p.path} tone="warn">
          <strong>{p.name}: {p.result.failed.length} failed and still listed</strong>
          {p.result.failed.map((f) => (
            <div key={f.path} className="ck-apps-failed" title={f.path}>
              {f.path.replace(/^\/Users\/[^/]+/, "~")}: {f.error}{" "}
              <Button size="sm" variant="secondary" aria-label={`Reveal ${f.path}`} onClick={() => api.reveal(f.path).catch((e) => setError(String(e)))}>
                Reveal
              </Button>
            </div>
          ))}
          <Button size="sm" variant="secondary" onClick={() => setPending((prev) => prev.filter((x) => x.path !== p.path))}>
            Dismiss
          </Button>
        </Notice>
      ))}
      <div className="ck-apps-card" tabIndex={0} aria-label="Installed apps">
        {apps.length === 0 && (refreshing || !loaded) && (
          <div className="ck-apps-state" role="status">
            <strong>Reading your applications…</strong>
            <span>Checking sizes and last use</span>
          </div>
        )}
        {apps.length === 0 && loaded && !refreshing && (
          <div className="ck-apps-state">
            <strong>No applications found</strong>
            <span>Refresh to read them again.</span>
          </div>
        )}
        {apps.length > 0 && shown.length === 0 && (
          <div className="ck-apps-state">
            <strong>No matching apps</strong>
            <span>Clear the search or filter, or refresh the list.</span>
            {filtered && (
              <Button
                size="sm"
                variant="secondary"
                onClick={() => {
                  setQuery("");
                  setFilter("all");
                }}
              >
                Clear filters
              </Button>
            )}
          </div>
        )}
        {shown.map((a) => (
          <AppRow
            key={a.path}
            app={a}
            largest={largest}
            icon={icons[a.path]}
            update={updates[a.path]}
            job={jobs[a.path]}
            onOpen={() => openApp(a)}
            onUpdate={() => startUpdate(a.path)}
          />
        ))}
      </div>
      <details className="ck-apps-about">
        <summary>About these numbers</summary>
        <p>
          Sizes are measured on disk and keep updating while the list refreshes. Unused means not opened for{" "}
          {UNUSED_DAYS}+ days; an unknown last use never counts as unused. Updates show what the last check found. Apps
          that update themselves open their own updater. {isWindows
            ? "Removing an app runs its own uninstaller; leftover folders you pick go to the Recycle Bin. Last used comes from what Windows recorded when you launched the app from Explorer or Start, so many apps show Unknown."
            : "Removing an app moves it to the Trash."}
        </p>
      </details>
      <StartsByItself />
    </div>
  );
}

const SCOPES: { scope: StartupItem["scope"]; label: string }[] = [
  { scope: "user", label: "Your account" },
  { scope: "all users", label: "All users" },
  { scope: "system", label: "System" },
];

/** Middle-truncates long text; the full value stays in the title. */
function middle(text: string, max = 60): string {
  if (text.length <= max) return text;
  const head = Math.ceil((max - 1) / 2);
  const tail = Math.floor((max - 1) / 2);
  return `${text.slice(0, head)}…${text.slice(text.length - tail)}`;
}

function StateText({ enabled }: { enabled: boolean | null }) {
  if (enabled === true) return <span className="ck-apps-badge ck-apps-badge--ok">On</span>;
  if (enabled === false) return <span className="ck-apps-badge">Off</span>;
  return <span className="ck-apps-badge">Unknown</span>;
}

/** Login and background items that start by themselves. Read only; loaded when opened, not on a timer. */
function StartsByItself() {
  const [report, setReport] = useState<StartupReport | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const load = () => {
    setLoading(true);
    setError(null);
    appsApi
      .startupItems()
      .then(setReport)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  };

  return (
    <details
      className="ck-apps-startup"
      onToggle={(e) => {
        if (e.currentTarget.open && report === null && !loading) load();
      }}
    >
      <summary>Starts by itself</summary>
      <div className="ck-apps-startup-body">
        <div className="ck-apps-startup-tools">
          <span className="ck-apps-sub" role="status">
            {loading ? "Reading…" : report ? `${report.rows.length} items` : ""}
          </span>
          <Button size="sm" variant="secondary" disabled={loading} onClick={load}>
            {loading ? "Reading…" : "Refresh"}
          </Button>
        </div>
        {error && (
          <div className="ck-apps-error" role="alert">
            {error}
          </div>
        )}
        {loading && !report && (
          <div className="ck-apps-state" role="status">
            <strong>Reading startup items…</strong>
          </div>
        )}
        {report && report.rows.length === 0 && (
          <div className="ck-apps-state">
            <strong>Nothing starts by itself</strong>
            <span>No launch items or Run entries were found.</span>
          </div>
        )}
        {report &&
          SCOPES.map(({ scope, label }) => {
            const rows = report.rows.filter((r) => r.scope === scope);
            if (rows.length === 0) return null;
            return (
              <div key={scope} className="ck-apps-startup-group" role="group" aria-label={label}>
                <h3 className="ck-apps-startup-scope">{label}</h3>
                {rows.map((row, index) => (
                  <div key={`${scope}:${row.kind}:${row.name}:${index}`} className="ck-apps-startrow">
                    <span className="ck-apps-text">
                      <span className="ck-apps-name">{row.name}</span>
                      <span className="ck-apps-sub">{row.kind}</span>
                    </span>
                    <span className="ck-apps-startwhat" title={row.program ?? row.path ?? undefined}>
                      {row.program ? middle(row.program) : "Not known"}
                    </span>
                    <StateText enabled={row.enabled} />
                    <Button
                      size="sm"
                      variant="secondary"
                      disabled={!row.path}
                      title={row.path ? undefined : "No file to reveal for this item"}
                      aria-label={`Reveal ${row.name}`}
                      onClick={() => {
                        if (row.path) api.reveal(row.path).catch((e) => setError(String(e)));
                      }}
                    >
                      Reveal
                    </Button>
                  </div>
                ))}
              </div>
            );
          })}
        {report && report.notes.length > 0 && (
          <div className="ck-apps-note">
            {report.notes.map((note) => (
              <p key={note}>{note}</p>
            ))}
          </div>
        )}
      </div>
    </details>
  );
}

function AppIcon({ src }: { src: string | null | undefined }) {
  if (src) return <img className="ck-apps-icon" src={src} alt="" />;
  return (
    <span className="ck-apps-icon ck-apps-icon--fallback" aria-hidden="true">
      <AppWindow size={16} strokeWidth={1.75} />
    </span>
  );
}

/** The update badge and action for one app. It sits beside the open button, never inside it. */
function UpdateControl({
  update,
  job,
  onUpdate,
}: {
  update: AppUpdate | undefined;
  job: UpdateJob | undefined;
  onUpdate: () => void;
}) {
  if (job?.state === "running") {
    return (
      <span className="ck-apps-update" role="status">
        <span className="ck-apps-status">{job.message}</span>
      </span>
    );
  }
  if (job?.state === "failed") {
    return (
      <span className="ck-apps-update" role="alert">
        <span className="ck-apps-status ck-apps-status--bad" title={job.message}>
          Update failed
        </span>
      </span>
    );
  }
  if (job?.state === "done") {
    return (
      <span className="ck-apps-update" role="status">
        <span className="ck-apps-status ck-apps-status--ok" title={job.message}>
          {job.message}
        </span>
      </span>
    );
  }
  if (update?.state === "available") {
    return (
      <span className="ck-apps-update">
        <span className="ck-apps-badge ck-apps-badge--accent">
          Update available{update.latest_version ? ` ${update.latest_version}` : ""}
        </span>
        {update.reason ? (
          <span className="ck-apps-status" title={update.reason}>
            {update.reason}
          </span>
        ) : null}
        <Button size="sm" variant="secondary" onClick={onUpdate}>
          Update
        </Button>
      </span>
    );
  }
  if (update?.state === "app_store") {
    return (
      <span className="ck-apps-update">
        <Button size="sm" variant="secondary" onClick={onUpdate}>
          Open in App Store
        </Button>
      </span>
    );
  }
  return null;
}

function AppRow({
  app,
  largest,
  icon,
  update,
  job,
  onOpen,
  onUpdate,
}: {
  app: AppEntry;
  largest: number;
  icon: string | null | undefined;
  update: AppUpdate | undefined;
  job: UpdateJob | undefined;
  onOpen: () => void;
  onUpdate: () => void;
}) {
  return (
    <div className="ck-apps-row">
      <Button variant="ghost" className="ck-apps-open" onClick={onOpen} title={app.path}>
        <AppIcon src={icon} />
        <span className="ck-apps-text">
          <span className="ck-apps-name">
            {app.name}
            {app.running && <span className="ck-apps-badge ck-apps-badge--ok">Running</span>}
            {app.protected && <span className="ck-apps-badge">Protected</span>}
          </span>
          <span className="ck-apps-sub">
            {app.version ? `${app.version} · ` : ""}Last used: {lastUsedText(app.last_used)}
          </span>
        </span>
      </Button>
      <UpdateControl update={update} job={job} onUpdate={onUpdate} />
      <span className="ck-apps-size">
        {bytes(app.size_bytes)}
        <span className="ck-apps-bar" aria-hidden="true">
          <i style={{ width: `${(app.size_bytes / largest) * 100}%` }} />
        </span>
      </span>
    </div>
  );
}

const LOCATIONS = [
  "Application",
  "User Library",
  "System Library",
  "Your app data",
  "Shared app data",
  "Installer receipt",
];
const CONFIDENCE: Record<string, string> = {
  exact: "Exact id",
  helper: "Helper id",
  group: "App group",
  prefix: "Id prefix",
  team: "Team id",
  name: "Name",
  receipt: "Receipt",
  install: "Registered app",
  publisher: "Publisher and product",
};

/** The bundle row from the list's facts, shown before the bundle part arrives. */
function bundleRow(app: AppEntry): RelatedItem {
  return {
    path: app.path,
    label: "Application",
    location: "Application",
    exact: true,
    confidence: "exact",
    reason: "The application bundle",
    admin: false,
    size_bytes: app.size_bytes,
    preselected: app.protected == null,
  };
}

function mergeBackground(prev: BackgroundEntry[], more: BackgroundEntry[]): BackgroundEntry[] {
  const seen = new Set(prev.map((b) => `${b.kind}:${b.label}`));
  const out = [...prev];
  for (const b of more) {
    const key = `${b.kind}:${b.label}`;
    if (!seen.has(key)) {
      seen.add(key);
      out.push(b);
    }
  }
  return out;
}
function Detail({
  app,
  update,
  job,
  retained,
  onStartUpdate,
  onUninstalled,
  onBack,
}: {
  app: AppEntry;
  update: AppUpdate | undefined;
  job: UpdateJob | undefined;
  retained: Map<string, Retained>;
  onStartUpdate: () => void;
  onUninstalled: (result: UninstallResult) => void;
  onBack: () => void;
}) {
  const [items, setItems] = useState<Map<string, RelatedItem>>(() => new Map([[app.path, bundleRow(app)]]));
  const [background, setBackground] = useState<BackgroundEntry[]>([]);
  const [receipts, setReceipts] = useState<string[]>([]);
  // The ticks from before the hub window closed (its web view is destroyed), for this app only.
  const keptPicked = useRef<KeptPicked | null>(
    (() => {
      const k = readKept<KeptPicked>(KEPT_PICKED);
      return k && k.app === app.path ? k : null;
    })(),
  );
  const [picked, setPicked] = useState<Set<string>>(
    () =>
      new Set(
        retained.get(app.path)?.picked ?? keptPicked.current?.picked ?? (app.protected == null ? [app.path] : []),
      ),
  );
  // Paths the user has changed: leftovers arriving later never overrule them.
  const touched = useRef(
    new Set<string>(retained.get(app.path)?.touched ?? keptPicked.current?.touched),
  );
  const [status, setStatus] = useState<"loading" | "done" | "error">("loading");
  const [leftoverError, setLeftoverError] = useState<string | null>(null);
  const [confirm, setConfirm] = useState(false);
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<UninstallResult | null>(null);
  const [error, setError] = useState<string | null>(null);

  const mergePart = (part: LeftoverPart) => {
    setItems((prev) => {
      const next = new Map(prev);
      for (const item of part.items) {
        const old = next.get(item.path);
        if (!old || part.source === "bundle" || (item.preselected && !old.preselected)) {
          next.set(item.path, item);
        }
      }
      return next;
    });
    const auto = part.items
      .filter((i) => i.preselected && !touched.current.has(i.path))
      .map((i) => i.path);
    if (auto.length > 0) setPicked((prev) => new Set([...prev, ...auto]));
    if (part.background.length > 0) setBackground((prev) => mergeBackground(prev, part.background));
    if (part.receipts.length > 0) setReceipts((prev) => [...new Set([...prev, ...part.receipts])]);
  };

  useEffect(() => {
    const sub = listen<LeftoversEvent>("apps-leftovers", (e) => {
      const event = e.payload;
      if (event.path !== app.path) return;
      if (event.kind === "part") {
        mergePart(event.part);
        return;
      }
      setLeftoverError(event.error);
      setStatus(event.error ? "error" : "done");
    });
    sub
      .then(() => appsApi.leftovers(app.path))
      .catch((err) => {
        setLeftoverError(String(err));
        setStatus("error");
      });
    return () => {
      sub.then((off) => off());
    };
  }, [app.path]);

  const all = [...items.values()];
  // Installer files under the app or a Library item are already listed there.
  const taken = [
    app.path,
    ...all.filter((i) => i.location === "User Library" || i.location === "System Library").map((i) => i.path),
  ];
  const visible = all.filter(
    (i) => i.location !== "Installer receipt" || !taken.some((t) => i.path === t || i.path.startsWith(`${t}/`)),
  );
  const pickedVisible = visible.filter((i) => picked.has(i.path));
  const total = pickedVisible.reduce((n, i) => n + i.size_bytes, 0);
  const adminPicked = pickedVisible.filter((i) => i.admin).length;

  const set = (paths: string[], on: boolean) => {
    for (const p of paths) touched.current.add(p);
    setPicked((prev) => {
      const next = new Set(prev);
      for (const p of paths) {
        if (on) next.add(p);
        else next.delete(p);
      }
      return next;
    });
  };

  const run = async () => {
    setConfirm(false);
    setBusy(true);
    setError(null);
    try {
      const r = await api.uninstall(app.path, app.bundle_id, pickedVisible.map((i) => i.path));
      setResult(r);
      onUninstalled(r);
      const gone = new Set(r.moved.map((m) => m.path));
      setItems((prev) => {
        const next = new Map(prev);
        for (const p of gone) next.delete(p);
        return next;
      });
      setPicked(new Set());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  useEffect(() => {
    retained.set(app.path, { picked, touched: touched.current });
    writeKept(KEPT_PICKED, { app: app.path, picked: [...picked], touched: [...touched.current] });
  }, [picked]);

  // Once the leftover scan has finished, a kept tick whose item is gone is dropped. Uninstall
  // still asks for confirmation.
  useEffect(() => {
    if (status !== "done") return;
    setPicked((prev) => {
      const next = new Set([...prev].filter((p) => items.has(p)));
      return next.size === prev.size ? prev : next;
    });
  }, [status]);


  const reveal = (path: string) => {
    api.reveal(path).catch((e) => setError(String(e)));
  };

  const group = (location: string) => {
    const rows = visible.filter((i) => i.location === location);
    if (rows.length === 0) return null;
    const paths = rows.map((i) => i.path);
    const allOn = paths.every((p) => picked.has(p));
    return (
      <section key={location} className="ck-apps-group" aria-label={location}>
        <div className="ck-apps-group-head">
          <span>
            {location} <span className="muted">· {rows.length} · {bytes(rows.reduce((n, i) => n + i.size_bytes, 0))}</span>
          </span>
          {rows.length > 1 && (
            <Button size="sm" variant="secondary" disabled={busy} onClick={() => set(paths, !allOn)}>
              {allOn ? "Deselect group" : "Select group"}
            </Button>
          )}
        </div>
        {rows.map((i) => (
          <div
            key={i.path}
            className="ck-apps-item"
            title={i.path}
            onClick={(e) => {
              // The whole row toggles, as the label it replaces did; its own checkbox and Reveal handle their own clicks.
              if (busy || (e.target as HTMLElement).closest("button, .rk-check")) return;
              set([i.path], !picked.has(i.path));
            }}
          >
            <Checkbox
              checked={picked.has(i.path)}
              disabled={busy}
              onChange={(on) => set([i.path], on)}
              label={<span className="ov-vh">Select {i.path.replace(/^\/Users\/[^/]+/, "~")}</span>}
            />
            <span className="ck-apps-item-main">
              <span className="ck-apps-path">{i.path.replace(/^\/Users\/[^/]+/, "~")}</span>
              <span className="ck-apps-sub">{i.reason}</span>
              <span className="ck-apps-tags">
                <span className="ck-apps-badge">Match: {CONFIDENCE[i.confidence] ?? i.confidence}</span>
                {i.admin && <span className="ck-apps-badge ck-apps-badge--warn">Admin required</span>}
                {!i.preselected && <span className="ck-apps-badge ck-apps-badge--warn">Review before moving</span>}
                <Button
                  size="sm"
                  variant="secondary"
                  disabled={!i.path}
                  title={i.path ? undefined : "No file to reveal for this item"}
                  aria-label={`Reveal ${i.path}`}
                  onClick={() => reveal(i.path)}
                >
                  Reveal
                </Button>
              </span>
            </span>
            <span className="ck-apps-size">{bytes(i.size_bytes)}</span>
          </div>
        ))}
      </section>
    );
  };

  return (
    <div className="view ck-apps">
      <div className="ck-apps-head">
        <Button size="sm" variant="secondary" onClick={onBack}>
          <span className="ck-apps-back">
            <ChevronLeft size={14} strokeWidth={1.75} aria-hidden="true" />
            Back to apps
          </span>
        </Button>
        <div className="ck-apps-head-title">
          <h2>{app.name}</h2>
          <div className="ck-apps-sub">
            {[app.version, app.bundle_id, `Last used: ${lastUsedText(app.last_used)}`].filter(Boolean).join(" · ")}
          </div>
        </div>
        <UpdateControl update={update} job={job} onUpdate={onStartUpdate} />
        <Button
          size="sm"
          variant="secondary"
          disabled={!app.path}
          title={app.path ? undefined : "This entry has no file or folder to reveal"}
          aria-label={`Reveal ${app.name}`}
          onClick={() => reveal(app.path)}
        >
          Reveal
        </Button>
      </div>
      {app.protected && <Notice tone="warn">{app.protected} Uninstall is not offered.</Notice>}
      {app.running && !app.protected && (
        <Notice tone="warn">
          Running. Pulse will ask it to quit first, and will not remove anything if it stays open.
        </Notice>
      )}
      {status === "loading" && (
        <Notice>
          Finding leftovers…
        </Notice>
      )}
      {leftoverError && (
        <div className="ck-apps-error" role="alert">
          {leftoverError}
        </div>
      )}
      {error && (
        <div className="ck-apps-error" role="alert">
          {error}
        </div>
      )}
      {result && (
        <Notice tone={result.failed.length > 0 ? "warn" : "ok"}>
          {isWindows
            ? `Started the uninstaller${result.moved.length > 0 ? ` · moved ${result.moved.length} leftover${result.moved.length === 1 ? "" : "s"} to the Recycle Bin: ${upTo(result.bytes_moved)} on disk · restore from the Recycle Bin · Space is not freed until the Recycle Bin is emptied.` : ""}`
            : `Moved ${result.moved.length} to the Trash: ${upTo(result.bytes_moved)} on disk · restore with Put Back in Finder · Space is not freed until the Trash is emptied.`}
          {result.failed.length > 0 && <strong> · {result.failed.length} failed and still listed</strong>}
          {result.failed.map((f) => (
            <div key={f.path} className="ck-apps-failed" title={f.path}>
              {f.path.replace(/^\/Users\/[^/]+/, "~")}: {f.error}
            </div>
          ))}
        </Notice>
      )}
      <div className="ck-apps-card" tabIndex={0} aria-label="Associated files">
        {LOCATIONS.map(group)}
        {background.length > 0 && (
          <div className="ck-apps-extra">
            <h3>Login and background items</h3>
            {background.map((b) => (
              <div key={`${b.kind}:${b.label}`} title={b.path ?? ""}>
                {b.kind} · {b.label}
              </div>
            ))}
          </div>
        )}
        {receipts.length > 0 && <div className="ck-apps-extra">Installer packages: {receipts.join(", ")}</div>}
        <details className="ck-apps-about">
          <summary>About these numbers</summary>
          <p>
            Sizes are measured on disk. Match shows how the file was tied to the app; items marked for review are left
            unchecked. {isWindows
              ? "Leftover folders go to the Recycle Bin and can be restored. Registry and startup entries are listed only; Pulse never removes them."
              : "Everything goes to the Trash and can be put back."}
          </p>
        </details>
      </div>
      {!app.protected && (
        <div className="ck-apps-foot">
          <div className="ck-apps-foot-copy">
            <strong>
              {pickedVisible.length} selected · {bytes(total)}
            </strong>
            <span className="ck-apps-sub">
              {adminPicked > 0 && `${adminPicked} need an administrator password · `}
              {app.running ? "Must quit first · " : ""}{isWindows
                ? "Runs the app's uninstaller; leftovers go to the Recycle Bin"
                : "Goes to Trash, restore with Put Back"}
            </span>
          </div>
          <Button
            size="sm"
            variant="danger"
            disabled={busy || status === "loading" || pickedVisible.length === 0}
            onClick={() => setConfirm(true)}
          >
            {busy ? "Working…" : isWindows ? "Uninstall" : "Move to Trash"}
          </Button>
        </div>
      )}
      {confirm && (
        <ConfirmDialog
          danger
          title={isWindows ? `Uninstall ${app.name}?` : `Move ${pickedVisible.length} item${pickedVisible.length === 1 ? "" : "s"} to Trash?`}
          description={isWindows
            ? `${app.name}'s own uninstaller opens and may ask for administrator rights. Once it has finished, any leftover folders you selected go to the Recycle Bin. If the uninstaller is cancelled, none are touched.`
            : `${app.name} and the selected files, ${bytes(total)} in all. Everything goes to the Trash, so it can be put back.${
                app.running ? " The app will be asked to quit first." : ""
              }${adminPicked > 0 ? " Root-owned items make Finder ask for your password." : ""}`}
          confirmLabel={isWindows ? "Uninstall" : "Move to Trash"}
          onConfirm={run}
          onCancel={() => setConfirm(false)}
        />
      )}
    </div>
  );
}
