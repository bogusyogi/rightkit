// Encrypted-PDF unlock flow: bridges pdf.js `onPassword` to the host's optional `onPassword`
// callback. No React or DOM imports so it can be unit-tested directly.

/** Why a password is being requested: `need` = the document is encrypted and none was tried yet;
 *  `incorrect` = the previously supplied one was wrong. */
export type PdfPasswordReason = "need" | "incorrect";

/** Return the password to try, or null/undefined to give up (the viewer then reports a
 *  `PdfPasswordCancelledError` through `onError` and shows no document). May be async. */
export type PdfPasswordHandler = (
  reason: PdfPasswordReason,
  attempt: number,
) => string | null | undefined | Promise<string | null | undefined>;

/** Reported through `onError` when `onPassword` declined (or threw / exceeded the attempt cap). */
export class PdfPasswordCancelledError extends Error {
  readonly reason: PdfPasswordReason;
  constructor(reason: PdfPasswordReason, message?: string) {
    super(message ?? (reason === "need" ? "PDF password required" : "PDF password incorrect"));
    this.name = "PdfPasswordCancelledError";
    this.reason = reason;
  }
}

/** Upper bound on prompts for one load, so a handler that keeps answering wrongly still ends. */
export const MAX_PASSWORD_ATTEMPTS = 10;

/** pdf.js `PasswordResponses`: 1 = NEED_PASSWORD, 2 = INCORRECT_PASSWORD. */
export function passwordReason(code: number): PdfPasswordReason {
  return code === 2 ? "incorrect" : "need";
}

export interface PasswordTask {
  onPassword: unknown;
  destroy(): unknown;
}

export interface PasswordFlow {
  /** Set when the flow ended the load itself (declined, threw, or attempt cap). */
  failure: Error | null;
}

export interface LoadTimer {
  /** Rejects with the message after `ms` of un-paused time. */
  promise: Promise<never>;
  pause(): void;
  resume(): void;
  clear(): void;
}

/** A timeout whose clock stops while the user is being asked for a password. */
export function createLoadTimer(ms: number, message: string): LoadTimer {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let reject: (error: Error) => void = () => {};
  const promise = new Promise<never>((_, rej) => { reject = rej; });
  promise.catch(() => {});
  const start = () => { timer = setTimeout(() => reject(new Error(message)), ms); };
  const stop = () => { if (timer !== undefined) clearTimeout(timer); timer = undefined; };
  start();
  return { promise, pause: stop, resume: () => { stop(); start(); }, clear: stop };
}

/** Install `task.onPassword`. The handler is read lazily through `getHandler`, so a re-render with
 *  a new callback never reloads the document. */
export function attachPasswordHandler(
  task: PasswordTask,
  getHandler: () => PdfPasswordHandler | undefined,
  timer?: Pick<LoadTimer, "pause" | "resume">,
): PasswordFlow {
  const flow: PasswordFlow = { failure: null };
  let attempts = 0;
  task.onPassword = (update: (value: string) => void, code: number) => {
    const reason = passwordReason(code);
    const handler = getHandler();
    const giveUp = (error: Error) => { flow.failure = error; timer?.resume(); void task.destroy(); };
    if (!handler) return giveUp(new PdfPasswordCancelledError(reason));
    attempts += 1;
    if (attempts > MAX_PASSWORD_ATTEMPTS) {
      return giveUp(new PdfPasswordCancelledError(reason, "PDF password attempts exhausted"));
    }
    timer?.pause();
    void (async () => {
      try {
        const value = await handler(reason, attempts);
        if (typeof value !== "string") return giveUp(new PdfPasswordCancelledError(reason));
        timer?.resume();
        update(value);
      } catch (cause) {
        giveUp(cause instanceof Error ? cause : new Error(String(cause)));
      }
    })();
  };
  return flow;
}
