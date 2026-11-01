import type { DisplayJob, HistoryRow } from "../contract.js";

export type DesktopSource =
  | { type: "queue"; id: string; job: DisplayJob; title: string; searchText: string }
  | { type: "saved"; id: string; row: HistoryRow; title: string; searchText: string };

export interface DesktopSourceGroups {
  active: DesktopSource[];
  recent: DesktopSource[];
  saved: DesktopSource[];
}

export type DesktopSourceFilter = "all" | "active" | "attention" | "saved";

const ACTIVE_TERMINAL = new Set(["done", "error", "cancelled"]);
type QueueSource = Extract<DesktopSource, { type: "queue" }>;
type SavedSource = Extract<DesktopSource, { type: "saved" }>;

export function buildDesktopSourceGroups(
  jobs: DisplayJob[],
  history: HistoryRow[],
  query = "",
  filter: DesktopSourceFilter = "all",
): DesktopSourceGroups {
  const normalized = query.trim().toLocaleLowerCase();
  const matches = (text: string) => !normalized || text.toLocaleLowerCase().includes(normalized);
  const historyById = new Map(history.map((row) => [row.id, row]));
  const queueSources: QueueSource[] = jobs.map((job) => {
    const title = job.title?.trim() || historyById.get(job.id)?.title?.trim() || job.input;
    const savedSnippet = historyById.get(job.id)?.snippet ?? "";
    return {
      type: "queue",
      id: job.id,
      job,
      title,
      searchText: `${title}\n${job.input}\n${job.msg ?? ""}\n${job.error ?? ""}\n${savedSnippet}`,
    };
  });
  const queueIds = new Set(jobs.map((job) => job.id));
  const savedSources: SavedSource[] = history.map((row) => {
    const title = row.title?.trim() || row.url;
    return {
      type: "saved",
      id: row.id,
      row,
      title,
      searchText: `${title}\n${row.url}\n${row.snippet ?? ""}`,
    };
  });
  const doneQueueById = new Map(
    queueSources
      .filter(({ job }) => job.stage === "done")
      .map((source) => [source.id, source] as const),
  );

  const visibleQueue = queueSources.filter(({ job, searchText }) => {
    if (!matches(searchText)) return false;
    if (filter === "saved") return false;
    if (filter === "active") return !ACTIVE_TERMINAL.has(job.stage);
    if (filter === "attention") return job.stage === "error" || Boolean(job.needs_auth);
    return true;
  });
  const visibleSaved: DesktopSource[] = savedSources
    .filter(({ id }) => filter === "saved" || !queueIds.has(id))
    .map((source) => (filter === "saved" ? (doneQueueById.get(source.id) ?? source) : source))
    .filter(({ searchText }) => matches(searchText) && (filter === "all" || filter === "saved"));

  return {
    active: visibleQueue.filter(
      (source): source is QueueSource =>
        source.type === "queue" && !ACTIVE_TERMINAL.has(source.job.stage),
    ),
    recent: visibleQueue.filter(
      (source): source is QueueSource =>
        source.type === "queue" && ACTIVE_TERMINAL.has(source.job.stage),
    ),
    saved: visibleSaved.filter(
      (source, index, list) => list.findIndex((candidate) => candidate.id === source.id) === index,
    ),
  };
}
