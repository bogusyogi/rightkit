// Request and page types for the host's paged dictation history query.
import type { HistoryItem } from "../lib/homeMetrics.js";

export const HISTORY_PAGE_SIZE = 50;

export interface NativeHistoryFilter {
  search: string;
  from_ms: number | null;
  until_ms: number | null;
  app: string;
}

export interface HistoryQueryArgs extends NativeHistoryFilter {
  cancelled_only: boolean;
}

export interface HistoryPageResult {
  items: HistoryItem[];
  total: number;
  has_more: boolean;
}
