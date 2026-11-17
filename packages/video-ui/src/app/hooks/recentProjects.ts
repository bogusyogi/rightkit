import { errorCode } from "../../lib/engine.js";
import { createHostSettingsBackend } from "../lib/settingsBackend.js";

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
