import { useEffect, useMemo, useRef, useState, type KeyboardEvent as ReactKeyboardEvent, type MouseEvent as ReactMouseEvent } from "react";
import { appName, systemHost } from "../host.js";
import { Badge, Button, ConfirmDialog, EmptyState, IconButton, Notice, Select, SegmentedControl, useContextMenu, Input } from "@rightkit/react-shell/react";
import { Activity, ChevronDown, ChevronRight, Copy, File, Folder as FolderIcon, FolderInput, FolderOpen, HardDrive, Info, MoreHorizontal, Network, RefreshCw, ShieldCheck, Terminal, Trash2, Undo2, Usb, X } from "lucide-react";
import {
  api,
  ago,
  bytes,
  isStale,
  isWindows,
  reclaimableNote,
  signedBytes,
  tone,
  type CleanupFinding,
  type CleanupReport,
  type FileIdentity,
  type Folder,
  type Growth,
  type HealthReport,
  type Inspect,
  type ChangedRange,
  type Row,
  type Volume,
} from "../api.js";
import { KINDS, kindOf, squarify } from "../chart.js";
import { readKept, writeKept } from "../kept.js";
import { ESTIMATE, trashFreesNothing, upTo } from "./storageEstimate.js";

// Windows has the Recycle Bin and File Explorer; the Mac has the Trash and Finder.
const TRASH = isWindows ? "Recycle Bin" : "Trash";
const FILE_MANAGER = isWindows ? "File Explorer" : "Finder";
import { ChromeSnapshotsLine } from "./ChromeSnapshots.js";
import { DriveHealthLine, DriveHealthPanel } from "./DriveHealth.js";
import { Duplicates } from "./Duplicates.js";
import { clearVisibleInterval, visibleInterval } from "../visible.js";

const shortPath = (path: string) => path.replace(/^\/Users\/[^/]+/, "~");
const VISIBLE_GROUPS = 6;
const VISIBLE_NOTES = 3;

type Pane = "findings" | "folders" | "changes" | "duplicates";

interface Group {
  key: string;
  label: string;
  items: CleanupFinding[];
  bytes: number;
  partial: boolean;
  risk: "safe" | "review";
  reason: string;
  discovered: boolean;
}

/** Findings survive leaving and returning to Storage; they are only rescanned on demand. */
let cachedReport: CleanupReport | null = null;

/** Without a live refresh, a saved scan younger than this is shown as it is on open. */
const SNAPSHOT_SECS = 6 * 60 * 60;
const isOld = (secs: number) => Date.now() / 1000 - secs > SNAPSHOT_SECS;

const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

const plural = (n: number, word: string) =>
  `${n} ${n === 1 ? word : /(ch|sh|s|x|z)$/.test(word) ? `${word}es` : `${word}s`}`;

/** Menu and keyboard actions apply to real items, never the "smaller files" total. */
const actionable = (r: Row) => !r.summary;

/** The volume a saved scan belongs to: its external drive, else the startup disk. */
const mountFor = (root: string, list: Volume[]) =>
  list.find((v) => !v.internal && v.mount_point === root)?.mount_point ?? "/";

// True for a path on a network volume: such a volume is never scanned without the person asking.
const onNetwork = (path: string, list: Volume[]) =>
  list.some((v) => {
    if (v.kind !== "network") return false;
    const root = v.mount_point.replace(/[\\/]+$/, "");
    return path === root || path.startsWith(`${root}/`) || path.startsWith(`${root}\\`);
  });

function groupFindings(list: CleanupFinding[]): Group[] {
  const map = new Map<string, Group>();
  for (const f of list) {
    let g = map.get(f.rule_id);
    if (!g) {
      const label = f.rule_name || f.name;
      g = { key: f.rule_id, label, items: [], bytes: 0, partial: false, risk: "safe", reason: f.reason, discovered: f.name.startsWith(`${label} · `) };
      map.set(f.rule_id, g);
    }
    g.items.push(f);
    g.bytes += f.bytes;
    g.partial ||= f.partial;
    if (f.risk !== "safe") g.risk = "review";
  }
  return [...map.values()].sort((a, b) => b.bytes - a.bytes);
}

const groupCount = (g: Group) =>
  g.items.length === 1 ? "" : g.discovered ? plural(g.items.length, "project") : plural(g.items.length, "item");

function noteIcon(f: CleanupFinding) {
  const text = `${f.action} ${f.reason}`.toLowerCase();
  if (text.includes("xcrun") || text.includes("terminal") || text.includes("command")) return Terminal;
  if (text.includes("in use") || text.includes("running")) return Activity;
  return Info;
}

/** The Kind filter. Folders and app bundles are told apart by the row, the rest by extension. */
type KindFilter = "all" | "folders" | "apps" | "documents" | "images" | "video" | "audio" | "archives" | "code" | "other";

const KIND_FILTERS: { value: KindFilter; label: string }[] = [
  { value: "all", label: "All kinds" },
  { value: "folders", label: "Folders" },
  { value: "documents", label: "Documents" },
  { value: "images", label: "Images" },
  { value: "video", label: "Video" },
  { value: "audio", label: "Audio" },
  { value: "archives", label: "Archives" },
  { value: "code", label: "Code" },
  { value: "apps", label: "Apps" },
  { value: "other", label: "Other" },
];

/** Extensions per file kind, checked in order. A file in none of them is Other. */
const FILE_KINDS: [KindFilter, string[]][] = [
  ["apps", ["dmg", "pkg", "exe", "msi"]],
  ["documents", ["pdf", "doc", "docx", "pages", "rtf", "txt", "md", "odt", "xls", "xlsx", "numbers", "csv", "ppt", "pptx", "key", "epub"]],
  ["images", ["jpg", "jpeg", "png", "gif", "heic", "heif", "tif", "tiff", "webp", "svg", "bmp", "raw", "cr2", "dng", "psd"]],
  ["video", ["mov", "mp4", "m4v", "mkv", "avi", "webm", "mpg", "mpeg"]],
  ["audio", ["mp3", "m4a", "wav", "flac", "aac", "aiff", "ogg"]],
  ["archives", ["zip", "tar", "gz", "tgz", "bz2", "xz", "7z", "rar", "iso"]],
  ["code", ["js", "jsx", "ts", "tsx", "rs", "swift", "py", "go", "java", "kt", "c", "h", "cpp", "hpp", "cs", "rb", "php", "sh", "json", "yaml", "yml", "toml", "html", "css", "scss", "xml", "sql"]],
];

/** Size filter: an item must be larger than the threshold (0 means any size). Decimal units, as the page shows them. */
const SIZE_FILTERS: { value: number; label: string }[] = [
  { value: 0, label: "Any size" },
  { value: 100_000_000, label: "Over 100 MB" },
  { value: 1_000_000_000, label: "Over 1 GB" },
  { value: 10_000_000_000, label: "Over 10 GB" },
];

/** Changed filter: when an item (a folder: anything inside it) was last modified. */
type ChangedFilter = "any" | "week" | "month" | "year" | "older";

const CHANGED_FILTERS: { value: ChangedFilter; label: string }[] = [
  { value: "any", label: "Any time" },
  { value: "week", label: "Last 7 days" },
  { value: "month", label: "Last 30 days" },
  { value: "year", label: "Last year" },
  { value: "older", label: "Older than a year" },
];

const DAY_SECONDS = 86_400;

/** The modified-time range a Changed choice means at `now` (Unix seconds); null for any time. */
function changedRange(filter: ChangedFilter, now: number): ChangedRange | null {
  switch (filter) {
    case "week": return { since: now - 7 * DAY_SECONDS };
    case "month": return { since: now - 30 * DAY_SECONDS };
    case "year": return { since: now - 365 * DAY_SECONDS };
    case "older": return { before: now - 365 * DAY_SECONDS };
    default: return null;
  }
}

/** The filter kind an item belongs to. Only real items reach here (not the "smaller files" total). */
function kindFilterOf(r: Row): KindFilter {
  if (r.is_dir) return r.name.toLowerCase().endsWith(".app") ? "apps" : "folders";
  const dot = r.name.lastIndexOf(".");
  const ext = dot > 0 ? r.name.slice(dot + 1).toLowerCase() : "";
  for (const [kind, exts] of FILE_KINDS) {
    if (exts.includes(ext)) return kind;
  }
  return "other";
}

/** A time as the local date and time. */
const localTime = (secs: number) => new Date(secs * 1000).toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });

/** Most matches one search returns (the engine clamps to 1000). */
const SEARCH_LIMIT = 100;
const KEPT_SELECTED = "storage.selected";

/** `autoScan` is Settings > General > Background > "Scan when Storage or Cleanup opens" (on by default). */
export function Storage({ autoScan = true }: { autoScan?: boolean }) {
  const [volumes, setVolumes] = useState<Volume[]>([]);
  const [active, setActive] = useState<string>("/");
  const [folder, setFolder] = useState<Folder | null>(null);
  // The file access permission itself; null until the first answer.
  const [access, setAccess] = useState<string | null>(null);
  useEffect(() => {
    let live = true;
    api.fullDiskAccessStatus().then((value) => live && setAccess(value)).catch(() => live && setAccess("unknown"));
    return () => {
      live = false;
    };
  }, [folder?.scanned_at]);
  const [results, setResults] = useState<Row[] | null>(null);
  // Which source answered the search on screen: Windows Search or the saved scan (Windows note).
  const [searchSource, setSearchSource] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [growth, setGrowth] = useState<Growth | null>(null);
  const [report, setReport] = useState<CleanupReport | null>(cachedReport);
  const [showAll, setShowAll] = useState(false);
  const [showNotes, setShowNotes] = useState(false);
  // One workspace at a time; Duplicates is the exact-content copy finder.
  const [pane, setPane] = useState<Pane>("findings");
  const [health, setHealth] = useState<HealthReport | null>(null);
  // The volume whose drive-health panel is open, by mount point.
  const [healthOpen, setHealthOpen] = useState<string | null>(null);
  const [openGroups, setOpenGroups] = useState<Set<string>>(new Set());
  const [pending, setPending] = useState<{ label: string; items: CleanupFinding[] } | null>(null);
  // One item waiting on a Trash confirmation, or on confirming a copy across drives.
  const [itemTrash, setItemTrash] = useState<{ row: Row; id: FileIdentity } | null>(null);
  const [itemMove, setItemMove] = useState<{ row: Row; id: FileIdentity; destination: string; target: string } | null>(null);
  const [undo, setUndo] = useState<{ id: string; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [scanning, setScanning] = useState(false);
  // True until the saved view has been asked for, so the page never shows an empty "Scanning…" first.
  const [opening, setOpening] = useState(true);
  const [findingsBusy, setFindingsBusy] = useState(false);
  // Latest scan request; an older one that comes back (cancelled) is ignored.
  const scanToken = useRef(0);
  const alive = useRef(true);
  const [error, setError] = useState<string | null>(null);
  // Image and video compression (the host app #202): the Settings switch, read again when a row is chosen, and the last result.
  const [compressOn, setCompressOn] = useState(false);
  const [compressNote, setCompressNote] = useState<string | null>(null);
  // The row the inspector shows (by path), and what the Rust side read about it.
  const [selected, setSelected] = useState<string | null>(null);
  // The row selected before the window was closed, with its folder; restored once, when that folder opens.
  const keptSelection = useRef<{ folder: string; path: string } | null>(readKept<{ folder: string; path: string }>(KEPT_SELECTED));
  const [inspect, setInspect] = useState<{ path: string; data: Inspect | null; error: string | null } | null>(null);
  const [kindFilter, setKindFilter] = useState<KindFilter>("all");
  // Bytes an item must exceed; 0 means any size.
  const [minSize, setMinSize] = useState(0);
  const [changedFilter, setChangedFilter] = useState<ChangedFilter>("any");
  // Search matches the date range left out for having no date.
  const [searchUndated, setSearchUndated] = useState(0);
  // Bumped when the engine reports a change while a search is on screen, so the search runs again.
  const [searchRefresh, setSearchRefresh] = useState(0);
  // The query as the engine-event listener sees it (that listener subscribes once).
  const queryNow = useRef(query);
  useEffect(() => {
    queryNow.current = query;
  }, [query]);
  // Only a scan that kept modified times can be filtered by date.
  const hasDates = folder?.has_dates === true;

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

  const refreshFindings = () => {
    setFindingsBusy(true);
    api
      .cleanupScan()
      .then((r) => {
        cachedReport = r;
        setReport(r);
      })
      .catch(() => {})
      .finally(() => setFindingsBusy(false));
  };

  // Findings show at once from the saved copy. A background rescan follows when
  // there is none, when it is old, or when a move has just changed the list.
  const loadFindings = (force = false) => {
    if (cachedReport) {
      setReport(cachedReport);
      if (force || (autoScan && isStale(cachedReport.scanned_at))) refreshFindings();
      return;
    }
    api
      .cleanupCached()
      .then((saved) => {
        if (saved && !cachedReport) {
          cachedReport = saved;
          setReport(saved);
        }
        if (force || (autoScan && (!saved || isStale(saved.scanned_at)))) refreshFindings();
      })
      .catch(() => { if (autoScan || force) refreshFindings(); });
  };

  const isInternal = (mount: string, list: Volume[]) => {
    const volume = list.find((v) => v.mount_point === mount);
    return !volume || volume.internal;
  };

  // Show data that is already scanned: pick the matching drive card, then the
  // growth line (home only) and, once, the cleanup findings.
  const show = (f: Folder, list: Volume[]) => {
    const external = list.find((v) => !v.internal && v.mount_point === f.root);
    const mount = external ? external.mount_point : (list.find((v) => v.internal)?.mount_point ?? "/");
    setActive(mount);
    setFolder(f);
    if (external) {
      setGrowth(null);
    } else {
      api.growth().then(setGrowth).catch(() => setGrowth(null));
      loadFindings();
    }
  };

  // The startup disk is scanned from the home folder; other volumes from their root.
  // Starting a scan cancels the one running; there is never more than one.
  const scanVolume = async (mount: string, list: Volume[] = volumes) => {
    const token = ++scanToken.current;
    setActive(mount);
    setQuery("");
    setError(null);
    setScanning(true);
    try {
      const f = await api.scan(isInternal(mount, list) ? undefined : mount);
      if (token !== scanToken.current || !alive.current) return;
      show(f, list);
      setScanning(false);
    } catch (e) {
      if (token !== scanToken.current || !alive.current) return;
      setScanning(false);
      if (!String(e).includes("cancelled")) setError(String(e));
    }
  };

  // Choosing a network volume only shows it; its scan waits for "Scan anyway".
  const chooseVolume = (mount: string) => {
    if (volumes.find((v) => v.mount_point === mount)?.kind !== "network") {
      scanVolume(mount);
      return;
    }
    setActive(mount);
    setFolder(null);
    setResults(null);
    setSelected(null);
    setQuery("");
    setError(null);
  };

  // A scan started before this view was reopened keeps running; wait for it.
  const follow = async (list: Volume[]) => {
    const token = ++scanToken.current;
    setScanning(true);
    for (;;) {
      await sleep(1500);
      if (token !== scanToken.current || !alive.current) return;
      const status = await api.scanStatus().catch(() => null);
      if (status && !status.running) break;
    }
    const f = await api.lastScan().catch(() => null);
    if (token !== scanToken.current || !alive.current) return;
    if (f) show(f, list);
    setScanning(false);
  };

  const open = (path: string) => run(async () => setFolder(await api.children(path)));

  // Single-item actions. The identity is read when the person asks for an
  // action; the Rust side checks it again right before anything moves.
  const showInFinder = (path: string) => api.finderOpen(path).catch((e) => setError(String(e)));
  const copyText = (text: string) => navigator.clipboard.writeText(text).catch((e) => setError(`Could not copy: ${String(e)}`));
  // Drops a moved or trashed item from what is on screen. The saved scan index
  // is not rewritten here, so a rescan (or re-drilling) shows it until then.
  const dropRow = (path: string) => {
    if (selected === path) setSelected(null);
    if (results) setResults(results.filter((r) => r.path !== path));
    else setFolder((f) => (f ? { ...f, rows: f.rows.filter((r) => r.path !== path) } : f));
  };
  useEffect(() => {
    api.mediaCompressStatus().then((s) => setCompressOn(s.enabled)).catch(() => setCompressOn(false));
  }, [selected]);
  // Writes a separate, smaller copy in a "the host app Compressed" folder beside the file; the original is never changed.
  const compressItem = (row: Row) =>
    run(async () => {
      setCompressNote(null);
      const done = await api.mediaCompress(row.path);
      setCompressNote(`Saved ${bytes(done.saved_bytes)}: ${shortPath(done.output)} is ${bytes(done.output_bytes)}, the original is unchanged.`);
    });
  const askTrash = (row: Row) => run(async () => setItemTrash({ row, id: await api.fileIdentity(row.path) }));
  const confirmItemTrash = () => {
    const pending = itemTrash;
    if (!pending) return;
    setItemTrash(null);
    run(async () => {
      try {
        await api.fileTrash(pending.row.path, pending.id);
        dropRow(pending.row.path);
      } finally {
        api.volumes().then(setVolumes).catch(() => {});
      }
    });
  };
  const finishMove = async (row: Row, id: FileIdentity, destination: string, copyAcrossVolumes: boolean) => {
    try {
      await api.fileMove(row.path, id, destination, copyAcrossVolumes);
      dropRow(row.path);
    } finally {
      api.volumes().then(setVolumes).catch(() => {});
    }
  };
  // Pick a destination, then move at once within one drive, or confirm a copy across drives first.
  const moveItem = (row: Row) =>
    run(async () => {
      const id = await api.fileIdentity(row.path);
      const destination = await api.fileChooseFolder();
      if (!destination) return;
      const plan = await api.fileMovePlan(row.path, id, destination);
      if (plan.same_volume) await finishMove(row, id, destination, false);
      else setItemMove({ row, id, destination, target: plan.target });
    });
  const confirmItemMove = () => {
    const pending = itemMove;
    if (!pending) return;
    setItemMove(null);
    run(() => finishMove(pending.row, pending.id, pending.destination, true));
  };
  // Double-click and Return open in Finder; ⌘C copies the path; ⌘⌫ asks to trash.
  const onRowKey = (e: ReactKeyboardEvent, row: Row) => {
    if (!actionable(row)) return;
    const command = isWindows ? e.ctrlKey && !e.altKey && !e.metaKey : e.metaKey && !e.altKey && !e.ctrlKey;
    if (command && e.key.toLowerCase() === "c") {
      e.preventDefault();
      copyText(row.path);
    } else if (e.key === "Enter") {
      e.preventDefault();
      showInFinder(row.path);
    } else if (command && e.key === "Backspace") {
      e.preventDefault();
      askTrash(row);
    }
  };
  // Declared after the handlers above: the builder runs during render.
  const itemMenu = useContextMenu<Row>(
    (row) => [
      { id: "finder", label: `${row.is_dir ? "Open" : "Reveal"} in ${FILE_MANAGER}`, icon: <FolderOpen size={13} />, run: () => showInFinder(row.path) },
      { id: "copy-path", label: "Copy path", icon: <Copy size={13} />, shortcut: "mod+c", separatorBefore: true, run: () => copyText(row.path) },
      { id: "copy-name", label: "Copy name", icon: <Copy size={13} />, run: () => copyText(row.name) },
      { id: "move", label: "Move to…", icon: <FolderInput size={13} />, separatorBefore: true, run: () => moveItem(row) },
      { id: "trash", label: `Move to ${TRASH}`, icon: <Trash2 size={13} />, shortcut: "mod+Backspace", danger: true, separatorBefore: true, run: () => askTrash(row) },
    ],
    "Item actions",
    // Fixed width: "Move to Recycle Bin" beside the Windows keycap "Ctrl+Backspace" needs more than the default 200.
    { width: isWindows ? 280 : 200 },
  );

  useEffect(() => {
    alive.current = true;
    (async () => {
      const list = await api.volumes().catch(() => [] as Volume[]);
      if (!alive.current) return;
      setVolumes(list);
      const status = await api.scanStatus().catch(() => null);
      if (!alive.current) return;
      if (status?.running) {
        const root = status.running_root;
        const external = list.find((v) => !v.internal && v.mount_point === root);
        setActive(external ? external.mount_point : (list.find((v) => v.internal)?.mount_point ?? "/"));
        setOpening(false);
        follow(list);
        return;
      }
      // The saved view opens at once. A rescan runs behind it only when the
      // saved one is old; with no saved view at all, the first scan starts now.
      // Without a live refresh a walk of the whole folder is long and nothing
      // keeps the view current, so a recent saved scan waits for Rescan.
      const saved = await api.lastScan().catch(() => null);
      if (!alive.current) return;
      setOpening(false);
      if (saved) {
        show(saved, list);
        const old = status?.live_refresh === false ? isOld(saved.scanned_at) : isStale(saved.scanned_at);
        // A saved network scan is never refreshed behind the page; "Scan anyway" refreshes it.
        if (old && autoScan && !onNetwork(saved.root, list)) scanVolume(mountFor(saved.root, list), list);
      } else if (autoScan) {
        scanVolume("/", list);
      }
    })();
    return () => {
      alive.current = false;
    };
  }, []);

  // The open folder, read by the event listener below without re-subscribing on every change.
  const openFolder = useRef<Folder | null>(null);
  useEffect(() => {
    openFolder.current = folder;
  }, [folder]);

  // The engine's live refresh (`storage-updated`, engine/src/watch.rs) names the folders whose
  // numbers or rows changed. The open folder is read again when one of them is it or lies below
  // it, or when the index says it is stale. Bursts are coalesced into one read a second after the
  // last event. A failed read keeps the rows on screen. The same event (the file-name index reports
  // its changes through it too) asks a search on screen to run again, once the burst is over.
  useEffect(() => {
    const within = (path: string, dir: string) => {
      const base = dir.replace(/[\\/]+$/, "");
      return path === dir || path === base || path.startsWith(`${base}/`) || path.startsWith(`${base}\\`);
    };
    let timer: ReturnType<typeof setTimeout> | null = null;
    let searchTimer: ReturnType<typeof setTimeout> | null = null;
    let disposed = false;
    let unlisten: (() => void) | null = null;
    const reread = () => {
      timer = null;
      const current = openFolder.current;
      if (!current) return;
      api
        .children(current.path)
        .then((f) => {
          if (!disposed && openFolder.current?.path === current.path) setFolder(f);
        })
        .catch(() => {});
    };
    try {
      void systemHost().subscribe("storage-updated", (event) => {
        if (queryNow.current.trim().length >= 2) {
          if (searchTimer != null) clearTimeout(searchTimer);
          searchTimer = setTimeout(() => setSearchRefresh((n) => n + 1), 1000);
        }
        const current = openFolder.current;
        if (!current) return;
        const { folders, stale } = event.payload;
        if (!stale && !folders.some((p) => within(p, current.path))) return;
        if (timer != null) clearTimeout(timer);
        timer = setTimeout(reread, 1000);
      })
        .then((stop) => {
          if (disposed) stop();
          else unlisten = stop;
        })
        .catch(() => {});
    } catch {
      // Outside the hub (a test page) there are no engine events; the page keeps what it shows.
    }
    return () => {
      disposed = true;
      if (timer != null) clearTimeout(timer);
      if (searchTimer != null) clearTimeout(searchTimer);
      unlisten?.();
    };
  }, []);

  useEffect(() => {
    if (!folder || query.trim().length < 2) {
      setResults(null);
      setSearchSource(null);
      setSearchUndated(0);
      return;
    }
    const handle = setTimeout(() => {
      // The size bound and the result limit both go to the backend, which applies the bound before
      // the limit (scanner::search) so the limit fills with matches.
      const range = hasDates ? changedRange(changedFilter, Math.floor(Date.now() / 1000)) : null;
      api
        .searchChanged(query.trim(), minSize > 0 ? minSize : undefined, range ?? undefined, SEARCH_LIMIT)
        .then((r) => {
          setResults(r.rows);
          setSearchSource(r.source ?? null);
          setSearchUndated(r.undated);
        })
        .catch((e) => setError(String(e)));
    }, 200);
    return () => clearTimeout(handle);
  }, [query, folder, minSize, changedFilter, hasDates, searchRefresh]);

  // Selection belongs to one folder listing: moving to another folder or rescanning clears it.
  useEffect(() => {
    if (keptSelection.current) {
      // While the kept selection is pending it must not be overwritten; choosing another row drops it.
      if (!selected) return;
      keptSelection.current = null;
    }
    writeKept(KEPT_SELECTED, selected && folder ? { folder: folder.path, path: selected } : null);
  }, [selected, folder?.path]);
  // The kept row's folder is opened once (the view loads the drive root first); the row is restored when it is open.
  const keptOpened = useRef(false);
  useEffect(() => {
    const kept = keptSelection.current;
    if (kept && folder) {
      if (folder.path === kept.folder) {
        keptSelection.current = null;
        setSelected(kept.path);
        return;
      }
      const base = folder.path;
      const below = kept.folder.startsWith(base) && (base.endsWith("/") || base.endsWith("\\") || /^[\\/]/.test(kept.folder.slice(base.length)));
      if (below && !keptOpened.current) {
        keptOpened.current = true;
        api.children(kept.folder).then(
          (f) => {
            if (alive.current && keptSelection.current === kept) setFolder(f);
          },
          () => {
            if (keptSelection.current === kept) keptSelection.current = null;
          },
        );
        setSelected(null);
        return;
      }
      keptSelection.current = null;
    }
    setSelected(null);
  }, [folder?.path, folder?.scanned_at]);

  // The inspector's facts are read once per selected item; a late answer for an item no longer shown is dropped.
  useEffect(() => {
    if (!selected) {
      setInspect(null);
      return;
    }
    let live = true;
    setInspect(null);
    api
      .storageInspect(selected)
      .then((data) => live && setInspect({ path: selected, data, error: null }))
      .catch((e) => live && setInspect({ path: selected, data: null, error: String(e) }));
    return () => {
      live = false;
    };
  }, [selected]);

  // Drive health for every drive card. The hub samples smartctl itself at most
  // every ten minutes, so polling here only reads the saved view.
  useEffect(() => {
    const mounts = volumes.filter((v) => !v.disk_image && v.kind === "local").map((v) => v.mount_point);
    if (mounts.length === 0) return;
    const run = () => api.driveHealth(mounts).then(setHealth).catch(() => {});
    run();
    const handle = visibleInterval(run, 60_000);
    return () => clearVisibleInterval(handle);
  }, [volumes]);

  const crumbs = useMemo(() => {
    if (!folder) return [];
    const parts: { name: string; path: string }[] = [];
    let path = folder.path;
    while (path && path.length >= folder.root.length) {
      parts.unshift({ name: path === folder.root ? folder.root_label : path.split("/").pop() || path, path });
      if (path === folder.root) break;
      path = path.slice(0, path.lastIndexOf("/")) || "/";
    }
    return parts;
  }, [folder]);

  const eligible = useMemo(
    () => (report?.findings ?? []).filter((f) => f.eligible).sort((a, b) => b.bytes - a.bytes),
    [report],
  );
  const others = (report?.findings ?? []).filter((f) => !f.eligible && f.bytes > 0).sort((a, b) => b.bytes - a.bytes);
  // With nothing to clear, open on the folders instead of an empty list (once,
  // so a pane the user picked is never taken away).
  const pickedPane = useRef(false);
  useEffect(() => {
    if (!pickedPane.current && report && eligible.length === 0) setPane("folders");
  }, [report, eligible.length]);
  const safeItems = eligible.filter((f) => f.risk === "safe");
  const freeable = report ? report.safe_bytes + report.review_bytes : 0;
  const groups = useMemo(() => groupFindings(eligible), [eligible]);
  const shown = showAll ? groups : groups.slice(0, VISIBLE_GROUPS);
  const notes = showNotes ? others : others.slice(0, VISIBLE_NOTES);
  const toggleGroup = (key: string) =>
    setOpenGroups((prev) => {
      const next = new Set(prev);
      if (!next.delete(key)) next.add(key);
      return next;
    });
  const internalActive = volumes.find((v) => v.mount_point === active)?.internal ?? active === "/";

  const confirmMove = () =>
    run(async () => {
      const items = pending?.items ?? [];
      setPending(null);
      const result = await api.cleanupApply(items);
      if (result.activity_id) {
        setUndo({
          id: result.activity_id,
          text: `Moved to the ${TRASH}: ${upTo(result.moved_bytes)} on disk${result.skipped.length ? `; ${result.skipped.length} skipped` : ""}. ${trashFreesNothing(TRASH)}`,
        });
      } else if (result.skipped.length) {
        setError(result.skipped.map((s) => `${shortPath(s.path)}: ${s.reason}`).join("\n"));
      }
      api.volumes().then(setVolumes).catch(() => {});
      loadFindings(true);
    });

  const undoMove = () =>
    run(async () => {
      if (!undo) return;
      await api.cleanupRestore(undo.id);
      setUndo(null);
      loadFindings(true);
    });

  const installers = volumes.filter((v) => v.disk_image);
  const [ejecting, setEjecting] = useState<string | null>(null);
  const [ejectError, setEjectError] = useState<string | null>(null);
  const eject = async (v: Volume) => {
    setEjecting(v.mount_point);
    setEjectError(null);
    try {
      await api.eject(v.mount_point);
    } catch (e) {
      setEjectError(`${v.name}: ${String(e)}`);
    }
    setEjecting(null);
    api.volumes().then(setVolumes).catch(() => {});
  };

  const segments = (
    <SegmentedControl
      label="Storage view"
      value={pane}
      options={[
        { value: "findings", label: "Findings" },
        { value: "folders", label: "Folders" },
        { value: "changes", label: "Changes" },
        { value: "duplicates", label: "Duplicates" },
      ]}
      onChange={(value) => {
        pickedPane.current = true;
        setPane(value as Pane);
      }}
    />
  );

  if (pane === "duplicates") {
    return (
      <div className="view st">
        <div className="st-bar">{segments}</div>
        <div className="st-scroll" tabIndex={0} aria-label="Duplicates">
          <Duplicates />
        </div>
      </div>
    );
  }

  // Every row of the listing, then the rows the filters keep. Shares are of the whole listing.
  const allRows = results ?? folder?.rows ?? [];
  // A date choice made on an earlier scan does not apply once the scan has no dates.
  const changed: ChangedFilter = hasDates ? changedFilter : "any";
  const filtersOn = kindFilter !== "all" || minSize > 0 || changed !== "any";
  // Search results were cut to the date range by the backend; a folder listing is cut here.
  const listRange = results ? null : changedRange(changed, Math.floor(Date.now() / 1000));
  let listUndated = 0;
  const rows = filtersOn
    ? allRows.filter((r) => {
        if (r.summary || (kindFilter !== "all" && kindFilterOf(r) !== kindFilter) || r.bytes <= minSize) return false;
        if (!listRange) return true;
        const modified = r.modified ?? 0;
        if (modified === 0) {
          listUndated += 1;
          return false;
        }
        return (listRange.since === undefined || modified >= listRange.since) && (listRange.before === undefined || modified < listRange.before);
      })
    : allRows;
  const undated = changed === "any" ? 0 : results ? searchUndated : listUndated;
  const largest = Math.max(1, ...rows.map((r) => r.bytes));
  const total = Math.max(1, allRows.reduce((sum, r) => sum + r.bytes, 0));
  const selectedRow = selected ? (allRows.find((r) => r.path === selected) ?? null) : null;
  const selectedInfo = inspect && selectedRow && inspect.path === selectedRow.path ? inspect : null;

  // Keys on the list: arrows and Home/End move the selection through the real items.
  const onListKey = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(e.key)) return;
    const items = rows.filter(actionable);
    if (items.length === 0) return;
    e.preventDefault();
    const at = items.findIndex((r) => r.path === selected);
    const last = items.length - 1;
    const next =
      e.key === "Home" ? 0
      : e.key === "End" ? last
      : e.key === "ArrowDown" ? Math.min(at + 1, last)
      : Math.max(at - 1, 0);
    const target = items[next];
    setSelected(target.path);
    Array.from(e.currentTarget.querySelectorAll<HTMLElement>(".st-row")).find((el) => el.dataset.path === target.path)?.focus();
  };
  // Escape closes the inspector, unless a confirmation is open over it.
  const onViewKey = (e: ReactKeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape" && selected && !itemTrash && !itemMove && !pending) {
      e.preventDefault();
      setSelected(null);
    }
  };
  const clearFilters = () => {
    setKindFilter("all");
    setMinSize(0);
    setChangedFilter("any");
  };

  const volumeList = volumes.filter((v) => !v.disk_image);
  const localList = volumeList.filter((v) => v.kind === "local");
  const networkList = volumeList.filter((v) => v.kind === "network");
  const activeVolume = volumeList.find((v) => v.mount_point === active);
  const ActiveIcon = activeVolume?.kind === "network" ? Network : activeVolume && !activeVolume.internal ? Usb : HardDrive;
  const card = health?.drives.find((d) => d.mount === active);
  const healthShown = healthOpen === active;
  const toolAvailable = health?.tool_available ?? true;
  const scanText =
    folder && scanning
      ? `Updated ${ago(folder.scanned_at)} · Refreshing…`
      : scanning
        ? "Scanning…"
        : folder
          ? `Updated ${ago(folder.scanned_at)}${folder.from_snapshot ? " (saved)" : ""}`
          : "";
  const scopeText = internalActive ? "Home folder" : "Whole drive";
  const changes = growth?.available ? [...growth.grown, ...growth.shrunk] : [];
  const openFromChange = (path: string) => {
    setPane("folders");
    open(path);
  };
  const headline = !report ? "Looking for what is safe to clear…" : freeable > 0 ? `${upTo(freeable)} eligible to move` : "Nothing obvious to clear";

  return (
    <div className="view st" onKeyDown={onViewKey}>
      <section className="st-volume" aria-label="Volume">
        <div className="st-volume-line">
          <ActiveIcon size={15} strokeWidth={1.75} className="st-volume-icon" aria-hidden="true" />
          {volumeList.length > 0 ? (
            <Select
              className="st-select"
              label="Volume"
              value={active}
              disabled={busy}
              onChange={chooseVolume}
              options={[
                ...localList.map((v) => ({
                  value: v.mount_point,
                  label: v.internal ? `${v.name} · Startup` : v.name,
                })),
                // Select has no optgroup (ui.d.ts:85): a disabled heading row keeps network volumes apart.
                ...(networkList.length > 0 ? [{ value: "__network__", label: "Network", disabled: true }] : []),
                ...networkList.map((v) => ({
                  value: v.mount_point,
                  disabled: v.state !== "ok",
                  label: v.state !== "ok"
                    ? `${v.name} · Not responding`
                    : v.available_bytes !== null
                      ? `${v.name} · ${bytes(v.available_bytes)} free`
                      : v.name,
                })),
              ]}
            />
          ) : (
            <span className="strong">No volumes</span>
          )}
          {activeVolume?.kind === "network" && <span className="st-net-label">Network</span>}
          {activeVolume && (
            <span className="st-free">
              {activeVolume.state !== "ok" ? (
                <span className="strong">Not responding</span>
              ) : activeVolume.available_bytes !== null && activeVolume.total_bytes !== null ? (
                <>
                  <span className="strong">{bytes(activeVolume.available_bytes)} free</span>
                  <span className="muted">of {bytes(activeVolume.total_bytes)}</span>
                  {reclaimableNote(activeVolume) && <span className="muted">· {reclaimableNote(activeVolume)}</span>}
                </>
              ) : (
                <span className="muted">Capacity unknown</span>
              )}
            </span>
          )}
          <Button size="sm" onClick={() => scanVolume(active)} disabled={busy || scanning || activeVolume?.state === "not responding"}>
            <RefreshCw size={12} /> Rescan
          </Button>
        </div>
        {activeVolume && activeVolume.total_bytes !== null && activeVolume.available_bytes !== null && activeVolume.total_bytes > 0 && (
          <Bar fraction={1 - activeVolume.available_bytes / activeVolume.total_bytes} height={6} />
        )}
        {(card || health) && (
          <div className="st-volume-foot">
            <DriveHealthLine card={card} toolAvailable={toolAvailable} />
            {card && (
              <Button variant="ghost" size="sm" onClick={() => setHealthOpen(healthShown ? null : active)} aria-expanded={healthShown}>
                {healthShown ? "Hide drive health" : "Drive health"}
              </Button>
            )}
          </div>
        )}
      </section>

      <div className="st-meta">
        <span className="st-meta-text">{[scopeText, scanText].filter(Boolean).join(" · ")}</span>
        {installers.length > 0 && <span className="st-meta-text">Mounted installers</span>}
        {installers.map((v) => (
          <span className="st-chip" key={v.mount_point} title={v.mount_point}>
            <span className="st-chip-name">{v.name}</span>
            <span>{bytes(v.total_bytes ?? 0)}</span>
            <Button size="sm" variant="ghost" onClick={() => eject(v)} disabled={ejecting === v.mount_point}>
              Eject
            </Button>
          </span>
        ))}
        {ejectError && <span className="error">{ejectError}</span>}
      </div>

      <div className="st-bar">{segments}</div>

      {activeVolume?.kind === "network" && !scanning && !folder && (
        <Notice
          action={activeVolume.state === "ok" ? (
            <Button size="sm" onClick={() => scanVolume(active)} disabled={busy}>
              Scan anyway
            </Button>
          ) : undefined}
        >
          {activeVolume.state === "ok"
            ? "This is a network volume. Scanning it reads every folder over the network."
            : "Not responding. The share did not answer in time, so nothing has been read from it."}
        </Notice>
      )}
      {error && (
        <Notice tone="bad">
          <span className="ck-lines">{error}</span>
        </Notice>
      )}
      {compressNote && (
        <Notice
          action={(
            <Button size="sm" variant="ghost" onClick={() => setCompressNote(null)}>
              Dismiss
            </Button>
          )}
        >
          {compressNote}
        </Notice>
      )}
      {undo && (
        <Notice
          action={(
            <Button size="sm" variant="ghost" onClick={undoMove} disabled={busy}>
              <Undo2 size={12} /> Undo
            </Button>
          )}
        >
          <span title={ESTIMATE}>{undo.text}</span>
        </Notice>
      )}
      {folder?.needs_access && access !== null && access !== "granted" && !results && (
        <Notice
          tone="warn"
          action={(
            <Button size="sm" onClick={() => api.openFullDiskAccess()}>
              {isWindows ? "Open File system settings" : "Open Full Disk Access"}
            </Button>
          )}
        >
          {isWindows ? "Some folders couldn't be read. Allow file system access in Windows Settings to include them." : "Some folders couldn't be read. Grant Full Disk Access to include them."}
        </Notice>
      )}
      {folder?.incomplete && !results && !(folder.needs_access && access !== null && access !== "granted") && (
        <div className="st-sub">
          {folder.unread
            ? `${folder.unread.toLocaleString()} ${folder.unread === 1 ? "item" : "items"} couldn't be read`
            : "Some items couldn't be read"}{" "}
          (long paths, access denied, or the scan's entry limit)
        </div>
      )}
      {folder && !folder.needs_access && folder.limited && !results && (
        <div className="st-sub">This is a very large folder, so sizes may be a little low.</div>
      )}

      <div className="st-scroll" tabIndex={0} aria-label="Storage content">
        {healthShown && (
          <DriveHealthPanel card={card} alerts={health?.alerts ?? []} toolAvailable={toolAvailable} />
        )}

        {pane === "findings" &&
          (internalActive ? (
            <section className="st-card" aria-label="Findings">
              <div className="st-card-head">
                <div>
                  <h2 className="st-h" title={freeable > 0 ? ESTIMATE : undefined}>
                    <ShieldCheck size={15} strokeWidth={1.75} aria-hidden="true" />
                    {headline}
                  </h2>
                  <p className="st-sub">
                    Home{report ? ` · scanned ${ago(report.scanned_at)}` : ""}
                    {report && findingsBusy ? " · Refreshing…" : ""}
                    {report && !findingsBusy && isStale(report.scanned_at) ? " · saved list, may be out of date" : ""}
                  </p>
                </div>
                {safeItems.length > 1 && (
                  <Button size="sm" variant="primary" onClick={() => setPending({ label: "Safe items", items: safeItems })} disabled={busy}>
                    Clear all safe ({upTo(report?.safe_bytes ?? 0)})
                  </Button>
                )}
              </div>
              <ChromeSnapshotsLine info={report?.chrome_snapshots} />
              {shown.map((g) => {
                const expanded = openGroups.has(g.key);
                const count = groupCount(g);
                return (
                  <div key={g.key} className="st-finding">
                    <Button variant="ghost" className="st-disclose" onClick={() => toggleGroup(g.key)} aria-expanded={expanded}>
                      {expanded ? <ChevronDown size={14} aria-hidden="true" /> : <ChevronRight size={14} aria-hidden="true" />}
                      <span className="st-finding-text">
                        <span className="st-finding-title">
                          <span className="strong">
                            {g.label}
                            {count && <span className="muted"> · {count}</span>}
                          </span>
                          <Badge tone={g.risk === "safe" ? "ok" : "warn"}>{g.risk === "safe" ? "Safe" : "Review"}</Badge>
                        </span>
                        <span className="st-reason">{g.reason}</span>
                      </span>
                    </Button>
                    <span className="st-measure">
                      <span className="st-size" title={ESTIMATE}>
                        {upTo(g.bytes, g.partial)}
                      </span>
                      <Button size="sm" onClick={() => setPending({ label: g.label, items: g.items })} disabled={busy}>
                        Move to {TRASH}
                      </Button>
                    </span>
                    {expanded && (
                      <div className="st-sub-items">
                        {g.items.map((f) => (
                          <div key={f.id} className="st-sub-item">
                            <span>
                              {g.items.length > 1 && <span>{f.name}</span>}
                              <span className="st-path" style={{ display: "block" }}>
                                {shortPath(f.path)}
                              </span>
                            </span>
                            <span className="st-size" title={ESTIMATE}>
                              {upTo(f.bytes, f.partial)}
                            </span>
                            <Button size="sm" variant="ghost" onClick={() => setPending({ label: f.name, items: [f] })} disabled={busy}>
                              Move to {TRASH}
                            </Button>
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                );
              })}
              {groups.length > VISIBLE_GROUPS && (
                <Button variant="ghost" size="sm" className="st-more-btn" onClick={() => setShowAll(!showAll)}>
                  {showAll ? "Show fewer" : `Show ${groups.length - VISIBLE_GROUPS} more`}
                </Button>
              )}
              {others.length > 0 && (
                <div className="st-notes">
                  <div className="st-notes-head">Notes</div>
                  {notes.map((f) => {
                    const Icon = noteIcon(f);
                    return (
                      <div key={f.id} className="st-note" title={f.path}>
                        <Icon size={12} strokeWidth={1.75} aria-hidden="true" />
                        <span className="st-note-text">
                          {f.rule_name || f.name} · {f.reason}
                        </span>
                        <span className="st-note-size">{bytes(f.bytes)}</span>
                      </div>
                    );
                  })}
                  {others.length > VISIBLE_NOTES && (
                    <Button variant="ghost" size="sm" className="st-more-btn" onClick={() => setShowNotes(!showNotes)}>
                      {showNotes ? "Show fewer notes" : `Show ${others.length - VISIBLE_NOTES} more notes`}
                    </Button>
                  )}
                </div>
              )}
            </section>
          ) : (
            <EmptyState
              icon={<ShieldCheck size={22} />}
              title="Findings cover your home folder"
              action={
                <Button size="sm" onClick={() => setPane("folders")}>
                  Browse this drive
                </Button>
              }
            />
          ))}

        {pane === "folders" && (
          <section className="st-card" aria-label="Folders">
            {results ? (
              <div className="st-sub">{plural(results.length, "match")}</div>
            ) : (
              <nav className="st-crumbs" aria-label="Folder path">
                {crumbs.map((c, i) => (
                  <span key={c.path}>
                    {i > 0 && <ChevronRight size={11} className="st-crumb-sep" aria-hidden="true" />}
                    <Button variant="ghost" size="sm" onClick={() => open(c.path)} disabled={busy} aria-current={i === crumbs.length - 1 ? "page" : undefined}>
                      {c.name}
                    </Button>
                  </span>
                ))}
              </nav>
            )}
            <Input
              className="search st-search"
              type="search"
              placeholder="Search files"
              aria-label="Search files"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
            />
            {isWindows && results !== null && searchSource === "windows_search" && (
              <p className="st-coverage">
                Names come from Windows Search. It finds only files in folders Windows indexes, so a folder it does not index is not found. Windows Indexing Options sets what it covers.
              </p>
            )}
            {isWindows && results !== null && searchSource === "saved_scan" && (
              <p className="st-coverage">
                {changedFilter !== "any"
                  ? "Names come from the last scan of this folder: the Changed filter needs modified dates, which Windows Search results do not carry."
                  : "Names come from the last scan of this folder, because Windows Search did not answer (it is stopped, or it does not index this folder). Windows Indexing Options sets what it covers."}
              </p>
            )}
            <div className="st-filters" role="group" aria-label="Filters">
              <Select className="st-select" label="Kind" value={kindFilter} onChange={setKindFilter} options={KIND_FILTERS} />
              <Select
                className="st-select"
                label="Size"
                value={String(minSize)}
                onChange={(v) => setMinSize(Number(v))}
                options={SIZE_FILTERS.map((s) => ({ value: String(s.value), label: s.label }))}
              />
              <Select
                className="st-select"
                label="Changed"
                value={changed}
                disabled={!hasDates}
                onChange={setChangedFilter}
                options={CHANGED_FILTERS}
              />
              {filtersOn && (
                <Button size="sm" variant="ghost" onClick={clearFilters}>
                  Clear filters
                </Button>
              )}
              {allRows.length > 0 && (
                <span className="st-count" aria-live="polite">
                  {rows.length.toLocaleString()} of {allRows.length.toLocaleString()} shown
                  {undated > 0 && ` · ${undated.toLocaleString()} ${undated === 1 ? "item has" : "items have"} no date`}
                </span>
              )}
            </div>
            {!folder && (opening ? <div className="muted">Loading…</div> : scanning && <div className="muted">Scanning…</div>)}
            {folder && allRows.length === 0 && !busy && !scanning && (
              <EmptyState icon={<FolderIcon size={22} />} title={results ? "No matches" : "This folder is empty"} />
            )}
            {allRows.length > 0 && rows.length === 0 && (
              <EmptyState
                icon={<FolderIcon size={22} />}
                title="No items match these filters"
                action={
                  <Button size="sm" onClick={clearFilters}>
                    Clear filters
                  </Button>
                }
              />
            )}
            {rows.length > 0 && (
              <div className="st-split">
                <div className="st-split-main">
                {!results && <Treemap rows={rows} select={setSelected} open={open} finder={showInFinder} menu={itemMenu.open} />}
                <div className="st-list" onKeyDown={onListKey}>
                  {rows.map((r) => {
                    const kind = KINDS[kindOf(r.path, r.is_dir)];
                    const pct = (r.bytes / total) * 100;
                    const isSelected = selected === r.path;
                    return (
                      <div
                        key={r.path}
                        data-path={r.path}
                        className={`st-row${actionable(r) ? " clickable" : ""}${isSelected ? " selected" : ""}`}
                        tabIndex={actionable(r) ? 0 : undefined}
                        aria-current={isSelected ? "true" : undefined}
                        onClick={() => {
                          if (actionable(r)) setSelected(r.path);
                        }}
                        onDoubleClick={() => {
                          // A folder opens in the list; a file (or a result) opens in Finder.
                          if (!actionable(r)) return;
                          if (r.is_dir && !results) open(r.path);
                          else showInFinder(r.path);
                        }}
                        onKeyDown={(e) => onRowKey(e, r)}
                        onContextMenu={(e) => (actionable(r) ? itemMenu.open(e, r) : e.preventDefault())}
                        title={`${r.path}\n${kind.label}`}
                      >
                        <span className="st-name">
                          <i className="st-dot" style={{ background: kind.color }} />
                          {r.is_dir ? <FolderIcon size={13} aria-hidden="true" /> : <File size={13} aria-hidden="true" />}
                          <span>{results ? shortPath(r.path) : r.name}</span>
                        </span>
                        <Bar fraction={r.bytes / largest} color={kind.color} />
                        <span className="st-pct">{pct >= 1 ? `${Math.round(pct)}%` : "<1%"}</span>
                        <span className="st-size">{bytes(r.bytes)}</span>
                        {actionable(r) ? (
                          <IconButton
                            label={`Actions for ${r.name}`}
                            aria-haspopup="menu"
                            onClick={(e) => {
                              e.stopPropagation();
                              itemMenu.open(e, r);
                            }}
                            onKeyDown={(e) => {
                              // Arrow keys pass through so the list can move the selection from here too.
                              if (e.key !== "ArrowDown" && e.key !== "ArrowUp") e.stopPropagation();
                            }}
                            onDoubleClick={(e) => e.stopPropagation()}
                          >
                            <MoreHorizontal size={15} aria-hidden="true" />
                          </IconButton>
                        ) : (
                          <span />
                        )}
                      </div>
                    );
                  })}
                </div>
                </div>
                {selectedRow && (
                  <ItemInspector
                    row={selectedRow}
                    info={selectedInfo}
                    parentBytes={results ? null : total}
                    network={onNetwork(selectedRow.path, volumes)}
                    busy={busy}
                    onClose={() => setSelected(null)}
                    onOpen={open}
                    onReveal={showInFinder}
                    onCopy={copyText}
                    onMove={moveItem}
                    onTrash={askTrash}
                    onCompress={compressOn && /\.(jpe?g|png|mp4|m4v|mov)$/i.test(selectedRow.path) ? compressItem : undefined}
                  />
                )}
              </div>
            )}
          </section>
        )}

        {pane === "changes" &&
          (!internalActive ? (
            <EmptyState icon={<Info size={22} />} title="Changes are tracked for your home folder" />
          ) : changes.length === 0 ? (
            <EmptyState icon={<Info size={22} />} title={growth?.available ? "No changes since the last scan" : "No comparison yet"}>
              {growth?.available ? null : (growth?.reason ?? "Changes appear after a second scan.")}
            </EmptyState>
          ) : (
            <section className="st-card" aria-label="Changes">
              <div className="st-card-head">
                <div>
                  <h2 className="st-h">Home changes</h2>
                  <p className="st-sub">
                    Since last scan
                    {growth?.since ? ` (${new Date(growth.since * 1000).toLocaleDateString(undefined, { month: "short", day: "numeric" })})` : ""}
                  </p>
                </div>
              </div>
              <div className="st-changes">
                {changes.map((c) => (
                  <Button variant="ghost" key={c.path} className="st-change" onClick={() => openFromChange(c.path)} disabled={busy} title={c.path}>
                    <span>{shortPath(c.path)}</span>
                    <span className={`st-delta ${c.bytes > 0 ? "up" : "down"}`}>{signedBytes(c.bytes)}</span>
                  </Button>
                ))}
              </div>
            </section>
          ))}
      </div>

      {itemMenu.element}

      {pending && (
        <ConfirmDialog
          title={pending.items.length === 1 ? `Move ${pending.items[0].name} to the ${TRASH}?` : `Move ${pending.items.length} items to the ${TRASH}?`}
          description={`${pending.items.length > 1 ? `${pending.label}: ` : ""}${plural(pending.items.length, "item")}, ${upTo(pending.items.reduce((s, f) => s + f.bytes, 0), pending.items.some((f) => f.partial))} on disk will move to the ${TRASH} (${ESTIMATE}). Nothing is deleted: you can put it back, or empty the ${TRASH} yourself. ${trashFreesNothing(TRASH)}`}
          confirmLabel={`Move to ${TRASH}`}
          onConfirm={confirmMove}
          onCancel={() => setPending(null)}
        />
      )}

      {itemTrash && (
        <ConfirmDialog
          title={`Move ${itemTrash.row.name} to the ${TRASH}?`}
          description={`${shortPath(itemTrash.row.path)}, ${upTo(itemTrash.row.bytes)} on disk will move to the ${TRASH} (${ESTIMATE}). Nothing is deleted: you can put it back, or empty the ${TRASH} yourself. ${trashFreesNothing(TRASH)}${onNetwork(itemTrash.row.path, volumes) ? " Network volumes usually delete at once instead of using the Trash." : ""}`}
          confirmLabel={`Move to ${TRASH}`}
          onConfirm={confirmItemTrash}
          onCancel={() => setItemTrash(null)}
        />
      )}

      {itemMove && (
        <ConfirmDialog
          title={`Copy ${itemMove.row.name} to another drive?`}
          description={`${shortPath(itemMove.destination)} is on another drive, so ${itemMove.row.name} is copied to ${shortPath(itemMove.target)}, and the original moves to the ${TRASH}. ${trashFreesNothing(TRASH)}`}
          confirmLabel={`Copy and move to ${TRASH}`}
          onConfirm={confirmItemMove}
          onCancel={() => setItemMove(null)}
        />
      )}
    </div>
  );
}

const W = 600;
const H = 96;

/**
 * The current folder as one level of tiles, coloured by kind. Click a tile to
 * select it for the inspector; double-click opens a folder or reveals a file;
 * right-click for the item menu.
 */
function Treemap({
  rows,
  select,
  open,
  finder,
  menu,
}: {
  rows: Row[];
  select: (path: string) => void;
  open: (path: string) => void;
  finder: (path: string) => void;
  menu: (event: ReactMouseEvent | MouseEvent, row: Row) => void;
}) {
  const items = rows.filter((r) => r.bytes > 0).slice(0, 40);
  if (items.length === 0) return null;
  const tiles = squarify(items.map((r) => r.bytes), W, H);
  return (
    <svg className="st-treemap" viewBox={`0 0 ${W} ${H}`} role="img" aria-label="Folder contents by size">
      {items.map((r, i) => {
        const t = tiles[i];
        const kind = KINDS[kindOf(r.path, r.is_dir)];
        const chars = Math.floor((t.w - 8) / 6.4);
        return (
          <g
            key={r.path}
            onClick={() => {
              if (actionable(r)) select(r.path);
            }}
            onDoubleClick={() => {
              // A folder opens in the list; a file opens in Finder.
              if (!actionable(r)) return;
              if (r.is_dir) open(r.path);
              else finder(r.path);
            }}
            onContextMenu={(e) => (actionable(r) ? menu(e, r) : e.preventDefault())}
            style={{ cursor: actionable(r) ? "pointer" : "default" }}
          >
            <title>{`${r.name}: ${bytes(r.bytes)}`}</title>
            <rect x={t.x + 0.5} y={t.y + 0.5} width={Math.max(t.w - 1, 0)} height={Math.max(t.h - 1, 0)} rx={2} fill={kind.color} opacity={0.88} />
            {t.w > 46 && t.h > 16 && chars > 3 && (
              <text x={t.x + 4} y={t.y + 13} fontSize={11} fill="#fff" style={{ pointerEvents: "none" }}>
                {r.name.length > chars ? r.name.slice(0, chars - 1) + "…" : r.name}
              </text>
            )}
          </g>
        );
      })}
    </svg>
  );
}

/** MIME type of an image the inspector previews; undefined for every other kind. HEIC, BMP and TIFF are not previewed. */
const PREVIEW_TYPES: Record<string, string> = { png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif", webp: "image/webp" };
const previewType = (path: string) => PREVIEW_TYPES[/\.([^./\\]+)$/.exec(path)?.[1]?.toLowerCase() ?? ""];

/**
 * The image itself, for a selected image file only. Nothing is read for other
 * kinds, and nothing on a network volume. The object URL is revoked when the
 * selection changes or the inspector closes.
 */
function ItemPreview({ row, network }: { row: Row; network: boolean }) {
  const type = row.is_dir || network ? undefined : previewType(row.path);
  const [loaded, setLoaded] = useState<{ path: string; url: string | null; reason: string | null } | null>(null);
  useEffect(() => {
    if (!type) return;
    let live = true;
    let url: string | null = null;
    api
      .storagePreview(row.path)
      .then((buffer) => {
        if (!live) return;
        url = URL.createObjectURL(new Blob([buffer], { type }));
        setLoaded({ path: row.path, url, reason: null });
      })
      .catch((e) => live && setLoaded({ path: row.path, url: null, reason: String(e) }));
    return () => {
      live = false;
      if (url) URL.revokeObjectURL(url);
      setLoaded(null);
    };
  }, [row.path, type]);
  if (row.is_dir) return null;
  if (network && previewType(row.path)) return <div className="st-sub">No preview on network drives</div>;
  if (!type) return null;
  const current = loaded?.path === row.path ? loaded : null;
  if (!current) return <div className="st-sub">Reading preview…</div>;
  if (current.url) return <img className="st-preview-image" src={current.url} alt={`Preview of ${row.name}`} />;
  return <div className="st-sub">{current.reason}</div>;
}

/** Facts about one selected item, with the same actions and confirmations as its right-click menu. */
function ItemInspector({
  row,
  info,
  parentBytes,
  network,
  busy,
  onClose,
  onOpen,
  onReveal,
  onCopy,
  onMove,
  onTrash,
  onCompress,
}: {
  row: Row;
  /** The Rust reads for this row; null while they are read. */
  info: { data: Inspect | null; error: string | null } | null;
  /** The whole listing's bytes, for the share; null for search results. */
  parentBytes: number | null;
  /** True when the item is on a network volume, which is never previewed. */
  network: boolean;
  busy: boolean;
  onClose: () => void;
  onOpen: (path: string) => void;
  onReveal: (path: string) => void;
  onCopy: (text: string) => void;
  onMove: (row: Row) => void;
  onTrash: (row: Row) => void;
  /** Present only while compression is switched on and the row is a JPEG, PNG, MP4 or MOV file. */
  onCompress?: (row: Row) => void;
}) {
  const kind = KINDS[kindOf(row.path, row.is_dir)];
  const data = info?.data ?? null;
  // Unknown reads as "Unknown", never as zero; while reading, "Reading…".
  const field = (pick: (d: Inspect) => string) => (!info ? "Reading…" : data ? pick(data) : "Unknown");
  const logical = data?.logical_bytes ?? null;
  const difference = logical === null ? null : row.bytes - logical;
  const share = parentBytes ? (row.bytes / parentBytes) * 100 : null;
  return (
    <aside className="st-inspector" aria-label="Item details">
      <div className="st-inspector-head">
        <span className="st-inspector-title">
          {row.is_dir ? <FolderIcon size={14} aria-hidden="true" /> : <File size={14} aria-hidden="true" />}
          <span>{row.name}</span>
        </span>
        <Button size="sm" variant="ghost" onClick={onClose}>
          <X size={12} aria-hidden="true" /> Close
        </Button>
      </div>
      <div className="st-inspector-path-line">
        <span className="st-inspector-path">{row.path}</span>
        <Button size="sm" onClick={() => onCopy(row.path)}>
          <Copy size={12} aria-hidden="true" /> Copy path
        </Button>
      </div>
      <ItemPreview row={row} network={network} />
      <dl className="st-facts">
        <dt>Kind</dt>
        <dd>{kind.label}</dd>
        <dt>Logical size</dt>
        <dd>{row.is_dir ? (info ? "Unknown" : "Reading…") : field((d) => (d.logical_bytes == null ? "Unknown" : bytes(d.logical_bytes)))}</dd>
        <dt>Allocated size</dt>
        <dd>{bytes(row.bytes)}</dd>
        {difference !== null && Math.abs(difference) >= 1_000_000 && (
          <>
            <dt>Allocated minus logical</dt>
            <dd>{signedBytes(difference)}</dd>
          </>
        )}
        {share !== null && (
          <>
            <dt>Share of this folder</dt>
            <dd>{share >= 1 ? `${Math.round(share)}%` : "<1%"}</dd>
          </>
        )}
        {row.is_dir && (
          <>
            <dt>Files directly inside</dt>
            <dd>{field((d) => (d.files == null ? "Unknown" : d.files.toLocaleString()))}</dd>
            <dt>Folders directly inside</dt>
            <dd>{field((d) => (d.folders == null ? "Unknown" : d.folders.toLocaleString()))}</dd>
          </>
        )}
        <dt>Created</dt>
        <dd>{field((d) => (d.created_at == null ? "Unknown" : localTime(d.created_at)))}</dd>
        <dt>Modified</dt>
        <dd>
          {info || !row.modified
            ? field((d) => (d.modified_at == null ? "Unknown" : localTime(d.modified_at)))
            : `${localTime(row.modified)}${row.is_dir ? " (newest inside)" : ""}`}
        </dd>
      </dl>
      {info?.error && <div className="st-sub">{info.error}</div>}
      <div className="st-inspector-actions">
        {row.is_dir && (
          <Button size="sm" onClick={() => onOpen(row.path)} disabled={busy}>
            <FolderOpen size={12} aria-hidden="true" /> Open folder
          </Button>
        )}
        <Button size="sm" onClick={() => onReveal(row.path)}>
          {row.is_dir ? "Open" : "Reveal"} in {FILE_MANAGER}
        </Button>
        <Button size="sm" onClick={() => onMove(row)} disabled={busy}>
          <FolderInput size={12} aria-hidden="true" /> Move to…
        </Button>
        {onCompress && (
          <Button size="sm" onClick={() => onCompress(row)} disabled={busy} title={`Writes a smaller copy in a ${appName()} Compressed folder beside it. The original is not changed.`}>
            Compress a copy
          </Button>
        )}
        <Button size="sm" variant="ghost" onClick={() => onTrash(row)} disabled={busy}>
          <Trash2 size={12} aria-hidden="true" /> Move to {TRASH}
        </Button>
      </div>
      <p className="st-help">
        Allocated is the disk space the scan counts for this item. Logical is the file&apos;s own length, which can be smaller for sparse or
        compressed files. Folders have no logical size here, because it is not added up. Removing this item could free up to its allocated
        size once the {TRASH} is emptied; hard links and clones can make the real gain smaller. This view does not show how the volume&apos;s
        free space changed. Folder counts cover direct children only.
      </p>
    </aside>
  );
}

export function Bar({ fraction, height = 4, color }: { fraction: number; height?: number; color?: string }) {
  const f = Math.min(Math.max(fraction, 0), 1);
  return (
    <div className="bar" style={{ height }}>
      <i style={{ width: `${f * 100}%`, background: color ?? tone(f) }} />
    </div>
  );
}
