import type { DisplayJob } from "../contract.js";

/**
 * Format a Tauri/RPC error for the existing error banner. Tauri returns the
 * Rust-side `Err(String)` as a string when the command surface raises one,
 * but it may also wrap it in an Error shape; `String(e)` (the pattern already
 * used throughout the app) does the right thing for both.
 *
 * S-6: this is the single seam every former `.catch(() => {})` site funnels
 * through so we don't lose error context for the operator while keeping the
 * presentation identical to the rest of the app.
 */
export function formatInvokeError(e: unknown): string {
  if (e == null) return "Unknown error";
  if (typeof e === "string") return e;
  if (e instanceof Error) return e.message || e.name || "Error";
  try {
    const s = String(e);
    return s === "[object Object]" ? "Unknown error" : s;
  } catch {
    return "Unknown error";
  }
}

export interface AddJobsFailure {
  /** Jobs the engine accepted before the failure; the caller inserts them. */
  accepted: DisplayJob[];
  message: string;
}

/**
 * Split an `add_jobs` rejection. A partial success arrives as
 * `{ accepted, error }` (issue #95); anything else is a whole-batch failure.
 */
export function addJobsFailure(e: unknown): AddJobsFailure {
  if (e && typeof e === "object" && Array.isArray((e as { accepted?: unknown }).accepted)) {
    const partial = e as { accepted: DisplayJob[]; error?: unknown };
    return { accepted: partial.accepted, message: formatInvokeError(partial.error) };
  }
  return { accepted: [], message: formatInvokeError(e) };
}
