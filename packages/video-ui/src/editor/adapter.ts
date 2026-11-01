/**
 * Revision-bound edit session against the canonical Rust timeline.
 *
 * The frontend owns only transient gesture state. On gesture release the
 * session submits one transaction (one action, or a grouped list) stamped with
 * the revision cursor the UI last saw. Undo/redo always pass `expect_cursor`
 * (supported by the engine today). Edits pass `expect_cursor` only when the
 * host advertises `edit.expect_cursor`; otherwise the session verifies the
 * returned cursor advanced exactly as expected and reports a stale
 * acknowledgement when it did not. Stale outcomes surface through `onStale`
 * so the host refreshes `project.show` and rebuilds — never silently
 * re-applied.
 */
import type { CapabilityReport } from "./capabilities";
import { supports } from "./capabilities";
import type { EditAction } from "./actions";

export type RevisionInfo = {
  cursor: number;
  total: number;
  can_undo: boolean;
  can_redo: boolean;
};

export type EditResponse = {
  status?: string;
  seq?: number | null;
  revision?: RevisionInfo;
  /** undo/redo: false when expect_cursor showed the step already happened. */
  changed?: boolean;
};

export type EngineCall = (
  method: string,
  params: Record<string, unknown>,
) => Promise<unknown>;

export type EditError = { code?: string; message?: string };

export class StaleEditError extends Error {
  readonly expected: number;
  readonly actual: number | null;
  constructor(expected: number, actual: number | null) {
    super(
      `timeline changed underneath the edit (expected revision ${expected}${
        actual == null ? "" : `, engine is at ${actual}`
      })`,
    );
    this.name = "StaleEditError";
    this.expected = expected;
    this.actual = actual;
  }
}

export class EngineEditError extends Error {
  readonly code: string;
  constructor(code: string, message: string) {
    super(message);
    this.name = "EngineEditError";
    this.code = code;
  }
}

export function errorCode(e: unknown): string | undefined {
  if (e && typeof e === "object" && "code" in e) {
    return String((e as { code: unknown }).code);
  }
  return undefined;
}

export function errorText(e: unknown): string {
  if (e instanceof Error) return e.message;
  if (e && typeof e === "object" && "message" in e) {
    return String((e as { message: unknown }).message);
  }
  return String(e);
}

function asEditResponse(v: unknown): EditResponse {
  return v && typeof v === "object" ? (v as EditResponse) : {};
}

export type ApplyResult = {
  /** Actions recorded as new revisions (retries/no-ops excluded). */
  applied: number;
  cursor: number;
  /** True when at least one action was already applied (idempotent retry). */
  retried: boolean;
};

export class EditSession {
  private cursor: number;
  private inFlight = 0;

  constructor(
    private readonly dir: string,
    revisionCursor: number,
    private readonly caps: CapabilityReport | null,
    private readonly call: EngineCall,
    private readonly onStale?: (expected: number, actual: number | null) => void,
  ) {
    this.cursor = revisionCursor;
  }

  get revisionCursor() {
    return this.cursor;
  }

  get busy() {
    return this.inFlight > 0;
  }

  /** Re-seat the session after the host refetches `project.show`. */
  resync(revisionCursor: number) {
    this.cursor = revisionCursor;
  }

  private stale(expected: number, actual: number | null): never {
    this.onStale?.(expected, actual);
    throw new StaleEditError(expected, actual);
  }

  private checkAck(expectedBefore: number, res: EditResponse) {
    const next = res.revision?.cursor;
    if (next == null) return; // engine did not report a cursor; nothing to verify
    if (res.status === "already_applied" || res.status === "no_change") {
      if (next !== expectedBefore) this.stale(expectedBefore, next);
      return;
    }
    if (next !== expectedBefore + 1) this.stale(expectedBefore + 1, next);
  }

  /**
   * Submit one action (or a gesture's grouped actions) as a single logical
   * transaction. Without `edit.batch`, grouped actions are applied sequentially
   * — each lands as its own engine revision; `applied` reports how many
   * recorded. A failure aborts the rest and reports `applied` for the caller
   * to display honestly.
   */
  async apply(actions: EditAction | EditAction[], label?: string): Promise<ApplyResult> {
    const list = Array.isArray(actions) ? actions : [actions];
    if (list.length === 0) return { applied: 0, cursor: this.cursor, retried: false };

    this.inFlight += 1;
    try {
      if (list.length > 1 && supports("edit.batch", this.caps)) {
        const params: Record<string, unknown> = {
          dir: this.dir,
          actions: list,
          label: label ?? null,
        };
        if (supports("edit.expect_cursor", this.caps)) params.expect_cursor = this.cursor;
        const res = asEditResponse(await this.call("edit", params));
        // A batch records at most one revision: cursor advances by 0 or 1.
        const next = res.revision?.cursor;
        if (next != null && next !== this.cursor && next !== this.cursor + 1) {
          this.stale(this.cursor + 1, next);
        }
        const recorded = next === this.cursor + 1;
        if (next != null) this.cursor = next;
        return { applied: recorded ? list.length : 0, cursor: this.cursor, retried: !recorded };
      }

      let applied = 0;
      let retried = false;
      for (const action of list) {
        const before = this.cursor;
        const params: Record<string, unknown> = { dir: this.dir, action };
        if (supports("edit.expect_cursor", this.caps)) params.expect_cursor = before;
        let res: EditResponse;
        try {
          res = asEditResponse(await this.call("edit", params));
        } catch (e) {
          if (errorCode(e) === "cursor_mismatch") {
            const actual =
              e && typeof e === "object" && "actual" in e
                ? Number((e as { actual: unknown }).actual)
                : null;
            this.stale(before, actual);
          }
          throw e;
        }
        this.checkAck(before, res);
        if (res.status === "already_applied" || res.status === "no_change") {
          retried = true;
        } else {
          applied += 1;
          if (res.revision) this.cursor = res.revision.cursor;
        }
      }
      return { applied, cursor: this.cursor, retried };
    } finally {
      this.inFlight -= 1;
    }
  }

  async undo(): Promise<boolean> {
    this.inFlight += 1;
    try {
      const res = asEditResponse(
        await this.call("undo", { dir: this.dir, expect_cursor: this.cursor }),
      );
      const next = res.revision?.cursor;
      if (next != null) this.cursor = next;
      return res.status !== "unchanged" && res.status !== "noop" && res.changed !== false;
    } catch (e) {
      if (errorCode(e) === "cursor_mismatch") this.stale(this.cursor, null);
      throw e;
    } finally {
      this.inFlight -= 1;
    }
  }

  async redo(): Promise<boolean> {
    this.inFlight += 1;
    try {
      const res = asEditResponse(
        await this.call("redo", { dir: this.dir, expect_cursor: this.cursor }),
      );
      const next = res.revision?.cursor;
      if (next != null) this.cursor = next;
      return res.status !== "unchanged" && res.status !== "noop" && res.changed !== false;
    } catch (e) {
      if (errorCode(e) === "cursor_mismatch") this.stale(this.cursor, null);
      throw e;
    } finally {
      this.inFlight -= 1;
    }
  }
}
