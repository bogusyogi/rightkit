/**
 * "Already queued" notice for a paste whose URL is already queued or running
 * (#109). The shell's `add_jobs` reports skipped duplicates on the rejection
 * object as `duplicates`; the rest of the batch still queues.
 */

export interface DuplicateSubmission {
  /** The input as the user submitted it. */
  input: string;
  /** Id of the active job that already covers this URL. */
  existing_job_id: string;
}

/** The duplicates carried by an `add_jobs` rejection, or none. */
export function duplicatesOf(e: unknown): DuplicateSubmission[] {
  if (e && typeof e === "object" && Array.isArray((e as { duplicates?: unknown }).duplicates)) {
    return (e as { duplicates: DuplicateSubmission[] }).duplicates;
  }
  return [];
}

/** Notice text for skipped duplicates, or `null` when there were none. */
export function alreadyQueuedNotice(duplicates: DuplicateSubmission[]): string | null {
  if (duplicates.length === 0) return null;
  if (duplicates.length === 1) return `Already queued: ${duplicates[0].input}`;
  return `Already queued: ${duplicates.length} links`;
}
