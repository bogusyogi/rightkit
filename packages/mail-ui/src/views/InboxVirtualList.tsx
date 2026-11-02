import type { VirtualItem } from "@tanstack/react-virtual";
import type { Thread } from "../lib/ipc.js";
import { keyOf } from "../lib/mail.js";
import { ThreadRow } from "./InboxThreadRow.js";

type ListItem =
  | { kind: "header"; label: string }
  | { kind: "row"; thread: Thread; rowIndex: number };

export function InboxVirtualList({
  focusIdx,
  getTotalSize,
  labelNameOf,
  listItems,
  measureElement,
  onOpenAttachment,
  openingAttachmentKey,
  query,
  scrollMargin,
  selected,
  setListAnchor,
  virtualItems,
}: {
  focusIdx: number;
  getTotalSize: () => number;
  labelNameOf: (id: string) => string;
  listItems: ListItem[];
  measureElement: (node: Element | null) => void;
  onOpenAttachment: (thread: Thread, filename: string) => void;
  openingAttachmentKey: string | null;
  query: string;
  scrollMargin: number;
  selected: Set<string>;
  setListAnchor: (node: HTMLDivElement | null) => void;
  virtualItems: VirtualItem[];
}) {
  return (
    <div
      ref={setListAnchor}
      className="list-virtual"
      style={{
        height: getTotalSize(),
        position: "relative",
      }}
    >
      {virtualItems.map((vi) => {
        const it = listItems[vi.index];
        return (
          <div
            key={vi.key}
            data-index={vi.index}
            ref={measureElement}
            style={{
              position: "absolute",
              top: 0,
              left: 0,
              width: "100%",
              transform: `translateY(${vi.start - scrollMargin}px)`,
            }}
          >
            {it.kind === "header" ? (
              <div className="ws-daygroup list-daygroup">{it.label}</div>
            ) : (
              <ThreadRow
                t={it.thread}
                focused={it.rowIndex === focusIdx}
                isSelected={selected.has(keyOf(it.thread))}
                labelNameOf={labelNameOf}
                onOpenAttachment={onOpenAttachment}
                openingAttachmentKey={openingAttachmentKey}
                query={query}
              />
            )}
          </div>
        );
      })}
    </div>
  );
}
