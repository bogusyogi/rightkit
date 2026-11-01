import Icon from "@rightkit/agent-ui-core/components/Icon";
import { EmptyState } from "@rightkit/react-shell/react";
import type { MemoryEntryRow } from "@rightkit/agent-ui-core/lib/types";
import { formatIsoTime } from "./runtimeUtils";

export default function MemoryRuntimePanel({
  entries,
  query,
  draft,
  keywordDraft,
  pending,
  onQuery,
  onDraft,
  onKeywordDraft,
  onAdd,
}: {
  entries: MemoryEntryRow[];
  query: string;
  draft: string;
  keywordDraft: string;
  pending: boolean;
  onQuery: (query: string) => void;
  onDraft: (draft: string) => void;
  onKeywordDraft: (draft: string) => void;
  onAdd: () => void;
}) {
  return (
    <div className="flex-1 min-h-0 overflow-y-auto flex flex-col">
      <div className="grid grid-cols-[minmax(180px,0.8fr)_minmax(220px,1.2fr)_minmax(160px,0.8fr)_auto] gap-2 px-5 py-3 border-b border-line bg-panel/60">
        <input
          className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text outline-none focus:border-accent-2"
          value={query}
          onChange={(e) => onQuery(e.target.value)}
          placeholder="search memories"
          aria-label="Search runtime memories"
        />
        <input
          className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text outline-none focus:border-accent-2"
          value={draft}
          onChange={(e) => onDraft(e.target.value)}
          placeholder="new memory"
          aria-label="New memory content"
        />
        <input
          className="h-8 rounded-lg border border-line bg-panel px-3 text-[12px] text-text outline-none focus:border-accent-2"
          value={keywordDraft}
          onChange={(e) => onKeywordDraft(e.target.value)}
          placeholder="keywords"
          aria-label="New memory keywords"
        />
        <button
          type="button"
          className="inline-flex items-center justify-center h-8 px-3 rounded-lg border border-accent-2/40 bg-accent-2/10 text-accent-2 text-[12px] font-semibold cursor-pointer hover:bg-accent-2/15 disabled:opacity-40 disabled:cursor-not-allowed"
          disabled={pending || !draft.trim()}
          onClick={onAdd}
        >
          {pending ? "Adding" : "Add"}
        </button>
      </div>

      {entries.length === 0 ? (
        <EmptyState icon={<Icon name="spark" size={18} />} title="No memories found">{"Durable memories appear here after the learning system or manual capture stores reusable project experience. Add one above to seed the store."}</EmptyState>
      ) : (
        <div className="flex flex-col divide-y divide-line">
          {entries.map((entry) => (
            <div
              className="grid grid-cols-[minmax(0,1fr)_minmax(160px,0.45fr)] gap-4 px-5 py-3"
              key={entry.id}
            >
              <div className="min-w-0">
                <strong className="block text-[13px] font-semibold text-text">
                  {entry.content}
                </strong>
                <p className="block text-[11px] text-text-3 truncate">
                  {entry.keywords.length ? entry.keywords.join(", ") : "No keywords"}
                </p>
              </div>

              <div className="min-w-0 text-right">
                <code className="block text-[12px] font-mono text-accent-2 truncate">
                  {entry.tier}
                </code>
                <span className="block text-[11px] text-text-3 truncate">
                  score {entry.score.toFixed(2)} - used {entry.accessCount}
                </span>
                <span className="block text-[11px] text-text-3 truncate">
                  {formatIsoTime(entry.createdAt)}
                </span>
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}
