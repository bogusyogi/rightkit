/**
 * Renderer-only Ctrl-Tab task switcher state.
 *
 * Ported from Waku's `TaskSwitcherUi`: opening snapshots order, repeated
 * presses move only the highlight, and commit happens once on modifier
 * release. The selected task is the only seed for a cold switcher, so opening
 * old windows cannot manufacture recency.
 */
export const CARD_WIDTH = 194;
export const CARD_HEIGHT = 169;
export const GRID_INSET = 8;
export const WINDOW_MARGIN = 44;
export const MAX_COLUMNS = 5;
export const MAX_ROWS = 2;
export const MAX_TASKS = MAX_COLUMNS * MAX_ROWS;

export type TaskSwitcherStatus = "idle" | "running" | "waiting" | "failed" | string;

export interface TaskSwitcherSession {
  id: string;
  title: string;
  model?: string | null;
  workspaceRoot?: string;
  status?: TaskSwitcherStatus;
  hasHistory?: boolean;
}

export interface TaskSwitcherSnapshot {
  open: boolean;
  orderedSessionIds: string[];
  highlightedSessionId: string | null;
  originalSessionId: string | null;
  columns: number;
}

export function orderedTaskIds(
  current: string | null,
  recent: readonly string[],
  startedTasks: readonly string[],
): string[] {
  const valid = new Set(startedTasks);
  const seen = new Set<string>();
  const ordered: string[] = [];
  const push = (id: string) => {
    if (ordered.length < MAX_TASKS && valid.has(id) && !seen.has(id)) {
      seen.add(id);
      ordered.push(id);
    }
  };

  if (current) push(current);
  for (const id of recent) push(id);
  return ordered;
}

export function initialHighlightIndex(
  ordered: readonly string[],
  current: string | null,
  reverse: boolean,
): number | null {
  if (ordered.length === 0) return null;
  if (ordered[0] === current) {
    if (ordered.length === 1) return 0;
    return reverse ? ordered.length - 1 : 1;
  }
  return reverse ? ordered.length - 1 : 0;
}

export function taskSwitcherColumnCount(count: number, viewportWidth: number): number {
  const boundedCount = Math.min(Math.max(0, count), MAX_TASKS);
  const usable = Math.max(
    viewportWidth - WINDOW_MARGIN - GRID_INSET * 2,
    CARD_WIDTH,
  );
  const fitting = Math.floor(usable / CARD_WIDTH);
  const minimumForTwoRows = Math.ceil(boundedCount / MAX_ROWS);
  return Math.min(
    boundedCount,
    Math.max(fitting, minimumForTwoRows, 1),
    MAX_COLUMNS,
  );
}

export function taskSwitcherGridHeight(
  count: number,
  columns: number,
  viewportHeight: number,
): number {
  const rows = Math.ceil(Math.max(0, count) / Math.max(1, columns));
  const contentHeight = rows * CARD_HEIGHT + GRID_INSET * 2;
  return Math.min(
    contentHeight,
    Math.max(viewportHeight - WINDOW_MARGIN, CARD_HEIGHT + GRID_INSET * 2),
  );
}

export function taskSwitcherTitle(session: Pick<TaskSwitcherSession, "title">): string {
  return session.title.trim().toLowerCase() === "new session"
    ? "New task"
    : session.title || "Untitled session";
}

export function taskSwitcherStatusLabel(status: TaskSwitcherStatus | undefined): string {
  switch (status) {
    case "running":
      return "Working";
    case "waiting":
      return "Waiting for input";
    case "failed":
      return "Failed";
    case "idle":
      return "Idle";
    default:
      return status || "Unknown status";
  }
}

/** the host app stored-row equivalent of Waku's has_started predicate. */
export function taskHasStarted(session: TaskSwitcherSession): boolean {
  return session.hasHistory === true || session.status !== "dormant";
}

export class TaskSwitcherUi {
  private openState = false;
  private orderedIds: string[] = [];
  private highlightedId: string | null = null;
  private originalId: string | null = null;
  private recentIds: string[] = [];
  private columnsState = 1;

  get isOpen(): boolean {
    return this.openState;
  }

  get snapshot(): TaskSwitcherSnapshot {
    return {
      open: this.openState,
      orderedSessionIds: [...this.orderedIds],
      highlightedSessionId: this.highlightedId,
      originalSessionId: this.originalId,
      columns: this.columnsState,
    };
  }

  recordAccess(sessionId: string): void {
    this.recentIds = [sessionId, ...this.recentIds.filter((id) => id !== sessionId)];
  }

  remove(sessionId: string): void {
    const removedIndex = this.orderedIds.indexOf(sessionId);
    this.recentIds = this.recentIds.filter((id) => id !== sessionId);
    this.orderedIds = this.orderedIds.filter((id) => id !== sessionId);
    if (this.highlightedId === sessionId) {
      const nextIndex = Math.min(
        Math.max(0, removedIndex),
        Math.max(0, this.orderedIds.length - 1),
      );
      this.highlightedId = this.orderedIds[nextIndex] ?? null;
    }
  }

  openSwitcher(
    currentSessionId: string | null,
    startedSessionIds: readonly string[],
    reverse = false,
    viewportWidth = 1024,
  ): boolean {
    const ordered = orderedTaskIds(currentSessionId, this.recentIds, startedSessionIds);
    const index = initialHighlightIndex(ordered, currentSessionId, reverse);
    if (index === null) return false;

    this.openState = true;
    this.orderedIds = ordered;
    this.highlightedId = ordered[index] ?? null;
    this.originalId = currentSessionId;
    this.columnsState = taskSwitcherColumnCount(ordered.length, viewportWidth);
    return true;
  }

  configureColumns(viewportWidth: number): void {
    this.columnsState = taskSwitcherColumnCount(this.orderedIds.length, viewportWidth);
  }

  setHighlight(index: number): void {
    const id = this.orderedIds[index];
    if (id) this.highlightedId = id;
  }

  highlightSession(sessionId: string): void {
    if (this.orderedIds.includes(sessionId)) this.highlightedId = sessionId;
  }

  cycle(reverse = false): void {
    if (!this.openState || this.orderedIds.length === 0) return;
    const current = this.orderedIds.indexOf(this.highlightedId ?? "");
    if (current < 0) return;
    const length = this.orderedIds.length;
    const next = reverse ? (current + length - 1) % length : (current + 1) % length;
    this.highlightedId = this.orderedIds[next] ?? null;
  }

  /** Dismiss and return the selected id only when the commit is still valid. */
  finish(
    pointerSelection: boolean,
    currentSessionId: string | null,
    startedSessionIds: readonly string[],
  ): string | null {
    if (!this.openState) return null;
    const selected = this.highlightedId;
    const mayCommit = pointerSelection || currentSessionId === this.originalId;
    const valid = selected !== null && startedSessionIds.includes(selected);
    this.dismiss();
    return mayCommit && valid ? selected : null;
  }

  dismiss(): void {
    this.openState = false;
    this.orderedIds = [];
    this.highlightedId = null;
    this.originalId = null;
    this.columnsState = 1;
  }
}
