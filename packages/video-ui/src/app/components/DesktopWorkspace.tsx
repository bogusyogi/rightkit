import { Button, IconButton, Notice, useToast } from "@rightkit/react-shell/react";
import { useCallback, useState } from "react";
import { useEngine } from "../../hooks/useEngine.js";
import { useProgress } from "../hooks/useProgress.js";
import { useVideoAssembly } from "../hooks/useVideoAssembly.js";
import { useVideoProjects } from "../hooks/useVideoProjects.js";
import { useVideoUpdateBusy, useVideoWorkspace } from "../hooks/useVideoWorkspace.js";
import { errorMessage } from "../../lib/engine.js";
import { baseName } from "../../lib/project.js";
import { Home } from "../views/Home.js";
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
  const progressController = useProgress();
  const { progress } = progressController;
  const { dir, isCurrentSelection, captureSelection, show, exportsCount, jobError, setJobError, output, setOutput, importMissing, setImportMissing, importUnverified, setImportUnverified, refresh, refreshChangedProject, open: openWorkspace, close: closeWorkspace, applyBundleReport } = useVideoWorkspace(progressController);
  const [tab, setTab] = useState<Tab>("projects");
  // Exports ⋯ → Import retention… routes here with the render preselected (R5).
  const [retentionPreselect, setRetentionPreselect] = useState<string | undefined>(undefined);
  /** Settings section to land on (status dot → Models). */
  const [settingsSection, setSettingsSection] = useState<string | undefined>(undefined);
  const error = statusError ?? status?.last_error ?? doctorError;
  const previewHost = useTimelinePreview(dir, show?.revision.cursor ?? 0);

  const { pending, resumable, renderMissing, assembling, rendering, submit, resume, render, cancel, clearRecovery } = useVideoAssembly({
    dir, isCurrentSelection, progressController, refresh, setError: setJobError, setOutput,
    callbacks: {
      onRecovered: () => setTab("jobs"),
      onStarted: () => setTab("jobs"),
      onSucceeded: () => { setTab("finished"); toast("Finished edit ready", { kind: "success" }); },
    },
  });

  useVideoUpdateBusy(dir, assembling, rendering, progress.running, setUpdateBusy);

  const open = useCallback((next: string, firstTab: Tab = "media") => {
    openWorkspace(next, () => setTab(firstTab), clearRecovery, (e) => toast(errorMessage(e), { kind: "error", ttl: 0 }));
  }, [openWorkspace, toast, clearRecovery]);
  const closeProject = useCallback(() => closeWorkspace(() => setTab("projects")), [closeWorkspace]);

  const { recents, recentInfo, dropActive, pickCreateProject, pickOpenProject, importBundle: importPickedBundle, openFiles } = useVideoProjects({
    dir, ready: status?.state === "ready", recentRefreshKey: tab === "projects", fallbackName: "Untitled", captureSelection,
    callbacks: {
      onOpen: (path) => open(path),
      onRecentError: (error) => setJobError(errorMessage(error)),
      onDropCreateError: (error) => toast(`Could not create a project folder: ${errorMessage(error)}`, { kind: "error", ttl: 0 }),
      onDropImportError: (error) => toast(errorMessage(error), { kind: "error", ttl: 0 }),
      onFilesImported: (count) => toast(`Imported ${count} file${count === 1 ? "" : "s"}`, { kind: "success" }),
    },
  });

  const createProject = useCallback(async () => {
    const result = await pickCreateProject("Choose or create a folder for the new project");
    if (result.status === "error") toast(errorMessage(result.error), { kind: "error", ttl: 0 });
  }, [pickCreateProject, toast]);

  const openFolder = useCallback(async () => {
    const result = await pickOpenProject(`Open a ${hostAppName()} project folder`);
    if (result.status === "error") toast(errorMessage(result.error), { kind: "error", ttl: 0 });
  }, [pickOpenProject, toast]);

  const importBundle = useCallback(async (): Promise<string | null> => {
    const result = await importPickedBundle({
      bundleTitle: "Choose the bundle folder to import",
      targetTitle: "Choose an empty folder for the imported project",
      applyReport: applyBundleReport,
    });
    if (result.status === "import-error") return errorMessage(result.error);
    if (result.status === "open-error") toast(errorMessage(result.error), { kind: "error", ttl: 0 });
    if (result.status === "imported" && !result.missing.length && !result.unverified.length) {
      toast("Bundle imported", { kind: "success" });
    }
    return null;
  }, [importPickedBundle, applyBundleReport, toast]);

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
              refreshChangedProject={refreshChangedProject}
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
              refreshChangedProject={refreshChangedProject}
              captureSelection={captureSelection}
              onImportRetention={(render) => {
                setRetentionPreselect(render);
                setTab("retention");
              }}
            />
          ) : tab === "shorts" ? (
            <ShortsView dir={dir} onChanged={refresh} refreshChangedProject={refreshChangedProject} />
          ) : tab === "phrases" ? (
            <PhrasesView dir={dir} onChanged={refresh} refreshChangedProject={refreshChangedProject} />
          ) : tab === "retention" ? (
            <RetentionView
              dir={dir}
              onChanged={refresh}
              refreshChangedProject={refreshChangedProject}
              onExportRevised={() => { if (isCurrentSelection()) setTab("exports"); }}
              preselectRender={retentionPreselect}
            />
          ) : tab === "assets" ? (
            <AssetsView dir={dir} captureSelection={captureSelection} refreshChangedProject={refreshChangedProject} />
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
