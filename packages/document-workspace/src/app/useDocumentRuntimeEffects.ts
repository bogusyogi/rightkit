import {
  useEffect,
  useRef,
  useState,
  type Dispatch,
  type MutableRefObject,
  type SetStateAction,
} from "react";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { docTitle, isUntitled, type LoadedDocument, type ViewMode } from "@rightkit/document-core/documents/types";
import {
  getInitialOpenPaths,
  inTauri,
  onAppEvent,
  grantKnownFileToken,
} from "@rightkit/document-core/state/tauriBridge";
import { clearSession, loadSession, saveSession } from "@rightkit/document-core/state/tauriBridge";
import { toast } from "@rightkit/platform-ui/headless";
import { readDraft } from "./documentDrafts.js";
import { scheduleAfterPaintOrDelay } from "@rightkit/document-core/app/deferredMount";
import { listenForNativeDrops } from "@rightkit/document-core/app/dropCoordination";
import type { BlockDraftRegistry } from "@rightkit/document-core/app/blockDrafts";
import type { OpenDocumentOptions } from "./useDocumentLoader.js";

/** Route native open-file events after the Markdown canvas has had a chance to consume image drops. */
export function handleOpenFileEvent(path: string, open: (path: string) => Promise<unknown>): void {
  // A multi-file Open With / rapid second-instance handoff emits one event per path back to back.
  // Run them one at a time so overlapping opens cannot interleave their tab, recents and session
  // writes; a failed open must not stall the files queued behind it.
  openFileChain = openFileChain.then(() => open(path)).then(
    () => undefined,
    () => undefined,
  );
}

let openFileChain: Promise<void> = Promise.resolve();

let sessionSaveRunning = false;
let sessionSaveNext: Array<[string, boolean]> | null = null;

/** Persist the session tabs with at most one native write in flight, newest snapshot last, so
 * out-of-order IPC completion can never leave an older, shorter tab list as the saved session. */
export function saveSessionOrdered(tabs: Array<[string, boolean]>): void {
  sessionSaveNext = tabs;
  if (sessionSaveRunning) return;
  sessionSaveRunning = true;
  void (async () => {
    try {
      while (sessionSaveNext) {
        const snapshot = sessionSaveNext;
        sessionSaveNext = null;
        try {
          await saveSession(snapshot);
        } catch {
          /* best effort; the next snapshot retries */
        }
      }
    } finally {
      sessionSaveRunning = false;
    }
  })();
}

/** Drop any queued session snapshot and wait for the in-flight native write to finish, so a purge
 * cannot be followed by a stale write that resurrects deleted rows. The next normal save writes the
 * current tabs only. */
export async function drainSessionSaves(): Promise<void> {
  sessionSaveNext = null;
  while (sessionSaveRunning) await new Promise((r) => setTimeout(r, 10));
  sessionSaveNext = null;
}

function safelyUnlisten(listener: () => void): void {
  try {
    // Tauri's dev bridge can return a rejecting thenable despite listener typing as void.
    // Consume both synchronous throws & asynchronous bridge teardown races.
    void Promise.resolve(listener()).catch(() => {});
  } catch {
    /* Tauri dev bridge may already have removed this listener. */
  }
}

// The benchmark autopilots are dev or browser-QA only (same build-time guard as
// `installResourceCensusDevHook`): in a production build these fold to constants and the
// autopilot branches are dropped, whatever the build environment carries.
const AUTOMATION_BUILD =
  import.meta.env?.DEV || import.meta.env?.VITE_RIGHTKIT_BROWSER_QA === "1";

export function nativeAutomationOwnsSession(): boolean {
  return (
    AUTOMATION_BUILD &&
    (import.meta.env?.VITE_PDF_BENCH_AUTOPILOT === "1" ||
      import.meta.env?.VITE_RESOURCE_MATRIX_AUTOPILOT === "1")
  );
}

function benchmarkInitialOpenPaths(initialOpenPaths: string[]): string[] {
  if (!AUTOMATION_BUILD || import.meta.env?.VITE_PDF_BENCH_AUTOPILOT !== "1") return initialOpenPaths;
  const fixturePath = String(import.meta.env?.VITE_PDF_BENCH_FIXTURE_PATH ?? "").trim();
  return fixturePath ? [...new Set([...initialOpenPaths, fixturePath])] : initialOpenPaths;
}

interface RuntimeEffectsDeps {
  openDocs: LoadedDocument[];
  activePath: string | null;
  hydrated: MutableRefObject<boolean>;
  untitledSeq: MutableRefObject<number>;
  refreshLibrary: () => Promise<void>;
  setOpenDocs: Dispatch<SetStateAction<LoadedDocument[]>>;
  writeEdits: (updater: (prev: Record<string, string>) => Record<string, string>) => void;
  setViews: Dispatch<SetStateAction<Record<string, ViewMode>>>;
  activate: (path: string) => Promise<void>;
  open: (path: string, opts?: OpenDocumentOptions) => Promise<void>;
  reload: (path: string) => void;
  isDirty: (doc: LoadedDocument) => boolean;
  setConfirmQuit: Dispatch<SetStateAction<boolean>>;
  prepareSetupRecovery: () => Promise<void>;
  restoreSetupRecovery: () => Promise<string | null>;
  restoreSession: boolean;
  // Block-draft registry: quit guard counts open drafts as unsaved work.
  blockDrafts?: BlockDraftRegistry;
  // Drives the launch-time "Restoring session…" indicator in the active-pane slot. The launch
  // effect sets it false in a `finally` so a thrown restore error still clears the indicator.
  setRestoringSession: Dispatch<SetStateAction<boolean>>;
}

export function useDocumentRuntimeEffects({
  openDocs,
  activePath,
  hydrated,
  untitledSeq,
  refreshLibrary,
  setOpenDocs,
  writeEdits,
  setViews,
  activate,
  open,
  reload,
  isDirty,
  setConfirmQuit,
  prepareSetupRecovery,
  restoreSetupRecovery,
  restoreSession,
  blockDrafts,
  setRestoringSession,
}: RuntimeEffectsDeps) {
  // Latest committed active tab, for the async launch flow: a launched file (or the user) that has
  // already taken activation must not be displaced by the background session restore.
  const activePathRef = useRef(activePath);
  activePathRef.current = activePath;
  // Bumped once when a file-launch finishes hydrating. The launched tabs open BEFORE hydration (so
  // they show first), which means their openDocs/activePath changes were skipped by the guard
  // below; this re-runs the persist effect once so the launched file is saved like it used to be.
  const [launchPersistTick, setLaunchPersistTick] = useState(0);

  // Persist the open-tab session (all tabs incl. untitled, + active) so it restores on next launch.
  // Guarded until after restore — otherwise the empty initial state would wipe the saved session.
  useEffect(() => {
    void launchPersistTick;
    if (!hydrated.current) return;
    // Native benchmark launchers own their exact fixture set. Restoring or persisting normal user
    // tabs would contaminate transport/resource measurements & overwrite the user's saved session.
    if (nativeAutomationOwnsSession()) return;
    const untitledPaths = openDocs.map((d) => d.path).filter(isUntitled);
    localStorage.setItem("vr.session.open", JSON.stringify(untitledPaths));
    localStorage.setItem(
      "vr.session.formats",
      JSON.stringify(
        Object.fromEntries(
          openDocs
            .filter((doc) => isUntitled(doc.path) && doc.format === "md")
            .map((doc) => [doc.path, doc.format]),
        ),
      ),
    );
    if (activePath && isUntitled(activePath)) {
      localStorage.setItem("vr.session.active", activePath);
    } else {
      localStorage.removeItem("vr.session.active");
    }
    if (!restoreSession) {
      void clearSession();
      return;
    }
    // Also persist file-backed tabs to SQLite for cross-launch restore.
    const fileTabs: Array<[string, boolean]> = openDocs
      .filter((d) => !isUntitled(d.path))
      .map((d) => [d.path, d.path === activePath]);
    saveSessionOrdered(fileTabs);
  }, [hydrated, openDocs, activePath, restoreSession, launchPersistTick]);

  // Launch: restore only never-saved buffers. Real files must re-enter through an OS-level
  // grant (dialog, drag/drop, file association, or confirmed recent).
  useEffect(() => {
    let disposed = false;
    let unOpenFile: (() => void) | undefined;
    let unNativeDrop: (() => void) | undefined;
    setRestoringSession(true);
    (async () => {
      try {
        const restoreFiles = restoreSession && !nativeAutomationOwnsSession();
        // The SQLite session read is independent of the listener / initial-path handshake below;
        // start it now so it overlaps those round-trips instead of queueing behind them. The no-op
        // catch only marks the rejection handled until it is awaited in the restore try/catch.
        const sessionLoad = restoreFiles ? loadSession() : null;
        sessionLoad?.catch(() => {});
        // Register BEFORE `rk_shell_open_paths_ready`: that shell command atomically marks the renderer
        // ready and drains paths queued by argv/macOS RunEvent::Opened. Once it returns, Rust may
        // emit future paths immediately, so installing this listener afterward would leave a small
        // but real startup window where Finder/second-instance opens disappear.
        const [un, unNative] = await Promise.all([
          onAppEvent("open-file", (path) => handleOpenFileEvent(path, open)),
          listenForNativeDrops(open),
        ]);
        if (disposed) {
          un();
          unNative();
          return;
        }
        unOpenFile = un;
        unNativeDrop = unNative;
        // Independent IPC round-trips — fan them out instead of paying two sequential awaits. The
        // recents refresh only feeds the sidebar, so a launched file does not wait for it.
        const libraryRefresh = refreshLibrary();
        libraryRefresh.catch(() => {});
        const initialOpenPaths = benchmarkInitialOpenPaths(await getInitialOpenPaths());

        for (let i = localStorage.length - 1; i >= 0; i--) {
          const key = localStorage.key(i);
          if (key?.startsWith("vr.draft.") && !key.startsWith("vr.draft.untitled:")) {
            localStorage.removeItem(key);
          }
        }

        // File launch (double-click, "Open with", argv): the launched file is what the user asked
        // to see, so it opens before the saved file-backed session and owns activation.
        const launchedPaths = new Set(initialOpenPaths);
        const launchOwnsActive = launchedPaths.size > 0;

        let sessionPaths: string[] = [];
        try {
          const parsed = JSON.parse(localStorage.getItem("vr.session.open") ?? "[]");
          sessionPaths = Array.isArray(parsed) ? parsed.filter(isUntitled) : [];
        } catch {
          /* ignore */
        }
        let markdownDraftPaths = new Set<string>();
        try {
          const formats: unknown = JSON.parse(localStorage.getItem("vr.session.formats") ?? "{}");
          if (formats && typeof formats === "object" && !Array.isArray(formats)) {
            markdownDraftPaths = new Set(
              Object.entries(formats)
                .filter(([path, format]) => isUntitled(path) && format === "md")
                .map(([path]) => path),
            );
          }
        } catch {
          /* Older or malformed session metadata restores as plain text. */
        }
        const savedActive = localStorage.getItem("vr.session.active");
        const paths = [...new Set(sessionPaths)];

        for (const path of paths) {
          const draft = readDraft(path) ?? "";
          setOpenDocs((prev) =>
            prev.some((x) => x.path === path)
              ? prev
              : [
                  ...prev,
                  {
                    path,
                    displayName: docTitle(path),
                    format: markdownDraftPaths.has(path) ? "md" : "txt",
                    content: "",
                  },
                ],
          );
          writeEdits((e) => ({ ...e, [path]: draft }));
          setViews((v) => ({ ...v, [path]: "preview" }));
          const n = Number.parseInt(path.split(":")[1] ?? "0", 10);
          if (Number.isFinite(n) && n >= untitledSeq.current) untitledSeq.current = n + 1;
        }
        // Activate the saved-active untitled buffer up front (vs a random fallback untitled)
        // so the user always sees content — never the empty state — while the file-backed
        // restore loop runs. Activating savedUntitledActive is safe: localStorage's
        // `vr.session.active` is set ONLY for untitled paths (the persist effect removes it
        // when the active becomes a file), so when savedUntitledActive is truthy, the file-
        // backed loop's `sessionActivePath` is guaranteed null and no swap can happen.
        //
        // A `fallbackUntitled` (paths[0] when the previous active was a file — so savedActive
        // is empty) is NOT activated up front: doing so would briefly show a wrong tab before
        // the file-backed loop swapped to the real saved-active file (~300ms flash). Instead,
        // the launch effect defers it; the "Restoring session…" indicator covers the wait
        // because `openDocs.length > 0` (the untitled loop above populated at least one row).
        const savedUntitledActive =
          savedActive && isUntitled(savedActive) && paths.includes(savedActive)
            ? savedActive
            : null;
        const fallbackUntitled = savedUntitledActive ? null : paths[0];
        if (savedUntitledActive && !launchOwnsActive) await activate(savedUntitledActive);

        // Session file path → its saved session position.
        const restoredOrder = new Map<string, number>();
        // Untitled buffers are already visible. Launch files stay immediately after them;
        // restored files follow in saved order, so background restore cannot move a launch row.
        const tabGroup = (path: string) =>
          launchedPaths.has(path) ? 1 : restoredOrder.has(path) ? 2 : 0;
        const arrange = (docs: LoadedDocument[]) => {
          const next = docs
            .map((doc, index) => ({ doc, index, group: tabGroup(doc.path) }))
            .sort(
              (a, b) =>
                a.group - b.group ||
                (a.group === 2
                  ? (restoredOrder.get(a.doc.path) ?? 0) - (restoredOrder.get(b.doc.path) ?? 0)
                  : 0) ||
                a.index - b.index,
            )
            .map(({ doc }) => doc);
          return next.every((doc, i) => doc === docs[i]) ? docs : next;
        };

        if (launchOwnsActive) {
          // Never await session metadata or grants before showing the requested file. The first
          // successful foreground open owns activation; a failed earlier path must not leave
          // later successes hidden behind the empty state.
          let launchActivated = false;
          for (const [index, path] of initialOpenPaths.entries()) {
            await open(path, {
              background: launchActivated,
              deferLibraryRefresh: true,
              keepError: index > 0,
              arrange,
            });
            if (!launchActivated) {
              // open() reports failures through the error banner, not its return value. Wait for
              // its state to commit before deciding whether the next file should be background.
              await new Promise<void>((resolve) => scheduleAfterPaintOrDelay(resolve));
              launchActivated = activePathRef.current !== null;
            }
          }
        }
        await libraryRefresh;

        let fileMissing = 0;
        let sessionActivePath: string | null = null;
        let firstRestored: string | null = null;
        let libraryRefreshed = false;
        if (sessionLoad) {
          // Restore file-backed tabs from the SQLite session. Open them in the BACKGROUND (no activation)
          // so each tab is a lazy placeholder, then activate exactly ONE at the end — otherwise every tab
          // would flash active as it opens and render up front, defeating the lazy first-render.
          try {
            const session = await sessionLoad;
            // Restore saved-active first so the user sees their document before background work starts.
            // Yield one committed frame before the remaining opens compete for startup resources.
            const sessionOrder = new Map(session.map(([token], index) => [token, index]));
            const restoreEntry = async ([token, active]: [string, boolean]) => {
              const restored = await grantKnownFileToken(token);
              if (!restored) return { active, restored: null };
              restoredOrder.set(restored, sessionOrder.get(token) ?? session.length);
              // A launched file that was also in the session is already open and fresh.
              if (!launchedPaths.has(restored)) {
                // keepError: a restore open must not clear an error it didn't set (e.g. the
                // launched file failing to open); its own failure still reports.
                await open(restored, {
                  background: true,
                  deferLibraryRefresh: true,
                  keepError: true,
                  arrange,
                });
              }
              return { active, restored };
            };
            const activeEntry = session.find(([, active]) => active);
            const backgroundEntries = session.filter((entry) => entry !== activeEntry);
            if (activeEntry) {
              const activeResult = await restoreEntry(activeEntry);
              if (activeResult.restored) {
                sessionActivePath = activeResult.restored;
                firstRestored = activeResult.restored;
                if (!launchOwnsActive) {
                  await activate(activeResult.restored);
                  await new Promise<void>((resolve) => scheduleAfterPaintOrDelay(resolve));
                }
              } else {
                fileMissing++;
              }
            }
            // Fan out the remaining per-tab grant + background open — each is an independent IPC round-trip
            // (token grant, then read_document), so serializing them was the dominant cost of a
            // multi-tab restore. Promise.allSettled keeps one bad token/file from aborting the rest
            // (matches the original per-token try/catch), and it preserves the INPUT order in its
            // results array regardless of completion order, so which tab ends up "first restored" /
            // "active" below is still decided by session order, not by which IPC call wins the race.
            // Each open() defers its own refreshLibrary — it runs ONCE after the loop instead of once
            // per tab.
            // Bounded fan-out: each open() also prefetches its format island (CodeMirror, PDF.js,
            // remark…), so restoring a big mixed session fully concurrently turns cold start into an
            // I/O + chunk-download burst. Four at a time keeps the pipeline busy without the stampede.
            const RESTORE_CONCURRENCY = 4;
            const results: PromiseSettledResult<{ active: boolean; restored: string | null }>[] =
              [];
            for (let i = 0; i < backgroundEntries.length; i += RESTORE_CONCURRENCY) {
              results.push(
                ...(await Promise.allSettled(
                  backgroundEntries.slice(i, i + RESTORE_CONCURRENCY).map(restoreEntry),
                )),
              );
            }
            for (const result of results) {
              if (result.status !== "fulfilled" || !result.value.restored) {
                fileMissing++;
                continue;
              }
              const { active, restored } = result.value;
              firstRestored ??= restored;
              if (active) sessionActivePath = restored;
            }
            if (firstRestored || launchedPaths.size > 0) {
              await refreshLibrary();
              libraryRefreshed = true;
            }
            // Each restored tab was inserted in place already, so this is normally a no-op (same
            // array back); it still settles anything added outside this flow meanwhile, as before.
            setOpenDocs(arrange);
          } catch {
            /* session load failed — non-fatal */
          }
        }
        if (launchedPaths.size > 0 && !libraryRefreshed) await refreshLibrary();

        // A file launch keeps activation unless its open failed and nothing else became active
        // (then fall through to the normal restore choice below). Wait one committed frame first
        // so the ref reflects the launched open's activation rather than a pending render.
        let keepLaunchActive = false;
        if (launchOwnsActive) {
          await new Promise<void>((resolve) => scheduleAfterPaintOrDelay(resolve));
          keepLaunchActive = activePathRef.current !== null;
        }
        if (!keepLaunchActive) {
          // Activate one tab so only it renders now. Prefer the saved-active file (covers the
          // case where savedUntitledActive was null and we deferred), then the deferred
          // fallbackUntitled (covers the case where the file-backed loop found no active file
          // either, and restoreSession off, where no file-backed tabs load at all), then the
          // first restored file (never land on the empty state while tabs are open). A
          // saved-active untitled was already activated up front — except on a failed file
          // launch, which skipped that — and the chain never overrides it with `firstRestored`,
          // which is just a "first file we happened to open in the loop", not the user's actual
          // previous active. activate() calls are idempotent (React skips the re-render if the
          // path didn't change).
          if (sessionActivePath) await activate(sessionActivePath);
          else if (savedUntitledActive) {
            if (launchOwnsActive) await activate(savedUntitledActive);
          } else if (fallbackUntitled) await activate(fallbackUntitled);
          else if (firstRestored) await activate(firstRestored);
        }
        if (fileMissing > 0) toast(`${fileMissing} session file(s) not found`, "info");

        const recoveredActive = await restoreSetupRecovery();
        if (recoveredActive && !keepLaunchActive) await activate(recoveredActive);

        hydrated.current = true;
        // The launched tabs opened before hydration; persist the session they now belong to.
        if (launchOwnsActive && !disposed) setLaunchPersistTick((tick) => tick + 1);
      } finally {
        // Always clear the restore indicator — even if restore throws, so the user never sees
        // a stuck "Restoring session…" pane. The state setter is safe to call repeatedly.
        if (!disposed) setRestoringSession(false);
      }
    })();
    return () => {
      disposed = true;
      unOpenFile?.();
      unNativeDrop?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [
    activate,
    open,
    restoreSession,
    restoreSetupRecovery,
    writeEdits,
    untitledSeq,
    setViews,
    setOpenDocs,
    refreshLibrary,
    hydrated,
    setRestoringSession,
  ]);

  // OS / drag-drop / watcher events.
  useEffect(() => {
    let disposed = false;
    const uns: Array<() => void> = [];
    const track = (p: Promise<() => void>) =>
      void p.then((u) => (disposed ? safelyUnlisten(u) : uns.push(u))).catch(() => {});
    track(onAppEvent("file-changed", (path) => reload(path)));
    track(onAppEvent("prepare-setup", () => void prepareSetupRecovery()));
    // Diagnostic: surface the Rust drag-drop event stream in the devtools console so we can see
    // whether drops over the sidebar reach the webview at all.
    return () => {
      disposed = true;
      uns.forEach(safelyUnlisten);
    };
  }, [prepareSetupRecovery, reload]);

  // Guard the window close: if any doc has unsaved changes, prompt instead of quitting — EXCEPT
  // untitled txt scratch buffers, which are auto-persisted to localStorage and restored on next
  // launch (Notepad-style), so quitting never loses them. Only file-backed / Word edits can be lost.
  useEffect(() => {
    if (!inTauri()) return;
    let unlisten: (() => void) | undefined;
    let disposed = false;
    getCurrentWindow()
      .onCloseRequested((event) => {
        // txt (both untitled and file-backed) persists edits to localStorage and restores them on
        // next launch (Notepad-style) — no data loss, so no prompt needed for any txt format.
        const dirtyDocs = openDocs.some((d) => isDirty(d) && d.format !== "txt");
        const openDrafts = blockDrafts ? blockDrafts.snapshotModified().length > 0 : false;
        if (dirtyDocs || openDrafts) {
          event.preventDefault();
          setConfirmQuit(true);
        }
      })
      .then((u) => {
        if (disposed) safelyUnlisten(u);
        else unlisten = u;
      });
    return () => {
      disposed = true;
      if (unlisten) safelyUnlisten(unlisten);
    };
  }, [openDocs, isDirty, setConfirmQuit, blockDrafts]);
}
