import { create } from "zustand";

export const PROJECT_RECENTS_STORAGE_KEY = "coderight.project-recents.v1";
export const MAX_PROJECT_RECENTS = 16;

export interface ProjectRecent {
  path: string;
  label: string;
  lastUsed: number;
  pinned: boolean;
  group: string | null;
  iconUrl?: string;
}

export interface ProjectRecentGroup {
  name: string;
  projects: ProjectRecent[];
}

interface PersistedProjectRecents {
  version: 1;
  projects: ProjectRecent[];
  activePath: string | null;
}

export interface ProjectRecentsState {
  projects: ProjectRecent[];
  activePath: string | null;
  addOrTouch: (path: string, label?: string) => ProjectRecent | null;
  remove: (path: string) => void;
  togglePinned: (path: string) => void;
  setGroup: (path: string, group: string | null) => void;
  setIcon: (path: string, iconUrl: string | null) => void;
  setActivePath: (path: string | null) => void;
  hydrate: () => void;
  clear: () => void;
}

function storage(): Storage | null {
  if (typeof localStorage === "undefined") return null;
  return localStorage;
}

export function normalizeProjectPath(input: string): string {
  const value = input.trim().replaceAll("\\", "/");
  if (!value) return "";
  if (value === "/") return value;
  if (/^[A-Za-z]:\/$/.test(value)) return value;
  return value.replace(/\/+$/, "");
}

export function projectLabel(path: string): string {
  const normalized = normalizeProjectPath(path);
  if (!normalized || normalized === "/") return normalized || "Project";
  const withoutDriveRoot = normalized.replace(/^[A-Za-z]:\/$/, "");
  const parts = withoutDriveRoot.split("/").filter(Boolean);
  return parts.at(-1) ?? normalized;
}

function validProject(value: unknown): value is ProjectRecent {
  if (!value || typeof value !== "object") return false;
  const item = value as Partial<ProjectRecent>;
  return (
    typeof item.path === "string" &&
    normalizeProjectPath(item.path).length > 0 &&
    typeof item.label === "string" &&
    typeof item.lastUsed === "number" &&
    Number.isFinite(item.lastUsed)
  );
}

function sanitizeProjects(value: unknown): ProjectRecent[] {
  if (!Array.isArray(value)) return [];
  const byPath = new Map<string, ProjectRecent>();
  for (const item of value) {
    if (!validProject(item)) continue;
    const raw = item as ProjectRecent;
    const path = normalizeProjectPath(raw.path);
    if (byPath.has(path)) continue;
    byPath.set(path, {
      path,
      label: raw.label.trim() || projectLabel(path),
      lastUsed: raw.lastUsed,
      pinned: raw.pinned === true,
      group: typeof raw.group === "string" && raw.group.trim() ? raw.group.trim() : null,
      ...(typeof raw.iconUrl === "string" && raw.iconUrl ? { iconUrl: raw.iconUrl } : {}),
    });
  }
  return orderProjectRecents([...byPath.values()]).slice(0, MAX_PROJECT_RECENTS);
}

function readPersisted(): { projects: ProjectRecent[]; activePath: string | null } {
  const store = storage();
  if (!store) return { projects: [], activePath: null };
  try {
    const parsed = JSON.parse(store.getItem(PROJECT_RECENTS_STORAGE_KEY) ?? "null") as
      | Partial<PersistedProjectRecents>
      | null;
    const projects = sanitizeProjects(parsed?.projects);
    const candidateActivePath =
      typeof parsed?.activePath === "string" ? normalizeProjectPath(parsed.activePath) : "";
    const activePath =
      candidateActivePath && projects.some((project) => project.path === candidateActivePath)
        ? candidateActivePath
        : null;
    return { projects, activePath };
  } catch {
    return { projects: [], activePath: null };
  }
}

function persist(projects: ProjectRecent[], activePath: string | null): void {
  const store = storage();
  if (!store) return;
  try {
    const payload: PersistedProjectRecents = {
      version: 1,
      projects,
      activePath,
    };
    store.setItem(PROJECT_RECENTS_STORAGE_KEY, JSON.stringify(payload));
  } catch {
    // Recents are a convenience cache; a blocked or full browser store must
    // never prevent a project from opening.
  }
}

export function orderProjectRecents(projects: readonly ProjectRecent[]): ProjectRecent[] {
  return [...projects].sort((a, b) => {
    if (a.pinned !== b.pinned) return a.pinned ? -1 : 1;
    return b.lastUsed - a.lastUsed;
  });
}

export function groupProjectRecents(projects: readonly ProjectRecent[]): ProjectRecentGroup[] {
  const groups = new Map<string, ProjectRecent[]>();
  for (const project of orderProjectRecents(projects)) {
    const name = project.group?.trim() || "Ungrouped";
    const entries = groups.get(name) ?? [];
    entries.push(project);
    groups.set(name, entries);
  }
  return [...groups.entries()].map(([name, entries]) => ({ name, projects: entries }));
}

const initial = readPersisted();

export const useProjectRecentsStore = create<ProjectRecentsState>((set, get) => ({
  projects: initial.projects,
  activePath: initial.activePath,

  addOrTouch(path, label) {
    const normalized = normalizeProjectPath(path);
    if (!normalized) return null;
    const existing = get().projects.find((project) => project.path === normalized);
    const next: ProjectRecent = {
      ...(existing ?? {
        path: normalized,
        label: label?.trim() || projectLabel(normalized),
        pinned: false,
        group: null,
      }),
      label: label?.trim() || existing?.label || projectLabel(normalized),
      lastUsed: Date.now(),
    };
    const projects = orderProjectRecents([
      next,
      ...get().projects.filter((project) => project.path !== normalized),
    ]).slice(0, MAX_PROJECT_RECENTS);
    const activePath = normalized;
    set({ projects, activePath });
    persist(projects, activePath);
    return next;
  },

  remove(path) {
    const normalized = normalizeProjectPath(path);
    const projects = get().projects.filter((project) => project.path !== normalized);
    const activePath = get().activePath === normalized ? null : get().activePath;
    set({ projects, activePath });
    persist(projects, activePath);
  },

  togglePinned(path) {
    const normalized = normalizeProjectPath(path);
    const projects = orderProjectRecents(
      get().projects.map((project) =>
        project.path === normalized ? { ...project, pinned: !project.pinned } : project,
      ),
    );
    set({ projects });
    persist(projects, get().activePath);
  },

  setGroup(path, group) {
    const normalized = normalizeProjectPath(path);
    const trimmed = group?.trim() || null;
    const projects = get().projects.map((project) =>
      project.path === normalized ? { ...project, group: trimmed } : project,
    );
    set({ projects });
    persist(projects, get().activePath);
  },

  setIcon(path, iconUrl) {
    const normalized = normalizeProjectPath(path);
    const projects = get().projects.map((project) => {
      if (project.path !== normalized) return project;
      const { iconUrl: _previous, ...withoutIcon } = project;
      return iconUrl?.trim() ? { ...withoutIcon, iconUrl: iconUrl.trim() } : withoutIcon;
    });
    set({ projects });
    persist(projects, get().activePath);
  },

  setActivePath(path) {
    const activePath = path ? normalizeProjectPath(path) || null : null;
    set({ activePath });
    persist(get().projects, activePath);
  },

  hydrate() {
    set(readPersisted());
  },

  clear() {
    set({ projects: [], activePath: null });
    persist([], null);
  },
}));

export const useProjectRecents = useProjectRecentsStore;