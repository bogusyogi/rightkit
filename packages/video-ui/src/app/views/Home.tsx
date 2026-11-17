import { Badge, Button, Card, ContextMenu, EmptyState, IconButton, PageShell, cx } from "@rightkit/react-shell/react";
import { useRef, useState } from "react";
import { type ProjectCard } from "../../lib/media.js";
import { baseName } from "../../lib/project.js";
import { useVideoHome, useVideoHomeThumbnail } from "../hooks/useVideoHome.js";
export { loadRecent, rememberProject, forgetProject } from "../hooks/recentProjects.js";
import { ErrorPanel, Thumbnail, timecode, Icon } from "../ui/index.js";


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
  onReveal,
}: {
  card: ProjectCard;
  onOpen: (card: ProjectCard) => void;
  onRemove: (dir: string) => void;
  onReveal: (dir: string) => void;
}) {
  const thumb = useVideoHomeThumbnail(card);
  const [menu, setMenu] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

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
              { id: "reveal", label: "Reveal in Finder", run: () => onReveal(card.dir) },
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
  const { cards, error, broken, importError, openCard, locate, remove, pickFiles, importBundle, reveal } = useVideoHome({
    ready, onOpen, onImportBundle, onFiles,
    missingFolderMessage: "Folder not found",
    locateTitle: "Locate the project folder",
  });

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
              <ProjectCardView key={c.dir} card={c} onOpen={openCard} onRemove={remove} onReveal={reveal} />
            ))}
          </div>
        )}
      </div>
    </PageShell>
  );
}
