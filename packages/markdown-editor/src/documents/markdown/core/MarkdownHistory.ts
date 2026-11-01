/**
 * Shared undo/redo history for the Markdown architecture v2 pipeline.
 *
 * Design principles:
 *
 *  - Each undo step is a {@link MarkdownHistoryGroup} containing one
 *    or more {@link MarkdownAppliedCommand}s. Commands are stored
 *    separately; groups are never composed into a single patch.
 *  - Coalescing is opt-in via {@link MarkdownAppliedCommand.coalesceKey}
 *    and respects origin, label, time window, and large-edit guards.
 *  - Redo is cleared on any new ordinary command.
 *  - Eviction drops the oldest complete groups first; groups are never
 *    split.
 *
 * The history model does not own the document; the buffer does. The
 * history only records command metadata; undo/redo is performed by
 * the caller applying the recorded inverse/forward resolved edits.
 */

import {
  DEFAULT_MARKDOWN_HISTORY_LIMITS,
  type MarkdownAppliedCommand,
  type MarkdownHistoryGroup,
  type MarkdownHistoryLabel,
  type MarkdownHistoryLimits,
} from "./types";

/**
 * Default per-group size guard for coalescing. Commands containing
 * edits that span more than this many code units never coalesce.
 */
const COALESCE_LARGE_EDIT_THRESHOLD = 4 * 1024;

/**
 * The shared history. Construct one per document mount; clear on
 * document replacement.
 */
export class MarkdownHistory {
  private readonly limits: MarkdownHistoryLimits;
  private undoStack: MarkdownHistoryGroup[] = [];
  private redoStack: MarkdownHistoryGroup[] = [];
  private changedCodeUnits = 0;

  constructor(limits: MarkdownHistoryLimits = DEFAULT_MARKDOWN_HISTORY_LIMITS) {
    this.limits = limits;
  }

  get canUndo(): boolean {
    return this.undoStack.length > 0;
  }

  get canRedo(): boolean {
    return this.redoStack.length > 0;
  }

  /**
   * Records a command. May merge it with the trailing undo group
   * when all coalescing rules are satisfied. Evicts oldest groups
   * when the limits are exceeded.
   */
  push(command: MarkdownAppliedCommand): void {
    const changed = countChangedCodeUnits(command);
    const last = this.undoStack[this.undoStack.length - 1];
    if (last && canCoalesce(last, command, this.limits.coalesceWindowMs)) {
      this.undoStack[this.undoStack.length - 1] = extendGroup(last, command, changed);
    } else {
      const group: MarkdownHistoryGroup = {
        commands: [command],
        changedCodeUnits: changed,
        startedAtMs: command.timestampMs,
        endedAtMs: command.timestampMs,
      };
      this.undoStack.push(group);
    }
    this.changedCodeUnits += changed;
    // Redo stack is cleared by any new ordinary command.
    this.redoStack = [];
    this.runEviction();
  }

  private runEviction(): void {
    const result = evict(this.undoStack, this.redoStack, this.limits, this.changedCodeUnits);
    this.undoStack = result.stack;
    this.redoStack = result.other;
    this.changedCodeUnits = result.changedCodeUnits;
  }

  /**
   * Removes and returns the most recent undo group, or `null` if
   * the undo stack is empty. The caller is responsible for applying
   * the inverse edits in reverse order.
   */
  takeUndoGroup(): MarkdownHistoryGroup | null {
    const group = this.undoStack.pop() ?? null;
    if (group) {
      this.changedCodeUnits -= group.changedCodeUnits;
    }
    return group;
  }

  /**
   * Removes and returns the most recent redo group, or `null`. The
   * caller reapplies the forward edits in original order.
   */
  takeRedoGroup(): MarkdownHistoryGroup | null {
    const group = this.redoStack.pop() ?? null;
    if (group) {
      this.changedCodeUnits -= group.changedCodeUnits;
    }
    return group;
  }

  /**
   * Pushes a group that has just been undone onto the redo stack.
   */
  pushUndoneGroup(group: MarkdownHistoryGroup): void {
    this.redoStack.push(group);
    this.changedCodeUnits += group.changedCodeUnits;
    this.runEviction();
  }

  /**
   * Pushes a group that has just been redone onto the undo stack.
   */
  pushRedoneGroup(group: MarkdownHistoryGroup): void {
    this.undoStack.push(group);
    this.changedCodeUnits += group.changedCodeUnits;
    this.runEviction();
  }

  /**
   * Wipes both stacks. Called when the underlying document is
   * replaced or external mutations invalidate the history.
   */
  clear(): void {
    this.undoStack = [];
    this.redoStack = [];
    this.changedCodeUnits = 0;
  }

  /** Read-only access for tests. */
  get _undoStack(): readonly MarkdownHistoryGroup[] {
    return this.undoStack;
  }

  /** Read-only access for tests. */
  get _redoStack(): readonly MarkdownHistoryGroup[] {
    return this.redoStack;
  }
}

function canCoalesce(
  prev: MarkdownHistoryGroup,
  curr: MarkdownAppliedCommand,
  coalesceWindowMs: number,
): boolean {
  const prevCommand = prev.commands[prev.commands.length - 1];
  if (!prevCommand) return false;
  if (curr.isolateHistory || prevCommand.isolateHistory) return false;
  if (!curr.coalesceKey || curr.coalesceKey !== prevCommand.coalesceKey) {
    return false;
  }
  if (curr.origin !== prevCommand.origin) return false;
  if (curr.timestampMs - prev.endedAtMs > coalesceWindowMs) return false;
  if (curr.origin === "external") return false;
  const allowed: ReadonlySet<MarkdownHistoryLabel> = new Set(["Typing", "Delete"]);
  if (!allowed.has(curr.label) || !allowed.has(prevCommand.label)) return false;
  if (curr.label !== prevCommand.label) return false;
  if (hasLargeEdit(curr) || hasLargeEdit(prevCommand)) return false;
  return true;
}

function hasLargeEdit(command: MarkdownAppliedCommand): boolean {
  for (const edit of command.resolved) {
    if (edit.to - edit.from > COALESCE_LARGE_EDIT_THRESHOLD) return true;
  }
  for (const edit of command.inverse) {
    if (edit.to - edit.from > COALESCE_LARGE_EDIT_THRESHOLD) return true;
  }
  return false;
}

function extendGroup(
  prev: MarkdownHistoryGroup,
  curr: MarkdownAppliedCommand,
  changed: number,
): MarkdownHistoryGroup {
  return {
    commands: [...prev.commands, curr],
    changedCodeUnits: prev.changedCodeUnits + changed,
    startedAtMs: prev.startedAtMs,
    endedAtMs: curr.timestampMs,
  };
}

function countChangedCodeUnits(command: MarkdownAppliedCommand): number {
  let n = 0;
  for (const edit of command.resolved) {
    n += edit.to - edit.from + countLogicalNewlines(edit.insert);
  }
  for (const edit of command.inverse) {
    n += edit.to - edit.from + countLogicalNewlines(edit.insert);
  }
  return n;
}

function countLogicalNewlines(text: string): number {
  let count = 0;
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) === 10) count++;
  }
  return count;
}

/**
 * Evicts oldest complete groups when either limit is exceeded. Groups
 * are never split. If a single group exceeds the changed-code-unit
 * limit it is retained (the caller should ensure such a group is
 * never created).
 */
function evict(
  stack: MarkdownHistoryGroup[],
  other: MarkdownHistoryGroup[],
  limits: MarkdownHistoryLimits,
  changedCodeUnits: number,
): { stack: MarkdownHistoryGroup[]; other: MarkdownHistoryGroup[]; changedCodeUnits: number } {
  const working = stack;
  const otherStack = other;
  let total = changedCodeUnits;
  while (
    working.length > 0 &&
    (working.length > limits.maxGroups ||
      total - otherStack.reduce((acc, g) => acc + g.changedCodeUnits, 0) >
        limits.maxChangedCodeUnits)
  ) {
    const removed = working.shift();
    if (removed) total -= removed.changedCodeUnits;
  }
  return { stack: working, other: otherStack, changedCodeUnits: total };
}
