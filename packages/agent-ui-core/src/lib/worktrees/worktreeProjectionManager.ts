import {
  createWorktree as createDaemonWorktree,
  listWorktrees as listDaemonWorktrees,
  removeWorktree as removeDaemonWorktree,
} from "../daemon/delivery-environments-attention";
import type { DaemonConfig } from "../types/settings-plugins";

/** The renderer's read-only projection of a daemon-owned Git worktree. */
export interface WorktreeMetadata {
  source: "daemon";
  path: string;
  projectDirectory: string;
  branch: string;
  label: string;
  name?: string;
  worktreeRoot: string;
  worktreeStatus: "ready" | "missing" | "invalid" | "not-a-repo";
  headState: "branch" | "detached" | "unborn";
  worktreeSource: "existing" | "created-for-session";
}

export interface ProjectRef {
  id: string;
  path: string;
}

export interface CreateWorktreeArgs {
  branch: string;
  base?: string;
  path?: string;
  force?: boolean;
}

export interface RemoveWorktreeOptions {
  force?: boolean;
}

interface PorcelainWorktreeEntry {
  path?: string;
  branch?: string;
  head?: string;
}

const CACHE_TTL_MS = 30_000;
const worktreeCache = new Map<string, { value: WorktreeMetadata[]; at: number }>();
const worktreeInflight = new Map<string, Promise<WorktreeMetadata[]>>();
const worktreeGeneration = new Map<string, number>();

export const normalizePath = (value: string): string => {
  const replaced = value.trim().replace(/\\/g, "/");
  if (replaced === "/") return "/";
  return replaced.length > 1 ? replaced.replace(/\/+$/, "") : replaced;
};

const generationFor = (directory: string): number => worktreeGeneration.get(directory) ?? 0;

export const invalidateWorktreeList = (directory: string): void => {
  const key = normalizePath(directory);
  worktreeGeneration.set(key, generationFor(key) + 1);
  worktreeCache.delete(key);
};

const parsePorcelainWorktrees = (stdout: string): PorcelainWorktreeEntry[] => {
  const entries: PorcelainWorktreeEntry[] = [];
  let current: PorcelainWorktreeEntry | null = null;

  for (const line of stdout.replace(/\r\n/g, "\n").split("\n")) {
    if (line === "") {
      if (current) entries.push(current);
      current = null;
      continue;
    }
    const separator = line.indexOf(" ");
    const key = separator < 0 ? line : line.slice(0, separator);
    const value = separator < 0 ? "" : line.slice(separator + 1).trim();
    if (key === "worktree") {
      if (current) entries.push(current);
      current = { path: value };
    } else if (current && key === "HEAD") {
      current.head = value;
    } else if (current && key === "branch") {
      current.branch = value;
    }
  }
  if (current) entries.push(current);
  return entries;
};

const deriveHeadState = (entry: PorcelainWorktreeEntry): WorktreeMetadata["headState"] => {
  const branch = (entry.branch ?? "").trim();
  const head = (entry.head ?? "").trim();
  if (branch) return "branch";
  return head ? "detached" : "unborn";
};

const directoryName = (directory: string): string => {
  const parts = normalizePath(directory).split("/").filter(Boolean);
  return parts[parts.length - 1] ?? directory;
};

const toMetadata = (
  entry: PorcelainWorktreeEntry,
  projectDirectory: string,
  worktreeSource: WorktreeMetadata["worktreeSource"],
): WorktreeMetadata | null => {
  const path = normalizePath(entry.path ?? "");
  if (!path) return null;
  const branch = (entry.branch ?? "").replace(/^refs\/heads\//, "").trim();
  const name = directoryName(path);
  return {
    source: "daemon",
    path,
    projectDirectory,
    branch,
    label: branch || name,
    name,
    worktreeRoot: path,
    worktreeStatus: "ready",
    headState: deriveHeadState(entry),
    worktreeSource,
  };
};

const readProjectWorktrees = async (
  config: DaemonConfig,
  projectDirectory: string,
): Promise<WorktreeMetadata[]> => {
  const result = await listDaemonWorktrees(config, projectDirectory);
  if (result.exitCode !== 0) {
    throw new Error(result.stderrTail || result.stdoutTail || "Git worktree listing failed");
  }
  const projectPath = normalizePath(projectDirectory);
  return parsePorcelainWorktrees(result.stdoutTail)
    .map((entry) => toMetadata(entry, projectPath, "existing"))
    .filter((entry): entry is WorktreeMetadata => entry !== null && entry.path !== projectPath)
    .sort((a, b) => a.label.toLowerCase().localeCompare(b.label.toLowerCase()));
};

const readStableProjectWorktrees = async (
  config: DaemonConfig,
  projectDirectory: string,
): Promise<WorktreeMetadata[]> => {
  while (true) {
    const generation = generationFor(projectDirectory);
    const worktrees = await readProjectWorktrees(config, projectDirectory);
    if (generation === generationFor(projectDirectory)) {
      worktreeCache.set(projectDirectory, { value: worktrees, at: Date.now() });
      return worktrees;
    }
  }
};

/** Lists worktrees through the daemon, with TTL caching and in-flight dedupe. */
export async function listProjectWorktrees(
  config: DaemonConfig,
  project: ProjectRef,
): Promise<WorktreeMetadata[]> {
  const projectDirectory = normalizePath(project.path);
  const cached = worktreeCache.get(projectDirectory);
  if (cached && Date.now() - cached.at < CACHE_TTL_MS) return cached.value;

  const inflight = worktreeInflight.get(projectDirectory);
  if (inflight) return inflight;

  const promise = readStableProjectWorktrees(config, projectDirectory).finally(() => {
    if (worktreeInflight.get(projectDirectory) === promise) worktreeInflight.delete(projectDirectory);
  });
  worktreeInflight.set(projectDirectory, promise);
  return promise;
}

/** Creates a worktree and returns only the daemon-confirmed path/branch projection. */
export async function createWorktree(
  config: DaemonConfig,
  project: ProjectRef,
  args: CreateWorktreeArgs,
): Promise<WorktreeMetadata> {
  const projectDirectory = normalizePath(project.path);
  const branch = args.branch.trim();
  if (!branch) throw new Error("Worktree branch is required");

  const result = await createDaemonWorktree(config, {
    workspaceRoot: projectDirectory,
    branch,
    base: args.base?.trim() || undefined,
    path: args.path?.trim() || undefined,
    force: args.force === true,
  });
  if (result.exitCode !== 0 || !result.path) {
    throw new Error(result.stderr || result.stdout || "Worktree creation failed");
  }

  invalidateWorktreeList(projectDirectory);
  const metadata = toMetadata(
    { path: result.path, branch: result.branch || branch, head: "created" },
    projectDirectory,
    "created-for-session",
  );
  if (!metadata) throw new Error("Daemon returned an invalid worktree path");
  return metadata;
}

/** Removes a worktree through the daemon; remote/local branch deletion is not a desktop concern. */
export async function removeProjectWorktree(
  config: DaemonConfig,
  project: ProjectRef,
  worktree: WorktreeMetadata,
  options: RemoveWorktreeOptions = {},
): Promise<void> {
  const projectDirectory = normalizePath(project.path);
  const result = await removeDaemonWorktree(config, {
    workspaceRoot: projectDirectory,
    path: worktree.path,
    force: options.force === true,
  });
  if (result.exitCode !== 0) {
    throw new Error(result.stderr || result.stdout || "Worktree removal failed");
  }
  invalidateWorktreeList(projectDirectory);
}

/** Compares discovery-owned fields rather than object identity. */
export const worktreeMapsEqual = (
  a: Map<string, WorktreeMetadata[]>,
  b: Map<string, WorktreeMetadata[]>,
): boolean => {
  if (a.size !== b.size) return false;
  for (const [key, value] of a) {
    const existing = b.get(key);
    if (!existing || existing.length !== value.length) return false;
    for (let index = 0; index < value.length; index += 1) {
      const next = value[index];
      const current = existing[index];
      if (
        next.path !== current.path ||
        next.branch !== current.branch ||
        next.name !== current.name ||
        next.label !== current.label ||
        next.projectDirectory !== current.projectDirectory ||
        next.worktreeRoot !== current.worktreeRoot ||
        next.headState !== current.headState ||
        next.worktreeSource !== current.worktreeSource
      ) return false;
    }
  }
  return true;
};

/** Keeps one shared Git topology under the first configured project. */
export const partitionWorktreesByRegisteredProject = (
  projects: ReadonlyArray<Pick<ProjectRef, "path">>,
  worktreesByProject: ReadonlyMap<string, WorktreeMetadata[]>,
): Map<string, WorktreeMetadata[]> => {
  const order = new Map<string, number>();
  projects.forEach((project, index) => {
    const path = normalizePath(project.path);
    if (path && !order.has(path)) order.set(path, index);
  });

  const sources = new Map<string, Array<{
    projectPath: string;
    worktrees: WorktreeMetadata[];
    projectIndex: number;
  }>>();
  for (const [rawPath, worktrees] of worktreesByProject) {
    const projectPath = normalizePath(rawPath);
    const projectIndex = order.get(projectPath);
    if (!projectPath || projectIndex === undefined || worktrees.length === 0) continue;
    const repositoryRoot = normalizePath(
      worktrees.find((worktree) => worktree.projectDirectory)?.projectDirectory || projectPath,
    );
    const entries = sources.get(repositoryRoot) ?? [];
    entries.push({ projectPath, worktrees, projectIndex });
    sources.set(repositoryRoot, entries);
  }

  const result = new Map<string, WorktreeMetadata[]>();
  for (const [repositoryRoot, repositorySources] of sources) {
    repositorySources.sort((a, b) => a.projectIndex - b.projectIndex || a.projectPath.localeCompare(b.projectPath));
    const first = repositorySources[0];
    if (!first) continue;
    const owner = order.has(repositoryRoot) ? repositoryRoot : first.projectPath;
    const topology = repositorySources.find((source) => source.projectPath === owner) ?? first;
    const seen = new Set<string>();
    const owned = topology.worktrees.filter((worktree) => {
      const path = normalizePath(worktree.path);
      if (!path || order.has(path) || seen.has(path)) return false;
      seen.add(path);
      return true;
    });
    if (owned.length > 0) result.set(owner, owned);
  }
  return result;
};

export function parseWorktreeList(stdout: string): WorktreeMetadata[] {
  return parsePorcelainWorktrees(stdout)
    .map((entry) => toMetadata(entry, "", "existing"))
    .filter((entry): entry is WorktreeMetadata => entry !== null);
}
