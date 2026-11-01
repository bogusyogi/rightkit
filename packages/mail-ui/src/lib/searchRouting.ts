export interface VisibleSearchThread {
  account_id?: string;
  thread_id?: string;
  from_name?: string;
  subject?: string;
  snippet?: string;
  participants?: string[];
}

export function searchAccountId(
  folder: string,
  activeAccount: string | null,
  activeLabel: string | null,
): string | null {
  if (activeLabel) return activeAccount;
  return folder === "all" ? null : activeAccount;
}

export function visibleThreadMatches<T extends VisibleSearchThread>(
  threads: T[],
  query: string,
): T[] {
  const q = query.trim().toLocaleLowerCase();
  if (!q) return [];
  return threads.filter((thread) => {
    const haystack = [
      thread.from_name,
      thread.subject,
      thread.snippet,
      ...(thread.participants ?? []),
    ]
      .filter(Boolean)
      .join(" ")
      .toLocaleLowerCase();
    return haystack.includes(q);
  });
}
