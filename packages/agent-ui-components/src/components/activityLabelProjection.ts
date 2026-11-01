// Presentation-only activity labels, ported from Waku's components.rs.
// Runtime/event ownership stays in the daemon and transcript projection.

export type ActivityKind =
  | "reasoning"
  | "command"
  | "file-change"
  | "file-read"
  | "file-search"
  | "file-list"
  | "search"
  | "plan"
  | "tool";

export interface ActivityFileChange {
  path?: string;
  filePath?: string;
  name?: string;
  additions?: number | null;
  deletions?: number | null;
  diff?: string | null;
}

export interface ActivityItem {
  kind: ActivityKind;
  title: string;
  complete: boolean;
  failed?: boolean;
  displayTarget?: string | null;
  displayDescription?: string | null;
  detail?: string | null;
  output?: string | null;
  arguments?: string | null;
  imageUrls?: readonly string[];
  fileChanges?: readonly ActivityFileChange[];
  reasoning?: string | null;
}

const GENERIC_TITLES: Record<ActivityKind, readonly string[]> = {
  reasoning: [],
  command: ["bash", "shell", "command", "run_command"],
  "file-change": ["edit_file", "multi_edit", "write_file", "apply_patch"],
  "file-read": ["read", "read_file"],
  "file-search": ["grep", "search_files", "file_search"],
  "file-list": ["ls", "list_files", "file_list"],
  search: ["web_search", "search"],
  plan: ["update_plan", "plan"],
  tool: ["tool"],
};

function argumentRecord(activity: ActivityItem): Record<string, unknown> | null {
  if (!activity.arguments?.trim()) return null;
  try {
    const parsed: unknown = JSON.parse(activity.arguments);
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? (parsed as Record<string, unknown>)
      : null;
  } catch {
    return null;
  }
}

function argumentString(activity: ActivityItem, ...keys: string[]): string | null {
  const args = argumentRecord(activity);
  for (const key of keys) {
    const value = args?.[key];
    if (typeof value === "string" && value.trim()) return value.trim();
  }
  return null;
}

function fileChangeName(change: ActivityFileChange): string {
  const candidate = change.name ?? change.filePath ?? change.path ?? "";
  const normalized = candidate.replaceAll("\\", "/").replace(/\/$/, "");
  return normalized.split("/").at(-1) || candidate;
}

export function isGenericActivityTitle(kind: ActivityKind, title: string): boolean {
  const normalized = title.trim().toLowerCase();
  return GENERIC_TITLES[kind].some((candidate) => candidate === normalized);
}

export function toolNameLeaf(name: string): string {
  const trimmed = name.trim();
  const namespaceLeaf = trimmed.split("__").at(-1) ?? trimmed;
  return namespaceLeaf.split(/[:./]/).at(-1) ?? namespaceLeaf;
}

export function isAskUserQuestion(activity: ActivityItem): boolean {
  if (activity.kind !== "tool") return false;
  return toolNameLeaf(activity.title)
    .replace(/[\s_-]/g, "")
    .toLowerCase() === "askuserquestion";
}

export function humanizeToolName(name: string): string {
  const leaf = toolNameLeaf(name);
  if (!leaf.trim()) return "Tool";
  if (/\s/.test(leaf)) return leaf.trim();
  const withWordBreaks = leaf
    .replace(/[_-]+/g, " ")
    .replace(/([a-z\d])([A-Z])/g, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .trim();
  return withWordBreaks ? withWordBreaks[0].toUpperCase() + withWordBreaks.slice(1) : "Tool";
}

function activityToolDisplayName(activity: ActivityItem): string {
  if (isAskUserQuestion(activity)) return "Ask questions";
  const target = activity.displayTarget?.trim() || argumentString(activity, "title", "target");
  if (target) return target;
  if (!isGenericActivityTitle(activity.kind, activity.title)) return humanizeToolName(activity.title);
  return "Tool";
}

function activityPathName(path: string): string {
  const normalized = path.replaceAll("\\", "/").replace(/\/$/, "");
  return normalized.split("/").at(-1) || path;
}

function derivedFileChanges(activity: ActivityItem): ActivityFileChange[] {
  if (activity.fileChanges?.length) return [...activity.fileChanges];
  const patch = argumentString(activity, "patch")?.replaceAll("\\n", "\n");
  if (!patch) return [];
  const paths = [...patch.matchAll(/^\*\*\* (?:Update|Add|Delete) File: (.+)$/gm)].map(
    (match) => match[1].trim(),
  );
  const diffPaths = [...patch.matchAll(/^diff --git a\/(.+?) b\/(.+)$/gm)].map(
    (match) => match[2].trim(),
  );
  const names = paths.length ? paths : diffPaths;
  if (!names.length) return [];
  return names.map((path) => ({ path }));
}

function patchStats(activity: ActivityItem): { additions: number; deletions: number } | null {
  const patch = argumentString(activity, "patch")?.replaceAll("\\n", "\n");
  if (!patch) return null;
  let additions = 0;
  let deletions = 0;
  for (const line of patch.split("\n")) {
    if (line.startsWith("+++") || line.startsWith("---")) continue;
    if (line.startsWith("+")) additions += 1;
    if (line.startsWith("-")) deletions += 1;
  }
  return { additions, deletions };
}

function fileSubject(activity: ActivityItem): string | null {
  const changes = derivedFileChanges(activity);
  if (changes.length === 1) return fileChangeName(changes[0]);
  if (changes.length > 1) return `${changes.length} files`;
  const target = activity.displayTarget?.trim() || argumentString(activity, "filePath", "path", "notebook_path");
  return target ? activityPathName(target) : null;
}

export function activityDisplayTitle(activity: ActivityItem): string {
  const failed = activity.failed === true;
  const state = (running: string, done: string, error: string) =>
    activity.complete ? (failed ? error : done) : running;
  switch (activity.kind) {
    case "file-change": {
      const subject = fileSubject(activity);
      if (!subject && !isGenericActivityTitle(activity.kind, activity.title)) return activity.title;
      return subject
        ? state(`Editing ${subject}`, `Edited ${subject}`, `Failed to edit ${subject}`)
        : state("Editing files", "Edited files", "Failed to edit files");
    }
    case "file-read": {
      const file = fileSubject(activity);
      if (!file && !isGenericActivityTitle(activity.kind, activity.title)) return activity.title;
      return file ? state(`Reading ${file}`, `Read ${file}`, `Failed to read ${file}`) : state("Reading file", "Read file", "Failed to read file");
    }
    case "file-search": {
      const query = activity.displayTarget?.trim() || argumentString(activity, "pattern", "query");
      if (!query && !isGenericActivityTitle(activity.kind, activity.title)) return activity.title;
      return query ? state(`Searching files for ${query}`, `Searched files for ${query}`, `Failed searching files for ${query}`) : state("Searching files", "Searched files", "Failed searching files");
    }
    case "file-list": {
      const directory = activity.displayTarget?.trim() || argumentString(activity, "path", "directory");
      if (!directory && !isGenericActivityTitle(activity.kind, activity.title)) return activity.title;
      const name = directory ? activityPathName(directory) : null;
      return name ? state(`Listing files in ${name}`, `Listed files in ${name}`, `Failed listing files in ${name}`) : state("Listing files", "Listed files", "Failed listing files");
    }
    case "command": {
      const description = activity.displayDescription?.trim() || argumentString(activity, "description");
      const command = activity.displayTarget?.trim() || argumentString(activity, "command");
      if (description) return state(`Running command: ${description}`, `Ran command: ${description}`, `Command failed: ${description}`);
      if (command) return state(`Running ${command}`, `Ran ${command}`, `Failed: ${command}`);
      if (!isGenericActivityTitle(activity.kind, activity.title)) return activity.title;
      return state("Running command", "Ran command", "Command failed");
    }
    case "search": {
      const query = activity.displayTarget?.trim() || argumentString(activity, "query");
      if (query) return state(`Searching the web for ${query}`, `Searched the web for ${query}`, `Web search failed for ${query}`);
      if (isGenericActivityTitle(activity.kind, activity.title)) return state("Searching the web", "Searched the web", "Web search failed");
      return activity.title;
    }
    case "plan":
      return isGenericActivityTitle(activity.kind, activity.title)
        ? state("Updating plan", "Updated plan", "Plan update failed")
        : activity.title;
    case "tool": return activityToolDisplayName(activity);
    case "reasoning": return activity.title;
  }
}

export function activityActionLabel(activity: ActivityItem): string {
  if (activity.kind === "tool" && isAskUserQuestion(activity)) return "Ask questions";
  const labels: Record<ActivityKind, string> = {
    reasoning: "Think", command: "Run", "file-change": "Edit", "file-read": "Read",
    "file-search": "Search", search: "Search", "file-list": "List", plan: "Plan", tool: "Tool",
  };
  return labels[activity.kind];
}

export function activityRowDetail(activity: ActivityItem): string {
  const custom = !isGenericActivityTitle(activity.kind, activity.title) ? activity.title : "";
  switch (activity.kind) {
    case "reasoning": return activity.reasoning || activity.title;
    case "command": return activity.displayDescription?.trim() || activity.displayTarget?.trim() || argumentString(activity, "description", "command") || custom;
    case "file-change": {
      const changes = derivedFileChanges(activity);
      return changes.length === 1 ? fileChangeName(changes[0]) : changes.length > 1 ? `${changes.length} files` : custom;
    }
    case "file-read":
    case "file-list": return activity.displayTarget?.trim() ? activityPathName(activity.displayTarget) : argumentString(activity, "filePath", "path") ? activityPathName(argumentString(activity, "filePath", "path") as string) : custom;
    case "file-search": return activityDisplayTitle(activity);
    case "search": { const query = activity.displayTarget?.trim() || argumentString(activity, "query"); return query ? `Search for ${query}` : custom; }
    case "plan": return custom;
    case "tool": return isAskUserQuestion(activity) ? "" : (activity.displayTarget?.trim() || (!isGenericActivityTitle(activity.kind, activity.title) ? activityToolDisplayName(activity) : ""));
  }
}

export function activityFileChangeStats(activity: ActivityItem): { additions: number; deletions: number } | null {
  if (activity.kind !== "file-change" || !activity.complete || activity.failed) return null;
  const changes = derivedFileChanges(activity);
  if (!changes.length) return null;
  if (changes.every((change) => typeof change.additions === "number" && typeof change.deletions === "number")) {
    return changes.reduce<{ additions: number; deletions: number }>((stats, change) => ({
      additions: stats.additions + (change.additions ?? 0),
      deletions: stats.deletions + (change.deletions ?? 0),
    }), { additions: 0, deletions: 0 });
  }
  return patchStats(activity);
}
