import { describe, expect, it } from "vitest";
import {
  createGitIndexMutationQueue,
  type GitIndexMutationDirection,
} from "./gitIndexMutationQueue";

type Deferred<T> = {
  promise: Promise<T>;
  resolve: (value: T) => void;
  reject: (error: unknown) => void;
};

const deferred = <T>(): Deferred<T> => {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
};

const tick = async (): Promise<void> => {
  await Promise.resolve();
};

describe("createGitIndexMutationQueue", () => {
  it("coalesces consecutive mutations with the same directory and direction", async () => {
    const calls: Array<{ direction: GitIndexMutationDirection; paths: string[] }> = [];
    let queue!: ReturnType<typeof createGitIndexMutationQueue>;
    queue = createGitIndexMutationQueue({
      runMutation: async ({ direction, paths }) => { calls.push({ direction, paths }); },
      onMutationComplete: () => {},
      onMutationError: () => {},
      onPathsComplete: () => {},
      scheduleFlush: () => queue.flush(),
    });

    queue.enqueue({ directory: "/repo", direction: "stage", paths: new Set(["a.ts"]) });
    queue.enqueue({ directory: "/repo", direction: "stage", paths: new Set(["b.ts", "a.ts"]) });
    queue.flush();
    await tick();

    expect(calls).toEqual([{ direction: "stage", paths: ["a.ts", "b.ts"] }]);
  });

  it("serializes alternating directions without losing order", async () => {
    const first = deferred<void>();
    const calls: Array<{ direction: GitIndexMutationDirection; paths: string[] }> = [];
    let count = 0;
    let queue!: ReturnType<typeof createGitIndexMutationQueue>;
    queue = createGitIndexMutationQueue({
      runMutation: ({ direction, paths }) => {
        calls.push({ direction, paths });
        count += 1;
        return count === 1 ? first.promise : Promise.resolve();
      },
      onMutationComplete: () => {},
      onMutationError: () => {},
      onPathsComplete: () => {},
      scheduleFlush: () => queue.flush(),
    });

    queue.enqueue({ directory: "/repo", direction: "stage", paths: new Set(["a.ts"]) });
    queue.enqueue({ directory: "/repo", direction: "unstage", paths: new Set(["a.ts"]) });
    queue.flush();
    queue.flush();
    await tick();
    expect(calls).toEqual([{ direction: "stage", paths: ["a.ts"] }]);
    expect(queue.isRunning()).toBe(true);

    first.resolve();
    await tick();
    await tick();
    expect(calls).toEqual([
      { direction: "stage", paths: ["a.ts"] },
      { direction: "unstage", paths: ["a.ts"] },
    ]);
  });

  it("reports errors, completes paths, and continues", async () => {
    const errors: unknown[] = [];
    const completed: string[][] = [];
    let count = 0;
    let queue!: ReturnType<typeof createGitIndexMutationQueue>;
    queue = createGitIndexMutationQueue({
      runMutation: async ({ direction }) => {
        count += 1;
        if (count === 1) throw new Error(`${direction} failed`);
      },
      onMutationComplete: () => {},
      onMutationError: (_mutation, error) => errors.push(error),
      onPathsComplete: (paths) => completed.push(paths),
      scheduleFlush: () => queue.flush(),
    });

    queue.enqueue({ directory: "/repo", direction: "stage", paths: new Set(["a.ts"]) });
    queue.enqueue({ directory: "/repo", direction: "unstage", paths: new Set(["b.ts"]) });
    queue.flush();
    await tick();
    await tick();

    expect(errors).toHaveLength(1);
    expect(completed).toEqual([["a.ts"], ["b.ts"]]);
  });

  it("passes the rollback callback to the error handler", async () => {
    let rolledBack = false;
    let queue!: ReturnType<typeof createGitIndexMutationQueue>;
    queue = createGitIndexMutationQueue({
      runMutation: async () => { throw new Error("stage failed"); },
      onMutationComplete: () => {},
      onMutationError: (mutation) => mutation.rollback?.(),
      onPathsComplete: () => {},
      scheduleFlush: () => queue.flush(),
    });
    queue.enqueue({
      directory: "/repo",
      direction: "stage",
      paths: new Set(["a.ts"]),
      rollback: () => { rolledBack = true; },
    });
    queue.flush();
    await tick();
    expect(rolledBack).toBe(true);
  });
});
