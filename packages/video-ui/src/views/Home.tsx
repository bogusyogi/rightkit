import { Badge, Button, Card, ContextMenu, EmptyState, IconButton, PageShell, cx } from "@rightkit/react-shell/react";
import { useCallback, useEffect, useRef, useState } from "react";
import { pickFolder } from "../lib/dialog.js";
import { errorCode, errorMessage, revealPath } from "../lib/engine.js";
import {
  type ProjectCard,
  SETTINGS_CHANGED,
  mediaScopeAllow,
  projectsList,
  thumbnails,
} from "../lib/media.js";
import { assetUrl } from "../lib/asset.js";
import { baseName, projectShow } from "../lib/project.js";
import { createHostSettingsBackend } from "../lib/settingsBackend.js";
import { ErrorPanel, Thumbnail, timecode, Icon } from "../ui/index.js";


const recentSettings = createHostSettingsBackend();
let recentQueue: Promise<unknown> = Promise.resolve();
const recentDirs = (value: unknown): string[] =>
  Array.isArray(value) ? value.filter((dir): dir is string => typeof dir === "string").slice(0, 8) : [];

function recentProjects(update?: (dirs: string[]) => string[]): Promise<string[]> {
  const run = recentQueue.then(async () => {
    for (let attempt = 0; ; attempt += 1) {
      const snapshot = await recentSettings.load();
      let dirs = recentDirs(snapshot.values.recent_projects);
      if (!update) return dirs;
      dirs = update(dirs);
      try {
        await recentSettings.patch({ recent_projects: dirs }, snapshot.revision);
        return dirs;
      } catch (error) {
        if (attempt > 0 || errorCode(error) !== "revision_conflict") throw error;
      }
    }
  });
  recentQueue = run.catch(() => undefined);
  return run;
}

export const loadRecent = () => recentProjects();
export const rememberProject = (dir: string) => recentProjects((dirs) => [dir, ...dirs.filter((d) => d !== dir)].slice(0, 8));
export const forgetProject = (dir: string) => recentProjects((dirs) => dirs.filter((d) => d !== dir));

const STATUS: Record<string, { label: string; tone: "ok" | "accent" | "neutral" | "warn" }> = {
  finished: { label: "Finished", tone: "ok" },
  needs_review: { label: "Needs review", tone: "accent" },
  media_only: { label: "Media only", tone: "neutral" },
  missing_media: { label: "Missing media", tone: "warn" },
};

function editedAgo(edited?: number | null): string {
  if (!edited) return "";
  const s = Math.max(0, Date.now() / 1000 - edited);
  if (s < 90) return "edited just now";
  if (s < 3600) return `edited ${Math.round(s / 60)}m ago`;
  if (s < 86400) return `edited ${Math.round(s / 3600)}h ago`;
  return `edited ${Math.round(s / 86400)}d ago`;
}

function metaLine(p: ProjectCard): string {
  const parts: string[] = [];
  if (p.duration_ms) parts.push(timecode(p.duration_ms));
  if (p.aspect) parts.push(p.aspect);
  if (p.sources !== undefined) parts.push(`${p.sources} clip${p.sources === 1 ? "" : "s"}`);
  const ago = editedAgo(p.edited);
  if (ago) parts.push(ago);
  return parts.join(" · ");
}

/** Owner review: stored `project.json` name, then the folder name de-slugged
 *  (`finding-your-voice` → `Finding your voice`). */
function displayName(p: ProjectCard): string {
  if (p.name) return p.name;
  const folder = baseName(p.dir).replace(/\.cutright$/, "");
  const words = folder.replace(/[-_]+/g, " ").trim();
  return words.length > 0 ? words[0].toUpperCase() + words.slice(1) : folder;
}

function ProjectCardView({
  card,
  onOpen,
  onRemove,
}: {
  card: ProjectCard;
  onOpen: (card: ProjectCard) => void;
  onRemove: (dir: string) => void;
}) {
  const [thumb, setThumb] = useState<string | undefined>();
  const [menu, setMenu] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    let live = true;
    setThumb(undefined);
    const t = card.thumb;
    if (!t) return;
    const target = t.render ? { render: t.render } : t.source ? { source: t.source } : null;
    if (!target) return;
    // Render targets have no engine-side duration; frame the render at 20% of
    // the card duration (per owner review), sources default to 20% inside.
    const at_ms = t.render && card.duration_ms ? Math.round(card.duration_ms / 5) : undefined;
    thumbnails(card.dir, target, { width: 480, ...(at_ms ? { at_ms } : {}) })
      .then((r) => {
        if (live && r.frames[0]) setThumb(assetUrl(r.frames[0].path));
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, [card]);

  const st = STATUS[card.status ?? ""] ?? (card.error ? STATUS.missing_media : STATUS.media_only);
  const name = displayName(card);

  return (
    <div className="Home-cardWrap">
      <Card flush className="Home-card" onClick={() => onOpen(card)} label={`Open ${name}`}>
        <Thumbnail src={thumb} badge={card.duration_ms ? timecode(card.duration_ms) : undefined} />
        <div className="Home-cardBody">
          <div className="Home-cardName">{name}</div>
          <div className="Home-cardMeta" title={card.dir}>
            {metaLine(card)}
          </div>
          <Badge tone={st.tone}>{st.label}</Badge>
        </div>
      </Card>
      <div className={cx("Home-cardMenu", menu && "Home-cardMenu--open")} ref={menuRef}>
        <IconButton
          label="Project actions"
          onClick={() => setMenu((m) => !m)}
        ><Icon name="more-horizontal" /></IconButton>
        {menu && menuRef.current && (
          <ContextMenu
            anchor={menuRef.current}
            placement="bottom-end"
            label="Project actions"
            itemAs="button"
            keyboard="escape-only"
            autoFocus={false}
            showLead={false}
            onClose={() => setMenu(false)}
            items={[
              { id: "reveal", label: "Reveal in Finder", run: () => void revealPath(card.dir).catch(() => undefined) },
              { id: "remove", label: "Remove from list", run: () => onRemove(card.dir) },
            ]}
          />
        )}
      </div>
    </div>
  );
}

export function Home({
  ready,
  dropActive,
  onOpen,
  onCreate,
  onOpenFolder,
  onImportBundle,
  onFiles,
}: {
  ready: boolean;
  dropActive: boolean;
  onOpen: (dir: string) => void;
  onCreate: () => void;
  onOpenFolder: () => void;
  /** Picks a bundle and a new folder, imports and opens it. Resolves with the error to show, or null. */
  onImportBundle: () => Promise<string | null>;
  /** Create a project from dropped/picked media files. */
  onFiles: (paths: string[]) => void;
}) {
  const [cards, setCards] = useState<ProjectCard[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [broken, setBroken] = useState<ProjectCard | null>(null);
  const [importError, setImportError] = useState<string | null>(null);

  const load = useCallback(async () => {
    try {
      setError(null);
      const recents = await loadRecent();
      if (recents.length === 0) {
        setCards([]);
        return;
      }
      const r = await projectsList(recents);
      const byDir = new Map(r.projects.map((p) => [p.dir, p]));
      // Recents the engine did not return (folder deleted) stay as warn cards.
      const merged = recents.map(
        (dir) =>
          byDir.get(dir) ??
          ({ dir, name: baseName(dir), status: "missing_media", error: "Folder not found" } as ProjectCard),
      );
      // Plus anything else discovered under the recent roots (subfolders).
      for (const p of r.projects) if (!merged.some((m) => m.dir === p.dir)) merged.push(p);
      // Card thumbnails stream through the asset protocol — grant scope
      // before the cards mount so their <img> requests aren't refused.
      await mediaScopeAllow(
        merged.filter((c) => !c.error).map((c) => c.dir),
        true,
      ).catch(() => undefined);
      setCards(merged);
    } catch (e) {
      setError(errorMessage(e));
      setCards([]);
    }
  }, []);

  useEffect(() => {
    if (!ready) return;
    void load();
    const changed = () => void load();
    window.addEventListener(SETTINGS_CHANGED, changed);
    return () => window.removeEventListener(SETTINGS_CHANGED, changed);
  }, [load, ready]);

  const openCard = (card: ProjectCard) => {
    setBroken(null);
    if (card.error || card.status === "missing_media") {
      void projectShow(card.dir)
        .then(() => onOpen(card.dir))
        .catch(() => setBroken(card));
      return;
    }
    onOpen(card.dir);
  };

  const locate = async () => {
    const dir = await pickFolder("Locate the project folder");
    if (!dir) return;
    void projectShow(dir)
      .then(async () => {
        await rememberProject(dir);
        setBroken(null);
        onOpen(dir);
      })
      .catch((e) => setError(errorMessage(e)));
  };

  const remove = (dir: string) => {
    void forgetProject(dir).then(() => {
      if (broken?.dir === dir) setBroken(null);
      return load();
    }).catch((e) => setError(errorMessage(e)));
  };

  const pickFiles = async () => {
    const { pickMedia } = await import("../lib/dialog.js");
    const files = await pickMedia();
    if (files.length > 0) onFiles(files);
  };

  const importBundle = () => {
    setImportError(null);
    void onImportBundle().then((message) => setImportError(message));
  };

  const grid = (cards?.length ?? 0) > 0;

  return (
    <PageShell
      title="Projects"
      actions={
        <>
          <Button variant="secondary" onClick={importBundle} disabled={!ready}>
            Import bundle…
          </Button>
          <Button variant="secondary" onClick={onOpenFolder} disabled={!ready}><Icon name="folder-open" />
            Open folder…
          </Button>
          <Button variant="primary" onClick={onCreate} disabled={!ready}><Icon name="plus" />
            New project
          </Button>
        </>
      }
      maxWidth={0}
    >
      <div className={cx("Home-body", dropActive && "Home-body--drop")}>
        {!ready && <p className="Home-dim">Waiting for the engine…</p>}
        {error && <ErrorPanel title="Something went wrong" detail={error} />}
        {importError && <ErrorPanel title="Couldn't import the bundle" message={importError} />}
        {broken && (
          <ErrorPanel
            title={`Can't open ${broken.name ?? baseName(broken.dir)}`}
            detail={broken.error ?? "The project folder or media is missing."}
            actions={<Button onClick={() => void locate()}>Locate folder…</Button>}
          />
        )}
        {!grid && cards !== null && (
          <div className="Home-dropzone">
            <EmptyState
              title="Drop footage to start"
              action={
                <Button variant="primary" onClick={() => void pickFiles()} disabled={!ready}>
                  Choose files…
                </Button>
              }
              icon={<Icon name="clapperboard" width={28} height={28} />}
            >
              Talking head, screen recordings or any clips. Originals are never modified.
            </EmptyState>
          </div>
        )}
        {grid && (
          <div className="Home-grid">
            {cards!.map((c) => (
              <ProjectCardView key={c.dir} card={c} onOpen={openCard} onRemove={remove} />
            ))}
          </div>
        )}
      </div>
    </PageShell>
  );
}
