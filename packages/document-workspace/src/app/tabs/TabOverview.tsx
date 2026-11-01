// Tab overview — the ⌘⇧A popover that drops out of the titlebar tab strip's right-edge
// button (brief Part A item 8; visual spec docs/design/viewright-tabs-explorer.html `.pop`).
// Built on the sidebar's shared Popover (glass panel, Escape + click-outside dismiss, and
// focus restore to the element that opened it). Top→bottom: search field (focused on open),
// type chips with counts, a scrollable list of matching tabs, and a footer with "N of M tabs"
// plus a bulk Close that only appears while a filter or search is active.
import {
  memo,
  useEffect,
  useRef,
  useState,
  type KeyboardEvent as ReactKeyboardEvent,
  type ReactNode,
  type RefObject,
} from "react";
import { moveIndex } from "@rightkit/platform-ui/headless";
import type { DocFormat, LoadedDocument } from "@rightkit/document-core/documents/types";
import { isUntitled } from "@rightkit/document-core/documents/types";
import { FileTypeIcon } from "../../documents/FileTypeIcon.js";
import { LiveTitle } from "../LiveTitle.js";
import { Popover } from "../sidebar/SidebarPrimitives.js";
import { pathFolder } from "../sidebar/SidebarPathUtils.js";
import { useTabDirty } from "./useTabDirty.js";

/** Type-filter groups — brief: All, PDF, Markdown (md+mdx), Word (docx), Images, HTML, Text.
 *  Formats outside these groups (web previews, unknowns) are reachable through All + search. */
type TypeGroupId = "all" | "pdf" | "md" | "docx" | "image" | "html" | "txt";

const TYPE_GROUPS: readonly {
  id: TypeGroupId;
  label: string;
  formats: readonly DocFormat[] | null;
}[] = [
  { id: "all", label: "All", formats: null },
  { id: "pdf", label: "PDF", formats: ["pdf"] },
  { id: "md", label: "Markdown", formats: ["md", "mdx"] },
  { id: "docx", label: "Word", formats: ["docx"] },
  { id: "image", label: "Images", formats: ["image"] },
  { id: "html", label: "HTML", formats: ["html"] },
  { id: "txt", label: "Text", formats: ["txt"] },
];

/** [singular, plural] nouns for the bulk-close label — "Close 8 PDFs", "Close 3 Word
 *  documents", "Close 5 tabs" when only a search is narrowing the set. */
const TYPE_NOUNS: Record<TypeGroupId, [string, string]> = {
  all: ["tab", "tabs"],
  pdf: ["PDF", "PDFs"],
  md: ["Markdown file", "Markdown files"],
  docx: ["Word document", "Word documents"],
  image: ["image", "images"],
  html: ["HTML file", "HTML files"],
  txt: ["text file", "text files"],
};

export interface TabOverviewProps {
  docs: LoadedDocument[];
  activePath: string | null;
  titleFor: (doc: LoadedDocument) => string;
  isDirty: (doc: LoadedDocument) => boolean;
  dirtyPaths: ReadonlySet<string>;
  subscribeDirty: (path: string, cb: () => void) => () => void;
  subscribeEdits: (path: string, cb: () => void) => () => void;
  getEditSnapshot: (path: string) => string | undefined;
  computeTitle?: (doc: LoadedDocument) => string;
  onActivate: (path: string) => void;
  /** Bulk close for the footer action (integrator wires closeMany — dirty docs route
   *  through the Save-All/Discard-All chooser upstream). */
  onClosePaths: (paths: string[]) => void;
  onClose: () => void;
  /** Overview toggle button — Popover already restores focus to its opener, so this is a
   *  fallback only (e.g. the button unmounted between open and close). */
  returnFocusRef: RefObject<HTMLButtonElement | null>;
}

export const TabOverview = memo(TabOverviewImpl);

function TabOverviewImpl({
  docs,
  activePath,
  titleFor,
  isDirty,
  dirtyPaths,
  subscribeDirty,
  subscribeEdits,
  getEditSnapshot,
  computeTitle,
  onActivate,
  onClosePaths,
  onClose,
  returnFocusRef,
}: TabOverviewProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLDivElement>(null);
  const [query, setQuery] = useState("");
  const [typeGroup, setTypeGroup] = useState<TypeGroupId>("all");

  // Search field takes focus on open (Popover's auto-focus only targets [role=menuitem]s,
  // which this list deliberately doesn't use — rows stay honest buttons).
  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  // If the opener vanished while we were open, land on the overview button instead of <body>.
  useEffect(
    () => () => {
      if (document.activeElement === document.body) returnFocusRef.current?.focus();
    },
    [returnFocusRef],
  );

  const q = query.trim().toLowerCase();
  const formats = TYPE_GROUPS.find((g) => g.id === typeGroup)?.formats ?? null;
  const shown = docs.filter((doc) => {
    if (formats && !formats.includes(doc.format)) return false;
    if (!q) return true;
    return (
      titleFor(doc).toLowerCase().includes(q) || pathFolder(doc.path).toLowerCase().includes(q)
    );
  });

  const rows = () => [
    ...(listRef.current?.querySelectorAll<HTMLElement>("[data-vr-ov-row]") ?? []),
  ];

  // ↓/↑ roam between the search field and the row list (clamped, no wrap — first row's ↑
  // returns to the field); Enter in the field opens the first match; Enter on a row is the
  // button's native click (activate + close). Escape is Popover's.
  const onKeyDown = (event: ReactKeyboardEvent<HTMLDivElement>) => {
    const list = rows();
    if (event.key === "ArrowDown") {
      if (!list.length) return;
      event.preventDefault();
      const at = list.indexOf(document.activeElement as HTMLElement);
      list[at < 0 ? 0 : moveIndex(at, list.length, "next", false)]?.focus();
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      const at = list.indexOf(document.activeElement as HTMLElement);
      if (at <= 0) inputRef.current?.focus();
      else list[moveIndex(at, list.length, "prev", false)]?.focus();
      return;
    }
    if (event.key === "Enter" && document.activeElement === inputRef.current && shown.length) {
      event.preventDefault();
      onActivate(shown[0].path);
      onClose();
    }
  };

  const activate = (path: string) => {
    onActivate(path);
    onClose();
  };

  const closeShown = () => {
    onClosePaths(shown.map((d) => d.path));
    onClose();
  };

  // Footer bulk action: only while a filter/search is active AND something matched —
  // "Close 8 PDFs" / "Close 3 Word documents" / "Close 5 tabs" (search-only), with the
  // dirty-count hint when the affected set includes unsaved tabs.
  const filterActive = typeGroup !== "all" || q !== "";
  const dirtyShown = shown.filter((d) => dirtyPaths.has(d.path) || isDirty(d)).length;
  const noun = TYPE_NOUNS[typeGroup][shown.length === 1 ? 0 : 1];

  return (
    <Popover onClose={onClose} className="vr-ov-pop" role="dialog" ariaModal={false} ariaLabel="Open tabs">
      <input
        ref={inputRef}
        className="vr-ov-q vr-focus"
        placeholder="Search open tabs"
        aria-label="Search open tabs"
        value={query}
        onChange={(event) => setQuery(event.target.value)}
        onKeyDown={onKeyDown}
      />
      <div className="vr-ov-chips" role="group" aria-label="Filter by type">
        {TYPE_GROUPS.map((group) => {
          const n = group.formats
            ? docs.filter((d) => group.formats?.includes(d.format)).length
            : docs.length;
          if (group.id !== "all" && n === 0) return null; // hide zero-count chips
          return (
            <button
              key={group.id}
              type="button"
              className="vr-ov-chip vr-press vr-focus"
              aria-pressed={typeGroup === group.id}
              onClick={() => setTypeGroup(group.id)}
            >
              {group.label} {n}
            </button>
          );
        })}
      </div>
      <div ref={listRef} className="vr-ov-list" onKeyDown={onKeyDown}>
        {shown.length === 0 ? (
          <div className="vr-ov-empty">No open tabs match</div>
        ) : (
          shown.map((doc) => (
            <OverviewRow
              key={doc.tabKey ?? doc.path}
              doc={doc}
              title={titleFor(doc)}
              titleNode={
                computeTitle && isUntitled(doc.path) ? (
                  <LiveTitle
                    doc={doc}
                    compute={computeTitle}
                    subscribeEdits={subscribeEdits}
                    getEditSnapshot={getEditSnapshot}
                  />
                ) : undefined
              }
              isDirty={isDirty}
              dirtyHint={dirtyPaths.has(doc.path)}
              subscribeDirty={subscribeDirty}
              active={doc.path === activePath}
              onActivate={activate}
            />
          ))
        )}
      </div>
      <div className="vr-ov-foot">
        <span className="vr-ov-sum">
          {shown.length} of {docs.length} {docs.length === 1 ? "tab" : "tabs"}
        </span>
        {filterActive && shown.length > 0 && (
          <button type="button" className="vr-ov-close vr-press vr-focus" onClick={closeShown}>
            Close {shown.length} {noun}
            {dirtyShown > 0 ? ` · ${dirtyShown} unsaved, will ask` : ""}
          </button>
        )}
      </div>
    </Popover>
  );
}

const OverviewRow = memo(OverviewRowImpl);

function OverviewRowImpl({
  doc,
  title,
  titleNode,
  isDirty,
  dirtyHint,
  subscribeDirty,
  active,
  onActivate,
}: {
  doc: LoadedDocument;
  title: string;
  titleNode?: ReactNode;
  isDirty: (doc: LoadedDocument) => boolean;
  dirtyHint: boolean;
  subscribeDirty: (path: string, cb: () => void) => () => void;
  active: boolean;
  onActivate: (path: string) => void;
}) {
  const dirty = useTabDirty(doc, dirtyHint, isDirty, subscribeDirty);
  const displayPath = doc.format === "web" ? (doc.web?.url ?? doc.path) : doc.path;
  const folder = isUntitled(doc.path) ? "Unsaved buffer" : pathFolder(displayPath);
  return (
    <button
      type="button"
      data-vr-ov-row
      className={`vr-ov-row${active ? " vr-ov-row-active" : ""}`}
      data-active={active || undefined}
      title={displayPath}
      aria-current={active || undefined}
      aria-label={`${title}${dirty ? " — Unsaved edits" : ""}`}
      onClick={() => onActivate(doc.path)}
    >
      <span className="vr-ov-row-ficon" aria-hidden="true">
        <FileTypeIcon format={doc.format} path={doc.path} size={20} />
      </span>
      <span className="vr-ov-row-text">
        <span className="vr-ov-row-name">{titleNode ?? title}</span>
        <span className="vr-ov-row-meta">{folder}</span>
      </span>
      {dirty && <span className="vr-ov-row-unsaved">● Unsaved</span>}
    </button>
  );
}
