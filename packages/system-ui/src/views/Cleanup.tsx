import { useEffect, useMemo, useRef, useState } from "react";
import { Badge, Button, Checkbox, ConfirmDialog, Notice, SegmentedControl } from "@rightkit/react-shell/react";
import { invoke } from "@tauri-apps/api/core";
import { ChevronRight } from "lucide-react";
import { ago, bytes, isStale, isWindows, type ChromeSnapshots } from "../api.js";
import { readKept, writeKept } from "../kept.js";

// Windows moves files to the Recycle Bin; the Mac moves them to the Trash.
const TRASH = isWindows ? "Recycle Bin" : "Trash";
const KEPT_SELECTED = "pulse.hub.cleanup.selected";
/** How each past move was measured, by activity id: a lower bound (≥) or an upper bound (up to). Absent means exact. */
const KEPT_BOUNDS = "pulse.hub.cleanup.bounds";
type MoveBound = "lower" | "upper";
import { ChromeSnapshotsLine } from "./ChromeSnapshots.js";

interface Finding {
  id: string;
  rule_id: string;
  category: string;
  name: string;
  path: string;
  bytes: number;
  partial: boolean;
  /** The size is the most deleting could free; hard links or clones may free less. */
  upper_bound?: boolean;
  risk: "safe" | "review" | "info";
  eligible: boolean;
  preselected: boolean;
  reason: string;
  action: string;
  dev: string;
  ino: string;
  /** Plain-language kind: "App cache", "RustRight build target", "Virtual machine", ... */
  kind?: string;
  /** Plain-language safety: "rebuildable cache", "experiment output", "virtual machine", "old release build". */
  safety?: string;
  /** The app, tool or project that made the item. */
  owner?: string;
  /** Unix seconds the item was last written; absent or 0 when unknown. */
  last_used?: number;
}

interface Report {
  chrome_snapshots?: ChromeSnapshots | null;
  findings: Finding[];
  safe_bytes: number;
  review_bytes: number;
  scanned_at: number;
}

interface Skipped {
  path: string;
  reason: string;
}

interface ApplyResult {
  moved_items: number;
  moved_bytes: number;
  skipped: Skipped[];
  activity_id: string | null;
  warnings?: string[];
  free_space?: VolumeFree[];
}

/** What the drive itself reported as available before and after a cleanup. */
interface VolumeFree {
  volume: string;
  before_available: number;
  after_available: number;
}

interface RestoreResult {
  restored_items: number;
  restored_bytes: number;
  skipped: Skipped[];
  warnings?: string[];
}

interface ActivityItem {
  path: string;
  trash_path: string | null;
  bytes: number;
  restored: boolean;
  /** Set when a crash left this item's state unknown; Undo refuses it. */
  interrupted?: string | null;
}

interface Activity {
  id: string;
  at: number;
  bytes: number;
  items: ActivityItem[];
  /** Absent in older history, where it means 0. */
  skipped?: number;
  /** Items a crash left unconfirmed; absent in older history, where it means 0. */
  unknown?: number;
  /** True when the host app was interrupted during this cleanup and settled it afterwards. */
  recovered?: boolean;
  /** Observed free space per drive; absent when it could not be read and in older history. */
  free_space?: VolumeFree[];
}

interface CleanupHistory {
  actions: Activity[];
  warnings: string[];
}

const cleanup = {
  scan: () => invoke<Report>("cleanup_scan"),
  cached: () => invoke<Report | null>("cleanup_cached"),
  apply: (items: { rule_id: string; path: string; dev: string; ino: string }[]) =>
    invoke<ApplyResult>("cleanup_apply", { items }),
  history: () => invoke<CleanupHistory>("cleanup_history"),
  restore: (id: string) => invoke<RestoreResult>("cleanup_restore", { id }),
};

const shortPath = (path: string) => path.replace(/^\/Users\/[^/]+/, "~");
const items = (n: number) => `${n} item${n === 1 ? "" : "s"}`;

// The plain-words warning for one run, or null when nothing needs saying.
const runWarning = (a: Activity): string | null => {
  const parts: string[] = [];
  const unknown = a.unknown ?? 0;
  const skipped = a.skipped ?? 0;
  if (unknown > 0) parts.push(`${items(unknown)} could not be confirmed`);
  if (skipped > 0) parts.push(`${items(skipped)} ${skipped === 1 ? "was" : "were"} skipped`);
  const detail = parts.join(" and ");
  if (a.recovered) {
    const lead = "Pulse was interrupted during this cleanup";
    return detail ? `${lead}; ${detail}.` : `${lead}.`;
  }
  if (!detail) return null;
  return `${detail.charAt(0).toUpperCase()}${detail.slice(1)}.`;
};

// One predicate for what can be selected, totalled and submitted. The backend
// still decides again when the move runs, and unknown liveness never passes.
const movable = (f: Finding) =>
  f.eligible && f.action === "Move to Trash" && (f.risk === "safe" || f.risk === "review") && !!f.dev && !!f.ino;

const total = (xs: Finding[]) => {
  const sum = xs.reduce((s, f) => s + f.bytes, 0);
  const prefix = xs.some((f) => f.partial) ? "≥ " : xs.some((f) => f.upper_bound) ? "up to " : "";
  return `${prefix}${bytes(sum)}`;
};

// The plain facts under an item's name: what it is, how safe it is to move, who made it, when it was last written.
const facts = (f: Finding) =>
  [f.kind, f.safety, f.owner ? `from ${f.owner}` : null, f.last_used ? `last used ${ago(f.last_used)}` : "last use unknown"]
    .filter(Boolean)
    .join(" · ");

// Wording for a size the scan could not prove would be freed.
const ESTIMATE = "An estimate from allocated size on disk, not file size. Hard links and clones can make the real gain smaller.";

// The drive's own free-space reading around a run: observed, and moved by anything else using the drive too.
const freeSpaceLine = (v: VolumeFree) => {
  const change = v.after_available - v.before_available;
  const amount = change === 0 ? "none" : `${change > 0 ? "+" : "-"}${bytes(Math.abs(change))}`;
  return `Free space on ${v.volume} changed by ${amount} (observed).`;
};
const FREE_SPACE_CAVEAT = `Items in the ${TRASH} stay on the drive, so space is not freed until the ${TRASH} is emptied. Other activity on the drive also moves this number.`;

type Pane = "findings" | "history";
type Note = { tone: "ok" | "warn" | "error"; text: string; detail?: string };

function GroupHead({
  title,
  xs,
  selected,
  onToggle,
  disabled,
}: {
  title: string;
  xs: Finding[];
  selected: Set<string>;
  onToggle: (on: boolean) => void;
  disabled: boolean;
}) {
  const count = xs.filter((f) => selected.has(f.id)).length;
  return (
    <div className="ck-group-head">
      <Checkbox
        checked={count === xs.length}
        indeterminate={count > 0 && count < xs.length}
        onChange={onToggle}
        disabled={disabled}
        label={<strong aria-label={`Select all ${title.toLowerCase()} items`}>{title}</strong>}
      />
      <span className="muted ck-num">{total(xs)}</span>
    </div>
  );
}

/** `autoScan` is Settings > General > Background > "Scan when Storage or Cleanup opens" (on by default). */
export function Cleanup({ autoScan = true }: { autoScan?: boolean }) {
  const [report, setReport] = useState<Report | null>(null);
  const [history, setHistory] = useState<Activity[]>([]);
  const [historyWarnings, setHistoryWarnings] = useState<string[]>([]);
  // Kept apart from `error`, so a rescan's clear does not hide a failed history load.
  const [historyError, setHistoryError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  // The selection from before the window was closed; it replaces the preselection on the first scan.
  const kept = useRef<string[] | null>(readKept<string[]>(KEPT_SELECTED));
  const [pending, setPending] = useState<Finding[] | null>(null);
  const [pane, setPane] = useState<Pane>("findings");
  const [busy, setBusy] = useState(false);
  const [refreshing, setRefreshing] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState<Note | null>(null);
  const [restoreIssue, setRestoreIssue] = useState<Record<string, string>>({});
  // Recorded when each move ran, since the history itself does not carry the scan's bounds.
  const [bounds, setBounds] = useState<Record<string, MoveBound>>(() => readKept<Record<string, MoveBound>>(KEPT_BOUNDS) ?? {});
  const seeded = useRef(false);
  // One findings scan at a time; a request made while one runs asks for another after it.
  const scanning = useRef(false);
  const again = useRef(false);

  useEffect(() => {
    // Until the first scan has replaced the kept selection, the empty set must not overwrite it.
    if (!seeded.current) return;
    writeKept(KEPT_SELECTED, selected.size ? [...selected] : null);
  }, [selected]);

  const run = async (work: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await work();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  // Preselected items are chosen once; later scans keep only what is still movable.
  // A move waiting on confirmation keeps the items that are still offered.
  const show = (next: Report) => {
    const first = !seeded.current;
    seeded.current = true;
    setReport(next);
    const now = next.findings.filter(movable);
    const restored = first ? kept.current : null;
    if (first) kept.current = null;
    setSelected((prev) =>
      first
        ? restored
          ? new Set(now.filter((f) => restored.includes(f.id)).map((f) => f.id))
          : new Set(now.filter((f) => f.preselected).map((f) => f.id))
        : new Set(now.filter((f) => prev.has(f.id)).map((f) => f.id)),
    );
    setPending((prev) => {
      const kept = prev ? now.filter((f) => prev.some((p) => p.id === f.id)) : [];
      return kept.length ? kept : null;
    });
  };

  // A rescan runs behind the page, which stays usable while it runs: the saved
  // findings can be selected and moved meanwhile, since every item is checked
  // again when it moves.
  const refresh = async () => {
    if (scanning.current) {
      again.current = true;
      return;
    }
    scanning.current = true;
    setRefreshing(true);
    setError(null);
    try {
      do {
        again.current = false;
        show(await cleanup.scan());
      } while (again.current);
    } catch (e) {
      setError(String(e));
    } finally {
      scanning.current = false;
      setRefreshing(false);
    }
  };

  // A failed history load stays on screen until a later load succeeds.
  const loadHistory = () =>
    cleanup
      .history()
      .then((next) => {
        setHistory(next.actions);
        setHistoryWarnings(next.warnings);
        setHistoryError(null);
      })
      .catch((e) => setHistoryError(String(e)));

  // Saved findings show at once. A rescan follows only when there are none or they are old.
  useEffect(() => {
    void loadHistory();
    cleanup
      .cached()
      .then((saved) => {
        if (saved) show(saved);
        if (autoScan && (!saved || isStale(saved.scanned_at))) void refresh();
      })
      .catch(() => { if (autoScan) void refresh(); });
  }, []);

  const all = report?.findings ?? [];
  const eligible = all.filter(movable);
  const safe = eligible.filter((f) => f.risk === "safe");
  const review = eligible.filter((f) => f.risk === "review");
  const held = all.filter((f) => !movable(f));
  const chosen = useMemo(() => eligible.filter((f) => selected.has(f.id)), [report, selected]);
  // Where the space is: every item the scan found, by category, whether or not it can be moved.
  const byCategory = useMemo(() => {
    const groups = new Map<string, Finding[]>();
    for (const f of all) groups.set(f.category, [...(groups.get(f.category) ?? []), f]);
    return [...groups.entries()]
      .map(([category, xs]) => ({
        category,
        xs,
        bytes: xs.reduce((s, f) => s + f.bytes, 0),
        safety: [...new Set(xs.map((f) => f.safety).filter(Boolean))].join(", "),
      }))
      .sort((a, b) => b.bytes - a.bytes);
  }, [report]);

  const toggle = (id: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  // One button for the common case: select the safe items and Chrome's signing copies for review.
  // It only selects; nothing moves until the person confirms the move.
  const freeable = eligible.filter((f) => f.risk === "safe" || f.rule_id === "chrome-signing-copies");
  const freeUp = () => setSelected(new Set(freeable.map((f) => f.id)));

  const toggleGroup = (xs: Finding[], on: boolean) =>
    setSelected((prev) => {
      const next = new Set(prev);
      xs.forEach((f) => (on ? next.add(f.id) : next.delete(f.id)));
      return next;
    });

  const move = (picked: Finding[]) => {
    setPending(null);
    return run(async () => {
      const sent = picked.filter(movable);
      const result = await cleanup.apply(sent.map((f) => ({ rule_id: f.rule_id, path: f.path, dev: f.dev, ino: f.ino })));
      const warnings = result.warnings ?? [];
      if (result.activity_id) {
        const bound: MoveBound | null = sent.some((f) => f.partial) ? "lower" : sent.some((f) => f.upper_bound) ? "upper" : null;
        if (bound) {
          const next = { ...bounds, [result.activity_id]: bound };
          setBounds(next);
          writeKept(KEPT_BOUNDS, next);
        }
      }
      const detail = [...result.skipped.map((s) => `${shortPath(s.path)}: ${s.reason}`), ...warnings].join("\n");
      setNote(
        result.skipped.length
          ? {
              tone: "warn",
              text: `Partly moved: ${bytes(result.moved_bytes)} (${items(result.moved_items)}) went to the ${TRASH}, ${result.skipped.length} skipped.`,
              detail,
            }
          : warnings.length
            ? {
                tone: "warn",
                text: `Moved to the ${TRASH}: ${bytes(result.moved_bytes)} (${items(result.moved_items)}), with a warning.`,
                detail,
              }
            : { tone: "ok", text: `Moved to the ${TRASH}: ${bytes(result.moved_bytes)} (${items(result.moved_items)}).`, detail: (result.free_space ?? []).length ? `${(result.free_space ?? []).map(freeSpaceLine).join(" ")} ${FREE_SPACE_CAVEAT}` : undefined },
      );
      // What moved leaves the list at once; the rescan behind it confirms.
      const left = new Set(result.skipped.map((s) => s.path));
      const gone = new Set(sent.filter((f) => !left.has(f.path)).map((f) => f.id));
      setReport((prev) => (prev ? { ...prev, findings: prev.findings.filter((f) => !gone.has(f.id)) } : prev));
      setSelected((prev) => new Set([...prev].filter((id) => !gone.has(id))));
      await loadHistory();
      void refresh();
    });
  };

  const restore = (id: string) =>
    run(async () => {
      setRestoreIssue((prev) => {
        const next = { ...prev };
        delete next[id];
        return next;
      });
      const result = await cleanup.restore(id);
      const detail = result.skipped.map((s) => `${shortPath(s.path)}: ${s.reason}`).join("\n");
      const warnings = result.warnings ?? [];
      if (result.skipped.length) setRestoreIssue((prev) => ({ ...prev, [id]: detail }));
      setNote(
        result.skipped.length
          ? { tone: "warn", text: `Partly restored: ${bytes(result.restored_bytes)} (${items(result.restored_items)}), ${result.skipped.length} stayed in the ${TRASH}.`, detail: warnings.join("\n") }
          : warnings.length
            ? { tone: "warn", text: `Restored: ${bytes(result.restored_bytes)} (${items(result.restored_items)}), with a warning.`, detail: warnings.join("\n") }
            : { tone: "ok", text: `Restored: ${bytes(result.restored_bytes)} (${items(result.restored_items)}).` },
      );
      await loadHistory();
      void refresh();
    });

  // A plain function, not a component: a component declared here is a new type on every
  // render, which remounts its rows and drops focus and open details on each selection.
  const group = (title: string, xs: Finding[]) =>
    xs.length === 0 ? null : (
      <section key={title} className="ck-group">
        <GroupHead title={title} xs={xs} selected={selected} onToggle={(on) => toggleGroup(xs, on)} disabled={busy} />
        {xs.map((f) => (
          <div key={f.id} className="ck-row">
            <span className="ck-check">
              <Checkbox
                checked={selected.has(f.id)}
                onChange={() => toggle(f.id)}
                disabled={busy}
                label={<span className="ov-vh">Select {f.name}</span>}
              />
            </span>
            <details className="ck-detail">
              <summary>
                <ChevronRight size={14} strokeWidth={1.75} aria-hidden />
                <span>
                  <span className="ck-title">
                    <strong>{f.name}</strong>
                    <Badge tone={f.risk === "safe" ? "ok" : "warn"}>{f.risk === "safe" ? "Safe" : "Review"}</Badge>
                  </span>
                  <span className="muted ck-reason">{f.reason}</span>
                  <span className="muted ck-reason">{facts(f)}</span>
                </span>
              </summary>
              <p className="muted ck-path">{shortPath(f.path)}</p>
              <p className="muted ck-path">{f.category}</p>
            </details>
            <span className="ck-num ck-size" title={f.upper_bound ? ESTIMATE : undefined}>
              {f.partial ? "≥ " : f.upper_bound ? "up to " : ""}
              {bytes(f.bytes)}
            </span>
          </div>
        ))}
      </section>
    );

  const historyPane = (
    <div className="ck-card">
      <h2>Moves and restore</h2>
      {historyWarnings.length > 0 && (
        <Notice tone="warn">
          {historyWarnings.map((w, n) => (
            <span key={n} className="ck-lines">{w}</span>
          ))}
        </Notice>
      )}
      {history.length === 0 && <p className="muted">Nothing moved yet. Confirmed moves appear here.</p>}
      {history.map((a) => {
        const left = a.items.filter((i) => !i.restored);
        // An item whose state is unknown (interrupted) is never offered for Undo.
        const canRestore = left.some((i) => i.trash_path && !i.interrupted);
        const issue = restoreIssue[a.id];
        const warning = runWarning(a);
        return (
          <div key={a.id} className="ck-history">
            <div className="ck-history-main">
              <strong>{new Date(a.at * 1000).toLocaleString()}</strong>
              <span className="muted">
                {a.items.length === 0
                  ? "Nothing was moved"
                  : `${items(a.items.length)} moved to the ${TRASH} · ${bounds[a.id] === "lower" ? "≥ " : bounds[a.id] === "upper" ? "up to " : ""}${bytes(a.bytes)}`}
                {left.length < a.items.length ? ` · ${a.items.length - left.length} restored` : ""}
              </span>
              {warning && (
                <Notice tone="warn">
                  <span className="ck-lines">{warning}</span>
                </Notice>
              )}
              {(a.free_space ?? []).length > 0 && (
                <p className="muted ck-path">
                  {(a.free_space ?? []).map(freeSpaceLine).join(" ")} {FREE_SPACE_CAVEAT}
                </p>
              )}
              {a.items.length > 0 && (
                <details className="ck-detail ck-paths">
                  <summary>
                    <ChevronRight size={14} strokeWidth={1.75} aria-hidden />
                    <span>Paths</span>
                  </summary>
                  {a.items.map((i) => (
                    <p key={i.path} className="muted ck-path">
                      {shortPath(i.path)} · {i.restored ? "Restored" : i.interrupted ? "State unknown" : i.trash_path ? `In the ${TRASH}` : "Not restorable"}
                    </p>
                  ))}
                </details>
              )}
              {a.items.filter((i) => !i.restored && i.interrupted).map((i) => (
                <p key={i.path} className="muted ck-path">
                  {shortPath(i.path)}: {i.interrupted}
                </p>
              ))}
              {issue && (
                <p className="ck-issue" role="alert">
                  Some items stayed in the {TRASH}.
                  <span className="ck-lines">{issue}</span>
                </p>
              )}
            </div>
            {a.items.length > 0 && (
              <Button size="sm" variant="secondary" onClick={() => restore(a.id)} disabled={busy || !canRestore}>
                {issue ? "Retry restore" : left.length === 0 ? "Restored" : "Restore"}
              </Button>
            )}
          </div>
        );
      })}
    </div>
  );

  const heading = !report
    ? "Looking for what can go…"
    : eligible.length === 0
      ? "Nothing to clean up"
      : `${total(eligible)} can go`;

  return (
    <div className="view ck-cleanup">
      <div className="ck-head">
        <div>
          <h2>{heading}</h2>
          {report && (
            <p className="muted">
              {eligible.length > 0 ? `${items(eligible.length)} · ` : ""}Updated {ago(report.scanned_at)}
              {refreshing ? " · Refreshing…" : ""}
            </p>
          )}
        </div>
        <Button size="sm" variant="secondary" onClick={refresh} disabled={busy || refreshing}>
          {refreshing ? "Refreshing…" : "Rescan"}
        </Button>
      </div>

      <SegmentedControl
        label="Cleanup workspace"
        value={pane}
        options={[
          { value: "findings", label: "Findings" },
          { value: "history", label: `History · ${history.length}` },
        ]}
        onChange={(v) => setPane(v as Pane)}
      />

      {note && (
        <Notice
          tone={note.tone === "error" ? "bad" : note.tone}
          action={pane === "findings" && history.length > 0 ? (
            <Button size="sm" variant="secondary" onClick={() => setPane("history")}>
              History and restore
            </Button>
          ) : undefined}
        >
          {note.text}
          {note.detail && <span className="ck-lines">{note.detail}</span>}
        </Notice>
      )}
      {error && (
        <Notice tone="bad">
          <span className="ck-lines">{error}</span>
        </Notice>
      )}
      {historyError && (
        <Notice tone="bad">
          <span className="ck-lines">{historyError}</span>
        </Notice>
      )}

      <div className="ck-scroll" tabIndex={0} aria-label="Cleanup content">
        {pane === "history" ? (
          historyPane
        ) : (
          <>
            {report && report.findings.some((f) => f.partial) && (
              <Notice tone="warn">Some sizes are partial; ≥ marks a lower bound.</Notice>
            )}
            <ChromeSnapshotsLine info={report?.chrome_snapshots} />
            {byCategory.length > 0 && (
              <div className="ck-card" aria-label="Where the space is">
                <strong>Where the space is</strong>
                {byCategory.map((g) => (
                  <div key={g.category} className="ck-held-row">
                    <span>
                      <strong>{g.category}</strong>
                      <span className="muted ck-reason">
                        {items(g.xs.length)}
                        {g.safety ? ` · ${g.safety}` : ""}
                      </span>
                    </span>
                    <span className="ck-num ck-size">{bytes(g.bytes)}</span>
                  </div>
                ))}
              </div>
            )}
            {report && eligible.length === 0 && <p className="muted ck-empty">Nothing to clean up right now. Rescan after apps close.</p>}
            {eligible.length > 0 && (
              <p className="muted">
                <Button size="sm" variant="secondary" onClick={freeUp} disabled={busy || freeable.length === 0}>
                  Free this up
                </Button>{" "}
                Selects the safe items and Chrome's signing copies for review. Nothing moves until you confirm.
              </p>
            )}
            {eligible.length > 0 && (
              <div className="ck-card">
                {group("Safe to regenerate", safe)}
                {group("Review", review)}
              </div>
            )}
            {held.length > 0 && (
              <details className="ck-held">
                <summary>
                  <ChevronRight size={14} strokeWidth={1.75} aria-hidden />
                  <span>
                    <strong>{held.length} not offered</strong>
                    <span className="muted"> · informational, running or unverified</span>
                  </span>
                </summary>
                {held.map((f) => (
                  <div key={f.id} className="ck-held-row">
                    <span>
                      <strong>{f.name}</strong>
                      <span className="muted ck-reason">{f.reason}</span>
                      {f.kind && <span className="muted ck-reason">{facts(f)}</span>}
                    </span>
                    <span className="ck-num ck-size">{bytes(f.bytes)}</span>
                  </div>
                ))}
              </details>
            )}
            <details className="ck-about">
              <summary>
                <ChevronRight size={14} strokeWidth={1.75} aria-hidden />
                <span>About these numbers</span>
              </summary>
              <p className="muted">
                Nothing is deleted. Items go to the {TRASH}, where you can put them back or empty it yourself. Sizes are what the
                scan measured; ≥ marks a lower bound when part of a folder could not be read. Items whose owner could not be
                verified, or whose app is running, are never offered. RustRight build targets are judged by RustRight's own state:
                a target a build holds or pins is never offered, and the current target is offered only after 14 days unused.
                Virtual machines are listed but not moved here; remove them with tart delete.
              </p>
            </details>
          </>
        )}
      </div>

      {pane === "findings" && eligible.length > 0 && (
        <div className="ck-selection">
          <div>
            <strong className="ck-num">
              {chosen.length} selected · {total(chosen)}
            </strong>
            <span className="muted"> to move to the {TRASH}</span>
          </div>
          <Button onClick={() => setPending(chosen)} disabled={busy || chosen.length === 0}>
            Move {chosen.length} to {TRASH}
          </Button>
        </div>
      )}

      {pending && (
        <ConfirmDialog
          title={`Move ${items(pending.length)} to the ${TRASH}?`}
          description={`${items(pending.length)}, ${total(pending)}, will move to the ${TRASH}${pending.some((f) => f.upper_bound || f.partial) ? ` (size is an estimate from allocated size, an upper bound: hard links and clones can make the real gain smaller)` : ""}. Nothing is deleted, and nothing is freed until the ${TRASH} is emptied: you can put them back from History here or from the ${TRASH}.`}
          confirmLabel={`Move to ${TRASH}`}
          onConfirm={() => move(pending)}
          onCancel={() => setPending(null)}
        />
      )}
    </div>
  );
}
