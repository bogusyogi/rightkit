// Meaning search with a keyword fallback: if the semantic call errors or finds
// nothing, the keyword results are shown with a short note saying so.

export const MEANING_EMPTY_NOTE = "Nothing matched by meaning; showing keyword results.";
export const MEANING_ERROR_NOTE = "Meaning search is unavailable; showing keyword results.";

export interface MeaningOutcome<T> {
  rows: T[];
  /** Short inline note, or null when the meaning results are shown as is. */
  note: string | null;
  /** True when `rows` came from the meaning search (so they are not paged). */
  usedMeaning: boolean;
}

export async function meaningOrKeyword<T>(
  meaning: () => Promise<T[]>,
  keyword: () => Promise<T[]>,
): Promise<MeaningOutcome<T>> {
  let note: string;
  try {
    const rows = await meaning();
    if (rows.length > 0) return { rows, note: null, usedMeaning: true };
    note = MEANING_EMPTY_NOTE;
  } catch {
    note = MEANING_ERROR_NOTE;
  }
  return { rows: await keyword(), note, usedMeaning: false };
}
