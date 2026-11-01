/**
 * Generation-gated state for one daemon connection bootstrap.
 *
 * This is a direct renderer-side port of Hermes Agent's
 * `backend-connection-state.ts`. A generation changes whenever the active
 * connection is invalidated, so late probes and process exits cannot publish
 * state into a newer attempt.
 */
export type ConnectionAttempt<TConnection> = {
  generation: number;
  promise: Promise<TConnection> | null;
};

export type ConnectionProcessOwner<TProcess> = {
  generation: number;
  process: TProcess;
};

export function createConnectionAttemptState<TProcess, TConnection>() {
  let generation = 0;
  let process: TProcess | null = null;
  let promise: Promise<TConnection> | null = null;

  return {
    startAttempt(): ConnectionAttempt<TConnection> {
      return { generation, promise: null };
    },

    setPromise(
      attempt: ConnectionAttempt<TConnection>,
      nextPromise: Promise<TConnection>,
    ): boolean {
      if (attempt.generation !== generation) return false;

      attempt.promise = nextPromise;
      promise = nextPromise;
      return true;
    },

    isCurrentAttempt(attempt: ConnectionAttempt<TConnection>): boolean {
      return attempt.generation === generation;
    },

    attachProcess(
      attempt: ConnectionAttempt<TConnection>,
      nextProcess: TProcess,
    ): ConnectionProcessOwner<TProcess> | null {
      if (attempt.generation !== generation) return null;

      process = nextProcess;
      return { generation, process: nextProcess };
    },

    clearForCurrentProcess(owner: ConnectionProcessOwner<TProcess>): boolean {
      if (owner.generation !== generation || owner.process !== process) return false;

      process = null;
      promise = null;
      return true;
    },

    clearPromiseForAttempt(attempt: ConnectionAttempt<TConnection>): boolean {
      if (
        attempt.generation !== generation ||
        (promise !== null && attempt.promise !== promise)
      ) {
        return false;
      }

      promise = null;
      return true;
    },

    getProcess(): TProcess | null {
      return process;
    },

    getPromise(): Promise<TConnection> | null {
      return promise;
    },

    invalidate(): TProcess | null {
      const currentProcess = process;

      generation += 1;
      process = null;
      promise = null;
      return currentProcess;
    },
  };
}

/** Compatibility name retained for callers ported from the Hermes donor. */
export const createBackendConnectionState = createConnectionAttemptState;
export type BackendConnectionAttempt<TConnection> = ConnectionAttempt<TConnection>;
export type BackendProcessOwner<TProcess> = ConnectionProcessOwner<TProcess>;
