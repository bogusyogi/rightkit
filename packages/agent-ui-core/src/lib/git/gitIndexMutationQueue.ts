export type GitIndexMutationDirection = "stage" | "unstage";

export type QueuedGitIndexMutation = {
  directory: string;
  direction: GitIndexMutationDirection;
  paths: Set<string>;
  rollback?: () => void;
};

export type GitIndexMutationSnapshot = {
  directory: string;
  direction: GitIndexMutationDirection;
  paths: string[];
  rollback?: () => void;
};

type GitIndexMutationQueueOptions = {
  runMutation: (mutation: GitIndexMutationSnapshot) => Promise<void>;
  onMutationComplete: (mutation: GitIndexMutationSnapshot) => void;
  onMutationError: (mutation: GitIndexMutationSnapshot, error: unknown) => void;
  onPathsComplete: (paths: string[]) => void;
  scheduleFlush: () => void;
};

export type GitIndexMutationQueue = {
  enqueue: (mutation: QueuedGitIndexMutation) => void;
  flush: () => void;
  clear: () => void;
  size: () => number;
  isRunning: () => boolean;
};

/**
 * Serializes daemon-owned Git index mutations. Consecutive operations with
 * the same direction are coalesced, while alternating stage/unstage calls
 * retain order. Rollback is deliberately a callback so the projection owns
 * its snapshot semantics rather than this queue guessing UI state.
 */
export const createGitIndexMutationQueue = ({
  runMutation,
  onMutationComplete,
  onMutationError,
  onPathsComplete,
  scheduleFlush,
}: GitIndexMutationQueueOptions): GitIndexMutationQueue => {
  const queuedMutations: QueuedGitIndexMutation[] = [];
  let running = false;

  const flush = (): void => {
    if (running) return;
    const nextMutation = queuedMutations.shift();
    if (!nextMutation) return;

    running = true;
    const snapshot: GitIndexMutationSnapshot = {
      directory: nextMutation.directory,
      direction: nextMutation.direction,
      paths: Array.from(nextMutation.paths),
      rollback: nextMutation.rollback,
    };

    void (async () => {
      try {
        await runMutation(snapshot);
        onMutationComplete(snapshot);
      } catch (error) {
        onMutationError(snapshot, error);
      } finally {
        onPathsComplete(snapshot.paths);
        running = false;
        if (queuedMutations.length > 0) scheduleFlush();
      }
    })();
  };

  return {
    enqueue: (mutation) => {
      const lastMutation = queuedMutations[queuedMutations.length - 1];
      if (lastMutation?.directory === mutation.directory && lastMutation.direction === mutation.direction) {
        mutation.paths.forEach((path) => {
          lastMutation.paths.add(path);
        });
        return;
      }
      queuedMutations.push(mutation);
    },
    flush,
    clear: () => {
      queuedMutations.length = 0;
    },
    size: () => queuedMutations.length,
    isRunning: () => running,
  };
};
