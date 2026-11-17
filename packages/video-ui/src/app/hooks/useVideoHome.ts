import { useCallback, useEffect, useState } from "react";
import { pickFolder } from "../lib/dialog.js";
import { errorMessage, revealPath } from "../../lib/engine.js";
import { type ProjectCard, SETTINGS_CHANGED, mediaScopeAllow, projectsList, thumbnails } from "../../lib/media.js";
import { assetUrl } from "../../lib/asset.js";
import { baseName, projectShow } from "../../lib/project.js";
import { loadRecent, rememberProject, forgetProject } from "./recentProjects.js";

/** Thumbnail selection & stale-result handling for a project card. */
export function useVideoHomeThumbnail(card: ProjectCard) {
  const [thumb, setThumb] = useState<string | undefined>();

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

  return thumb;
}

/** Home project data & actions; the host supplies navigation and copy. */
export function useVideoHome({ ready, onOpen, onImportBundle, onFiles, missingFolderMessage, locateTitle }: {
  ready: boolean;
  onOpen: (dir: string) => void;
  onImportBundle: () => Promise<string | null>;
  onFiles: (paths: string[]) => void;
  missingFolderMessage: string;
  locateTitle: string;
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
          ({ dir, name: baseName(dir), status: "missing_media", error: missingFolderMessage } as ProjectCard),
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
  }, [missingFolderMessage]);

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
    const dir = await pickFolder(locateTitle);
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

  const reveal = (dir: string) => { void revealPath(dir).catch(() => undefined); };
  return { cards, error, broken, importError, openCard, locate, remove, pickFiles, importBundle, reveal };
}
