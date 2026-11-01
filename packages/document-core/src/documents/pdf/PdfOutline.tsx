import { useState } from "react";
import type { OutlineNode } from "./pdfShared";

// Recursive outline (bookmarks) tree. Each node with a destination is a clickable row; nodes with
// children get an expand/collapse chevron (expanded by default).
export function PdfOutline({
  nodes,
  onSelect,
  depth = 0,
}: {
  nodes: OutlineNode[];
  onSelect: (dest: string | unknown[] | null) => void;
  depth?: number;
}) {
  return (
    <ul>
      {nodes.map((node, i) => (
        <OutlineRow key={i} node={node} onSelect={onSelect} depth={depth} />
      ))}
    </ul>
  );
}

function OutlineRow({
  node,
  onSelect,
  depth,
}: {
  node: OutlineNode;
  onSelect: (dest: string | unknown[] | null) => void;
  depth: number;
}) {
  const [open, setOpen] = useState(true);
  const hasKids = node.items && node.items.length > 0;
  return (
    <li>
      <div
        className="flex items-center text-muted hover:text-paper"
        style={{ paddingLeft: depth * 12 + 4 }}
      >
        {hasKids ? (
          <button
            type="button"
            onClick={() => setOpen((o) => !o)}
            className="vr-press vr-focus w-4 shrink-0 rounded text-muted-2 hover:text-paper"
            title={open ? "Collapse outline section" : "Expand outline section"}
            aria-label={open ? "Collapse outline section" : "Expand outline section"}
            aria-expanded={open}
          >
            {open ? "▾" : "▸"}
          </button>
        ) : (
          <span className="w-4 shrink-0" />
        )}
        <button
          type="button"
          onClick={() => onSelect(node.dest)}
          disabled={!node.dest}
          className={`flex-1 truncate py-1 pr-2 text-left ${node.bold ? "font-semibold" : ""} ${node.italic ? "italic" : ""} disabled:cursor-default disabled:opacity-60`}
          title={node.title}
        >
          {node.title || "Untitled"}
        </button>
      </div>
      {hasKids && open && <PdfOutline nodes={node.items} onSelect={onSelect} depth={depth + 1} />}
    </li>
  );
}
