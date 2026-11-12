import { Button, IconButton, Notice, useToast } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useState } from "react";
import { videoAppHost } from "../../host.js";
import { useEngine } from "../../hooks/useEngine.js";
import { useProgress } from "../hooks/useProgress.js";
import {
  type AssembleForm,
  type AssembleInput,
  type AssembleStatus,
  type PendingAssemble,
  buildAssembleInput,
  clearPending,
  loadPending,
  newAssembleId,
  savePending,
} from "../lib/assemble.js";
import { pickFolder } from "../lib/dialog.js";
import { engineCall, errorCode, errorMessage } from "../../lib/engine.js";
import { type BundleImportResult, SETTINGS_CHANGED, bundleImport, projectsList, rendersList, settingsGet, storageStatus } from "../../lib/media.js";
import { type ProjectShow, baseName, projectInit, projectShow } from "../../lib/project.js";
import { Home, loadRecent, rememberProject } from "../views/Home.js";
import { ProgressView } from "../views/ProgressView.js";
import { PromptPanel } from "../views/PromptPanel.js";
import { TimelineView } from "../views/TimelineView.js";
import { SourcesView } from "../views/SourcesView.js";
import { AutoEditView } from "../views/AutoEditView.js";
import { ShortsView } from "../views/ShortsView.js";
import { PhrasesView } from "../views/PhrasesView.js";
import { RetentionView } from "../views/RetentionView.js";
import { AssetsView } from "../views/AssetsView.js";
import { ExportsView } from "../views/ExportsView.js";
import { SettingsView } from "../views/SettingsView.js";
import { AppShell, StatusRow, useShortcuts, type Command, type NavGroup } from "@rightkit/react-shell/react";
import { shellBridge } from "../lib/shell.js";
import { setUpdateBusy } from "../lib/services.js";
import { mediaScopeAllow } from "../../lib/media.js";
import { editorShortcuts } from "../../editor/keys.js";
import { useTimelinePreview } from "../../components/Player.js";
import { Icon } from "../ui/index.js";
import { hostAppName } from "../appName.js";

type Tab =
  | "projects"
  | "media"
  | "recipe"
  | "finished"
  | "exports"
  | "shorts"
  | "phrases"
  | "retention"
  | "assets"
  | "prompt"
  | "jobs"
  | "settings";

/** Project tabs in nav order; ⌘1–4 select them by position. */
const PROJECT_TABS: { tab: Tab; label: string }[] = [
  { tab: "media", label: "Media" },
  { tab: "recipe", label: "Edit recipe" },
  { tab: "finished", label: "Finished edit" },
  { tab: "exports", label: "Exports" },
];

/** Display name for a recent-projects row: stored project name, else the
 *  folder name de-slugged. Mirrors Home's `displayName`. */
export function recentLabel(dir: string, name?: string | null): string {
  if (name) return name;
  const folder = baseName(dir).replace(/\.cutright$/, "");
  const words = folder.replace(/[-_]+/g, " ").trim();
  return words.length > 0 ? words[0].toUpperCase() + words.slice(1) : folder;
}

export function DesktopWorkspace() {
  const toast = useToast();
  const { status, doctor, doctorError, statusError } = useEngine();
  const { progress, begin, patch, seed, reset } = useProgress();
  const [pending, setPending] = useState<PendingAssemble | null>(null);
  const [resumable, setResumable] = useState(false);
  /** The recovered run's stored render file is gone; its render step must run again. */
  const [renderMissing, setRenderMissing] = useState(false);
  const [dir, setDir] = useState<string | null>(null);
  const [show, setShow] = useState<ProjectShow | null>(null);
  const [exportsCount, setExportsCount] = useState(0);
  const [tab, setTab] = useState<Tab>("projects");
  // Exports ⋯ → Import retention… routes here with the render preselected (R5).
  const [retentionPreselect, setRetentionPreselect] = useState<string | undefined>(undefined);
  /** R16: where dropped-footage projects are created (never the footage dir). */
  const [projectRoot, setProjectRoot] = useState<string | null>(null);
  /** Recent rows: display names + missing-media state from `projects.list`. */
  const [recentInfo, setRecentInfo] = useState<Record<string, { name?: string | null; missing?: boolean }>>({});
  /** Settings section to land on (status dot → Models). */
  const [settingsSection, setSettingsSection] = useState<string | undefined>(undefined);
  const [assembling, setAssembling] = useState(false);
  const [rendering, setRendering] = useState(false);
  const [jobError, setJobError] = useState<string | null>(null);
  const [output, setOutput] = useState<string | null>(null);
  /** Names missing on this computer after a bundle import; shown until dismissed. */
  const [importMissing, setImportMissing] = useState<string[] | null>(null);
  const [importUnverified, setImportUnverified] = useState<string[] | null>(null);
  const [dropActive, setDropActive] = useState(false);
  const error = statusError ?? status?.last_error ?? doctorError;
  const previewHost = useTimelinePreview(dir, show?.revision.cursor ?? 0);
  const [recents, setRecents] = useState<string[]>([]);

  useEffect(() => {
    if (status?.state !== "ready") return;
    let live = true;
    const load = () => void loadRecent()
      .then((dirs) => { if (live) setRecents(dirs.slice(0, 5)); })
      .catch((e) => { if (live) setJobError(errorMessage(e)); });
    load();
    window.addEventListener(SETTINGS_CHANGED, load);
    return () => {
      live = false;
      window.removeEventListener(SETTINGS_CHANGED, load);
    };
  }, [dir, tab === "projects", status?.state]);

  useEffect(() => {
    setUpdateBusy(dir !== null || assembling || rendering || progress.running);
    return () => setUpdateBusy(false);
  }, [dir, assembling, rendering, progress.running]);

  // Dynamic asset scope: project dir + its renders/media dirs. The scope glob skips
  // dot-directories unless named, so the thumbnail cache (.cache/thumbs) is listed.
  useEffect(() => {
    if (!dir) return;
    void mediaScopeAllow([dir, `${dir}/media`, `${dir}/renders`, `${dir}/variants`, `${dir}/.cache`], true).catch(
      () => undefined,
    );
  }, [dir]);

  // R16: resolve the drop project folder once — the configured app setting,
  // else the engine's platform default (~/Movies/the host app).
  useEffect(() => {
    let live = true;
    (async () => {
      try {
        const [s, st] = await Promise.all([settingsGet(), storageStatus()]);
        if (!live) return;
        const configured = s.app.project_root;
        setProjectRoot(
          typeof configured === "string" && configured.trim()
            ? configured
            : st.default_project_root ?? null,
        );
      } catch {
        /* engine still starting; drops are ignored until this resolves */
      }
    })();
    return () => {
      live = false;
    };
  }, []);

  // Recent-project rows show the stored name and a warn dot for missing media.
  useEffect(() => {
    if (recents.length === 0) return;
    let live = true;
    projectsList(recents)
      .then((r) => {
        if (!live) return;
        const info: Record<string, { name?: string | null; missing?: boolean }> = {};
        for (const dir of recents) info[dir] = { missing: true };
        for (const p of r.projects) {
          info[p.dir] = {
            name: p.name,
            missing: (p.missing_sources ?? 0) > 0 || p.status === "missing_media",
          };
        }
        setRecentInfo(info);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [recents]);

  const refresh = useCallback(async () => {
    if (!dir) return;
    setShow(await projectShow(dir));
    rendersList(dir)
      .then((r) => setExportsCount(r.renders.filter((x) => x.name.endsWith(".mp4")).length))
      .catch(() => undefined);
  }, [dir]);
  useEffect(() => {
    if (!dir) return;
    let cancelled = false;
    projectShow(dir)
      .then((s) => !cancelled && setShow(s))
      .catch((e) => !cancelled && setJobError(errorMessage(e)));
    rendersList(dir)
      .then((r) => !cancelled && setExportsCount(r.renders.filter((x) => x.name.endsWith(".mp4")).length))
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [dir]);
  useEffect(() => {
    if (progress.output) setOutput(progress.output);
    if (progress.done || progress.error) void refresh().catch(() => undefined);
  }, [progress.output, progress.done, progress.error, refresh]);

  const open = useCallback((next: string, firstTab: Tab = "media") => {
    setShow(null);
    setOutput(null);
    setJobError(null);
    // importBundle calls open before it sets its own notice, so the notice it shows survives this clear.
    setImportMissing(null);
    setImportUnverified(null);
    reset();
    setTab(firstTab);
    setPending(null);
    setResumable(false);
    setRenderMissing(false);
    setDir(next);
    void rememberProject(next).catch((e) => toast(errorMessage(e), { kind: "error", ttl: 0 }));
  }, [reset, toast]);

  const closeProject = useCallback(() => {
    setDir(null);
    setShow(null);
    setImportMissing(null);
    setImportUnverified(null);
    setTab("projects");
  }, []);

  const createProject = useCallback(async () => {
    const dir = await pickFolder("Choose or create a folder for the new project");
    if (!dir) return;
    try {
      await projectInit(dir, baseName(dir));
      open(dir);
    } catch (e) {
      toast(errorMessage(e), { kind: "error", ttl: 0 });
    }
  }, [open, toast]);

  /** Checks an existing project folder with the engine, then opens it. Shared by Open folder… and Import bundle. */
  const openExisting = useCallback(
    async (path: string) => {
      await projectShow(path);
      open(path);
    },
    [open],
  );

  const openFolder = useCallback(async () => {
    const picked = await pickFolder(`Open a ${hostAppName()} project folder`);
    if (!picked) return;
    try {
      await openExisting(picked);
    } catch (e) {
      toast(errorMessage(e), { kind: "error", ttl: 0 });
    }
  }, [openExisting, toast]);

  /** Two folder picks (bundle, then new project folder), `bundle.import`, then the Open folder… path.
   *  Resolves with the error to show on Home, or null when nothing failed. */
  const importBundle = useCallback(async (): Promise<string | null> => {
    const bundle = await pickFolder("Choose the bundle folder to import");
    if (!bundle) return null;
    const target = await pickFolder("Choose an empty folder for the imported project");
    if (!target) return null;
    let report: BundleImportResult;
    try {
      report = await bundleImport({ bundle, dir: target });
    } catch (e) {
      return errorMessage(e);
    }
    try {
      await openExisting(report.dir);
    } catch (e) {
      toast(errorMessage(e), { kind: "error", ttl: 0 });
      return null;
    }
    const missing = [
      ...report.missing_media.map((m) => m.name ?? baseName(m.path)),
      ...report.missing_assets.map((path) => baseName(path)),
      ...(report.missing_generated_sources ?? []).map((g) => baseName(g.path)),
    ];
    const unverified = [
      ...(report.unverified_assets ?? []).map((path) => baseName(path)),
      ...(report.unverified_generated_sources ?? []).map((g) => baseName(g.path)),
    ];
    if (unverified.length > 0) setImportUnverified(unverified);
    if (missing.length > 0) setImportMissing(missing);
    else if (unverified.length === 0) toast("Bundle imported", { kind: "success" });
    return null;
  }, [openExisting, toast]);

  /** §3.1 + R16: dropped media creates a project under the configured project
   *  folder (Settings › Storage › Project folder; default ~/Movies/the host app) —
   *  never inside the user's footage folder. The project is named after the
   *  first dropped file's stem (`Interview`, then `Interview 2`, …) and the
   *  originals are imported in place. */
  const openFiles = useCallback(
    async (paths: string[]) => {
      const first = paths[0];
      if (!first || !projectRoot) return;
      const stem = baseName(first).replace(/\.[^.]+$/, "").replace(/[\\/]/g, "").trim() || "Untitled";
      // `projects.create` picks `stem`, `stem 2`, … — never an occupied dir.
      let created: string;
      try {
        const r = await engineCall<{ dir: string }>("projects.create", { root: projectRoot, name: stem });
        created = r.dir;
      } catch (e) {
        toast(`Could not create a project folder: ${errorMessage(e)}`, { kind: "error", ttl: 0 });
        return;
      }
      try {
        open(created);
        for (const file of paths) {
          await engineCall("source.add", { dir: created, file });
        }
        toast(`Imported ${paths.length} file${paths.length === 1 ? "" : "s"}`, { kind: "success" });
      } catch (e) {
        toast(errorMessage(e), { kind: "error", ttl: 0 });
      }
    },
    [open, toast, projectRoot],
  );

  // File drops land on the Projects screen only; inside a project the Media
  // screen owns its own drop handling.
  useEffect(() => {
    let un: (() => void) | undefined;
    // A missing app host takes the same silent path as a failed subscription, as before.
    void Promise.resolve()
      .then(() =>
        videoAppHost().onDragDrop((e) => {
          if (dir) return;
          if (e.type === "enter" || e.type === "over") setDropActive(true);
          else if (e.type === "leave") setDropActive(false);
          else if (e.type === "drop") {
            setDropActive(false);
            void openFiles(e.paths);
          }
        }),
      )
      .then((u) => (un = u))
      .catch(() => undefined);
    return () => un?.();
  }, [dir, openFiles]);

  // Global keyboard (§2): the shell's shortcut registry owns the chords, the help overlay and dispatch.
  // Project tabs apply only with a project open; a skipped binding leaves the key to the page.
  useShortcuts([
    { id: "cr.new-project", chord: "mod+n", label: "New project", group: "Projects", run: () => void createProject() },
    { id: "cr.open-project", chord: "mod+o", label: "Open project folder", group: "Projects", run: () => void openFolder() },
    { id: "cr.settings", chord: "mod+,", label: "Settings", group: "Navigation", run: () => setTab("settings") },
    ...PROJECT_TABS.map(({ tab, label }, i) => ({
      id: `cr.tab.${tab}`,
      chord: `mod+${i + 1}`,
      label: `Go to ${label}`,
      group: "Navigation",
      when: () => Boolean(dir),
      run: () => setTab(tab),
    })),
    ...editorShortcuts(),
  ]);

  // On open: if this project has an unfinished assemble, ask the engine where it stands.
  useEffect(() => {
    if (!dir) return;
    const saved = loadPending(dir);
    if (!saved) return;
    let cancelled = false;
    engineCall<AssembleStatus>("assemble.status", { dir, assemble_id: saved.assembleId })
      .then((st) => {
        if (cancelled) return;
        // A stored render whose file is gone must run again, so the run is not cleared as finished.
        const missing = !!st.render && st.render_exists === false;
        if (st.status === "done" && !missing) {
          clearPending(dir);
          return;
        }
        setPending(saved);
        setResumable(true);
        setRenderMissing(missing);
        seed(st);
        // Recovered run: land on Progress so the user can resume it.
        setTab("jobs");
      })
      .catch((e) => {
        if (cancelled) return;
        // not_found: the engine never recorded this run; the saved request can still be sent.
        if (errorCode(e) === "not_found") {
          setPending(saved);
          setResumable(true);
          setTab("jobs");
        } else setJobError(errorMessage(e));
      });
    return () => {
      cancelled = true;
    };
  }, [dir, seed]);

  const run = async (record: PendingAssemble) => {
    if (!dir) return;
    setAssembling(true);
    setResumable(false);
    setRenderMissing(false);
    setJobError(null);
    setTab("jobs");
    const tag = begin("assemble", record.assembleId);
    try {
      const result = await engineCall<{ render?: { output?: string } }>(
        "assemble",
        { dir, input: record.input },
        tag,
      );
      if (result?.render?.output) setOutput(result.render.output);
      patch({ running: false, done: true });
      clearPending(dir);
      setPending(null);
      await refresh();
      setTab("finished");
      toast("Finished edit ready", { kind: "success" });
    } catch (e) {
      const code = errorCode(e);
      if (code === "cancelled" || code === "render_cancelled") patch({ running: false, cancelled: true });
      else {
        setJobError(errorMessage(e));
        patch({ running: false });
      }
      setResumable(true);
      await refresh().catch(() => undefined);
    } finally {
      setAssembling(false);
    }
  };

  const submit = (form: AssembleForm) => {
    // The id is fixed here and persisted with the exact input: re-sending both resumes.
    const assembleId = newAssembleId();
    const input: AssembleInput = buildAssembleInput(form, assembleId);
    const record: PendingAssemble = { assembleId, input, startedAt: Date.now() };
    if (dir) savePending(dir, record);
    setPending(record);
    void run(record);
  };

  const resume = () => {
    if (pending) void run(pending);
  };

  const render = async (preset: "youtube" | "reels" | "tiktok" | "square") => {
    if (!dir) return;
    setRendering(true);
    setJobError(null);
    const tag = begin("render");
    const job = `ui-${tag}`;
    patch({ render: { job }, running: true });
    try {
      const result = await engineCall<{ output?: string; status?: string }>("render", { dir, preset, job }, tag);
      if (result?.output) setOutput(result.output);
    } catch (e) {
      setJobError(errorMessage(e));
    } finally {
      setRendering(false);
      patch({ render: null, running: false });
    }
  };

  // `assemble.cancel` also stops the assemble's in-flight render (engine side).
  const cancel = () => {
    const report = (e: unknown) => setJobError(errorMessage(e));
    const aid = progress.assembleId;
    if (aid) {
      void engineCall("assemble.cancel", { assemble_id: aid }).catch(report);
    } else if (progress.render?.job) {
      void engineCall("render.cancel", { job: progress.render.job }).catch(report);
    }
  };

  const projectName = show?.project.name ?? (dir ? baseName(dir) : undefined);
  const mediaCount = show?.summary.sources ?? 0;

  const projectGroups: NavGroup[] = dir
    ? [
        {
          title: "Edit footage",
          items: [
            { id: "media", label: "Media", icon: <Icon name="film" />, badge: mediaCount > 0 ? <span className="ws-count">{mediaCount}</span> : undefined },
            { id: "recipe", label: "Edit recipe", icon: <Icon name="sliders-horizontal" /> },
            { id: "finished", label: "Finished edit", icon: <Icon name="list-video" /> },
            { id: "exports", label: "Exports", icon: <Icon name="download" />, badge: exportsCount > 0 ? <span className="ws-count">{exportsCount}</span> : undefined },
          ],
        },
        {
          title: "Clip & repurpose",
          items: [
            { id: "shorts", label: "Shorts", icon: <Icon name="scissors" /> },
            { id: "phrases", label: "Find phrases", icon: <Icon name="text-quote" /> },
            { id: "retention", label: "Retention", icon: <Icon name="chart-line" /> },
          ],
        },
        {
          title: "Create",
          items: [
            { id: "assets", label: "Assets", icon: <Icon name="image" /> },
            { id: "prompt", label: "From prompt", icon: <Icon name="wand-sparkles" /> },
          ],
        },
      ]
    : [{ items: [{ id: "projects", label: "Projects", icon: <Icon name="folder" /> }] }];
  const groups: NavGroup[] =
    recents.length > 0
      ? [
          ...projectGroups,
          {
            title: "Recent projects",
            items: recents.map((r) => ({
              id: `recent:${r}`,
              label: recentLabel(r, recentInfo[r]?.name),
              icon: <span className={`ws-dot${dir === r ? " ws-dot--cur" : recentInfo[r]?.missing ? " ws-dot--missing" : ""}`} />,
              keywords: ["recent", r],
            })),
          },
        ]
      : projectGroups;
  const activeId = tab === "settings" ? "" : dir ? tab : "projects";
  const navigate = (id: string) => {
    if (id.startsWith("recent:")) open(id.slice("recent:".length));
    else if (id === "projects") closeProject();
    else setTab(id as Tab);
  };
  const commands: Command[] = [
    { id: "cr.new-project", label: "New project", shortcut: "cr.new-project", run: () => void createProject() },
    { id: "cr.open-project", label: "Open project folder…", shortcut: "cr.open-project", run: () => void openFolder() },
    ...(dir ? [{ id: "cr.close-project", label: "Close project", run: closeProject }] : []),
  ];
  const engineTone = status?.state === "ready" ? "ok" : error ? "bad" : "warn";

  return (
    <AppShell
      groups={groups}
      activeId={activeId}
      onNavigate={navigate}
      title={
        dir ? (
          <span className="ws-crumb">
            <button type="button" className="ws-crumb__link" onClick={closeProject}>Projects</button>
            <span aria-hidden>/</span>
            <strong>{projectName}</strong>
          </span>
        ) : undefined
      }
      sidebarTop={
        <div className="ws-side-top">
          <Button variant="secondary" onClick={() => void createProject()} className="cr-btn--block"><Icon name="plus" />
            New project
          </Button>
        </div>
      }
      wordmark={
        <span className="ws-wordmark">
          Cut<span>Right</span>
        </span>
      }
      statusFooter={
        <>
          <p className="ws-foot-note">Originals stay untouched · every edit is reversible</p>
          <button
            type="button"
            className="ws-status"
            title={error ?? undefined}
            onClick={() => {
              setSettingsSection("models");
              setTab("settings");
            }}
          >
            <StatusRow tone={engineTone} label="Engine" value={status?.state === "ready" ? "Ready" : error ? "Needs attention" : "Starting"} />
          </button>
        </>
      }
      onOpenSettings={() => setTab("settings")}
      settingsActive={tab === "settings"}
      commands={commands}
      bridge={shellBridge}
    >
      <div className="ws-plane">
          {importMissing && (
            <Notice action={<IconButton label="Dismiss" onClick={() => setImportMissing(null)}><Icon name="x" /></IconButton>}>
              Imported. Missing on this computer: <strong>{importMissing.join(", ")}</strong>
            </Notice>
          )}
          {importUnverified && (
            <Notice action={<IconButton label="Dismiss" onClick={() => setImportUnverified(null)}><Icon name="x" /></IconButton>}>
              Imported. Found on this computer but could not be checked against the original:{" "}
              <strong>{importUnverified.join(", ")}</strong>
            </Notice>
          )}
          {tab === "settings" ? (
            <SettingsView dir={dir} status={status} doctor={doctor} error={error} section={settingsSection} />
          ) : !dir ? (
            <Home
              ready={status?.state === "ready"}
              dropActive={dropActive}
              onOpen={open}
              onCreate={() => void createProject()}
              onOpenFolder={() => void openFolder()}
              onImportBundle={importBundle}
              onFiles={(paths) => void openFiles(paths)}
            />
          ) : tab === "media" ? (
            <SourcesView
              dir={dir}
              onChanged={refresh}
              onNavigate={(to) => setTab(to === "recipe" ? "recipe" : "finished")}
            />
          ) : tab === "recipe" ? (
            <AutoEditView
              dir={dir}
              onChanged={refresh}
              onNavigate={(to) => setTab(to === "recipe" ? "recipe" : "finished")}
            />
          ) : tab === "finished" ? (
            show ? (
              <TimelineView
                show={show}
                progress={progress}
                outputPath={output}
                rendering={rendering || (progress.running && !!progress.render)}
                previewHost={previewHost}
                onChanged={refresh}
                onRender={(p) => void render(p)}
              />
            ) : (
              <ProgressView progress={progress} jobError={jobError} assembling={assembling} onCancel={cancel} resumable={resumable} renderMissing={renderMissing} recovered={pending?.input.shots.map((s, i) => s.prompt || `Shot ${i + 1}`)} onResume={resume} />
            )
          ) : tab === "exports" ? (
            <ExportsView
              dir={dir}
              progress={progress}
              rendering={rendering || (progress.running && !!progress.render)}
              outputPath={output}
              onRender={(p) => void render(p)}
              onChanged={refresh}
              onImportRetention={(render) => {
                setRetentionPreselect(render);
                setTab("retention");
              }}
            />
          ) : tab === "shorts" ? (
            <ShortsView dir={dir} onChanged={refresh} />
          ) : tab === "phrases" ? (
            <PhrasesView dir={dir} onChanged={refresh} />
          ) : tab === "retention" ? (
            <RetentionView
              dir={dir}
              onChanged={refresh}
              onExportRevised={() => setTab("exports")}
              preselectRender={retentionPreselect}
            />
          ) : tab === "assets" ? (
            <AssetsView dir={dir} onNewVideo={() => setTab("prompt")} />
          ) : tab === "prompt" ? (
            <PromptPanel
              busy={assembling}
              progress={progress}
              onSubmit={submit}
              onOpenFinished={() => setTab("finished")}
            />
          ) : (
            <ProgressView
              progress={progress}
              jobError={jobError}
              assembling={assembling}
              onCancel={cancel}
              resumable={resumable}
              renderMissing={renderMissing}
              recovered={pending?.input.shots.map((s, i) => s.prompt || `Shot ${i + 1}`)}
              onResume={resume}
            />
          )}
      </div>
    </AppShell>
  );
}
