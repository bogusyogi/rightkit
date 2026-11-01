import Icon from "@rightkit/agent-ui-core/components/Icon";

export default function ConversationFindBar({
  findQuery,
  setFindQuery,
  findActive,
  findCount,
  findInputRef,
  supportsHighlight,
  closeFind,
  stepFind,
}: {
  findQuery: string;
  setFindQuery: (value: string) => void;
  findActive: number;
  findCount: number;
  findInputRef: React.RefObject<HTMLInputElement | null>;
  supportsHighlight: boolean;
  closeFind: () => void;
  stepFind: (dir: 1 | -1) => void;
}) {
  return (
    <search className="sticky top-2 z-30 ml-auto mr-1 flex items-center gap-1.5 rounded-lg border border-line bg-panel/95 px-2 py-1.5 shadow-app backdrop-blur">
      <Icon name="search" size={13} />
      <input
        ref={findInputRef}
        value={findQuery}
        onChange={(e) => setFindQuery(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            stepFind(e.shiftKey ? -1 : 1);
          } else if (e.key === "Escape") {
            e.preventDefault();
            closeFind();
          }
        }}
        placeholder={supportsHighlight ? "Find in conversation" : "Find unavailable"}
        disabled={!supportsHighlight}
        aria-label="Find in conversation"
        className="w-44 bg-transparent text-[12.5px] text-text placeholder:text-text-3 outline-none"
      />
      <span className="min-w-[44px] text-right text-[11px] tabular-nums text-text-3">
        {findQuery.trim() ? `${findActive}/${findCount}` : "0/0"}
      </span>
      <button
        type="button"
        onClick={() => stepFind(-1)}
        disabled={findCount === 0}
        className="grid h-6 w-6 place-items-center rounded-md border-0 bg-transparent text-text-2 hover:bg-panel-2 hover:text-text disabled:opacity-40"
        aria-label="Previous match"
        title="Previous (Shift+Enter)"
      >
        <span className="rotate-180">
          <Icon name="chevron" size={13} />
        </span>
      </button>
      <button
        type="button"
        onClick={() => stepFind(1)}
        disabled={findCount === 0}
        className="grid h-6 w-6 place-items-center rounded-md border-0 bg-transparent text-text-2 hover:bg-panel-2 hover:text-text disabled:opacity-40"
        aria-label="Next match"
        title="Next (Enter)"
      >
        <Icon name="chevron" size={13} />
      </button>
      <button
        type="button"
        onClick={closeFind}
        className="grid h-6 w-6 place-items-center rounded-md border-0 bg-transparent text-text-2 hover:bg-panel-2 hover:text-text"
        aria-label="Close find"
        title="Close (Esc)"
      >
        <Icon name="close" size={13} />
      </button>
    </search>
  );
}
