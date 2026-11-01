// Shared state for the explorer sidebar (Part C): the "vr.explorer.v1" settings-mirror record
// (expanded dirs, section open/closed, sidebar width, scroll position) with a module-level cache +
// debounced write, plus the small path helpers the lazy tree and the active-file reveal need.
import { getSettingsMirror, setSettingsMirror } from "@rightkit/document-core/state/tauriBridge";
import { splitPath } from "../SidebarPathUtils";

export const EXPLORER_SETTINGS_KEY = "vr.explorer.v1";

export interface ExplorerSectionState {
  pinned: boolean;
  recent: boolean;
  locations: boolean;
}

export interface ExplorerPersisted {
  /** Dir paths currently expanded in the Locations tree / Pinned folders. */
  expanded: string[];
  sections: ExplorerSectionState;
  /** Sidebar pixel width (also mirrored to localStorage "vr.sidebar.width"). */
  width: number;
  /** Explorer scroll offset (scrollTop of the nav container). */
  scroll: number;
}

export function defaultExplorerPersisted(): ExplorerPersisted {
  return {
    expanded: [],
    sections: { pinned: true, recent: true, locations: true },
    width: 0,
    scroll: 0,
  };
}

let cache: ExplorerPersisted | null = null;
/** Set once any save has landed — a late-resolving load must not stomp a fresher write. */
let mutated = false;
let writeTimer: number | null = null;

function sanitize(raw: unknown): ExplorerPersisted {
  const fallback = defaultExplorerPersisted();
  if (raw === null || typeof raw !== "object") return fallback;
  const o = raw as Partial<ExplorerPersisted>;
  const s = o.sections as Partial<ExplorerSectionState> | undefined;
  return {
    expanded: Array.isArray(o.expanded)
      ? o.expanded.filter((p): p is string => typeof p === "string")
      : [],
    sections: {
      pinned: typeof s?.pinned === "boolean" ? s.pinned : true,
      recent: typeof s?.recent === "boolean" ? s.recent : true,
      locations: typeof s?.locations === "boolean" ? s.locations : true,
    },
    width: typeof o.width === "number" && Number.isFinite(o.width) ? o.width : 0,
    scroll: typeof o.scroll === "number" && Number.isFinite(o.scroll) ? o.scroll : 0,
  };
}

/** Load the persisted explorer record once (SQLite mirror via settings_get); merges over defaults
 *  and caches the result so the sidebar shell and the sections share one IPC read. */
export async function loadExplorerPersisted(): Promise<ExplorerPersisted> {
  if (cache) return cache;
  const raw = await getSettingsMirror(EXPLORER_SETTINGS_KEY);
  if (mutated && cache) return cache;
  let parsed = defaultExplorerPersisted();
  if (raw != null) {
    try {
      parsed = sanitize(JSON.parse(raw));
    } catch {
      /* corrupt mirror value — keep defaults */
    }
  }
  if (!mutated) cache = parsed;
  return cache ?? parsed;
}

/** Synchronous read of the last-loaded record (defaults before hydration resolves). */
export function explorerPersistedSync(): ExplorerPersisted {
  return cache ?? defaultExplorerPersisted();
}

/** Merge a patch into the record and schedule a debounced (~300ms) mirror write. */
export function saveExplorerPersisted(patch: Partial<ExplorerPersisted>): void {
  mutated = true;
  cache = { ...(cache ?? defaultExplorerPersisted()), ...patch };
  if (writeTimer != null) window.clearTimeout(writeTimer);
  writeTimer = window.setTimeout(() => {
    writeTimer = null;
    if (cache) void setSettingsMirror(EXPLORER_SETTINGS_KEY, JSON.stringify(cache));
  }, 300);
}

// ── Path helpers (separator-agnostic; the tree sees both / and \ paths) ──────

/** Case-insensitive "is `child` strictly inside `dir`" on path segments. */
export function pathInside(dir: string, child: string): boolean {
  const d = splitPath(dir).join("/").toLowerCase();
  const c = splitPath(child).join("/").toLowerCase();
  return c.length > d.length && c.startsWith(d + "/");
}

/** The chain of ancestor directories of `filePath` from `rootDir` (inclusive) down to the file's
 *  parent dir — used by "reveal the active file" to expand each level. Returns [] when the file is
 *  not under rootDir. Ancestors are rebuilt from the file path's own segments so they keep the
 *  backend's canonical casing (they must string-match the `path` keys of tree dir entries). */
export function ancestorDirs(filePath: string, rootDir: string): string[] {
  if (!pathInside(rootDir, filePath)) return [];
  const rootSegs = splitPath(rootDir);
  const fileSegs = splitPath(filePath);
  const sep = filePath.includes("\\") ? "\\" : "/";
  const out: string[] = [];
  for (let i = rootSegs.length; i < fileSegs.length; i++) {
    if (i === rootSegs.length) {
      out.push(rootDir);
      continue;
    }
    const head = fileSegs[0] ?? "";
    if (/^[A-Za-z]:$/.test(head)) {
      out.push(head + sep + fileSegs.slice(1, i).join(sep));
    } else {
      out.push((filePath.startsWith("/") ? "/" : "") + fileSegs.slice(0, i).join(sep));
    }
  }
  return out;
}

/** The name of the child of `dir` on the way to `target` (the target itself when it is a direct
 *  child), or undefined when `dir` is not a strict ancestor of `target`. Separator-agnostic:
 *  `C:\Temp` with `C:\Temp\abc\x.md` gives "abc". */
export function childNameOnPath(dir: string, target: string | null): string | undefined {
  if (!target || !pathInside(dir, target)) return undefined;
  return splitPath(target)[splitPath(dir).length];
}

const KNOWN_FORMATS = new Set([
  "md",
  "mdx",
  "txt",
  "pdf",
  "html",
  "docx",
  "image",
  "web",
  "unknown",
]);

/** ExplorerEntry.kind → row kind. "dir" stays "dir", a supported format id becomes "file"
 *  (openable, broker-granted path), anything else is "other" (real path, no grant — never open). */
export function rowKindOf(kind: string): "dir" | "file" | "other" {
  if (kind === "dir") return "dir";
  if (kind === "other" || !KNOWN_FORMATS.has(kind)) return "other";
  return "file";
}
