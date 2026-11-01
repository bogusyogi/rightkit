import { useRef, useState, type ReactNode } from "react";
import type { DocxHostToolbarContext } from "./eigenpalReact";
import { useDocxCommandBridge } from "./useDocxCommandBridge";
import { shortcutLabel } from "@rightkit/document-core/platform";
import { DocumentToolbarSearch, type DocumentSearchMode } from "@rightkit/document-core/documents/DocumentToolbarFind";
import { DocumentToolbarShell } from "@rightkit/document-core/documents/DocumentToolbarShell";

// OOXML font size is in half-points (22 = 11pt). Inlined so we don't take a direct
// dependency on @eigenpal/docx-editor-core (it's a transitive dep, not hoisted).
const halfToPt = (hp: number) => hp / 2;
const ptToHalf = (pt: number) => pt * 2;
import { TBtn, Sep, Group, Dropdown, Item, ColorGrid } from "@rightkit/document-core/documents/toolbarPrimitives";
import {
  IconUndo,
  IconRedo,
  IconAlignLeft,
  IconAlignCenter,
  IconAlignRight,
  IconAlignJustify,
  IconBullet,
  IconNumbered,
  IconIndent,
  IconOutdent,
  IconHighlight,
  IconImage,
  IconLink,
  IconOutline,
} from "./toolbarIcons";
import { DocBar } from "@rightkit/document-core/app/shell/docBar";

// Curated font set (Word-common). Applied via the format bridge; no eigenpal UI.
const FONTS = [
  "Calibri",
  "Cambria",
  "Arial",
  "Helvetica",
  "Times New Roman",
  "Georgia",
  "Garamond",
  "Verdana",
  "Tahoma",
  "Trebuchet MS",
  "Courier New",
  "Consolas",
];
const SIZES = [8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 36, 48, 72];

// RRGGBB (no #). Compact Word-like text palette + a highlight set.
const TEXT_COLORS = [
  { name: "Black", hex: "000000" },
  { name: "Dk Gray", hex: "434343" },
  { name: "Gray", hex: "666666" },
  { name: "Lt Gray", hex: "999999" },
  { name: "White", hex: "FFFFFF" },
  { name: "Red", hex: "E5484D" },
  { name: "Orange", hex: "E8833A" },
  { name: "Yellow", hex: "E5A300" },
  { name: "Green", hex: "2E9C68" },
  { name: "Teal", hex: "1F9C9C" },
  { name: "Blue", hex: "2F6FEB" },
  { name: "Indigo", hex: "5B53D6" },
  { name: "Purple", hex: "8B5CF6" },
  { name: "Magenta", hex: "C2398A" },
  { name: "Copper", hex: "C46A3D" },
  { name: "Sienna", hex: "A5552B" },
];
const HIGHLIGHTS = [
  { name: "Yellow", hex: "FFFF00" },
  { name: "Green", hex: "00FF00" },
  { name: "Cyan", hex: "00FFFF" },
  { name: "Magenta", hex: "FF00FF" },
  { name: "Blue", hex: "00B0F0" },
  { name: "Red", hex: "FF0000" },
  { name: "Gray", hex: "BFBFBF" },
  { name: "Lime", hex: "C4F000" },
];

interface Props {
  context: DocxHostToolbarContext;
  /** Read mode — the single floating capsule keeps viewing controls (outline, zoom); every
   *  editing control hides. */
  readOnly?: boolean;
  /** Outline (headings) panel toggle — docked here instead of the vendor's floating button. */
  outlineOpen?: boolean;
  onToggleOutline?: () => void;
  findBar?: ReactNode;
  findMode?: DocumentSearchMode;
  onRequestFind?: () => void;
  onRequestReplace?: () => void;
}

export function HostDocxToolbar({
  context,
  readOnly = false,
  outlineOpen,
  onToggleOutline,
  findBar,
  findMode,
  onRequestFind,
  onRequestReplace,
}: Props) {
  const cmd = useDocxCommandBridge(context);
  const f = context.selectionFormatting;
  const disabled = context.disabled;

  const align = f.alignment;
  const list = f.listState?.type;
  const sizePt = f.fontSize ? Math.round(halfToPt(f.fontSize)) : undefined;
  const paragraphStyles = (context.documentStyles ?? []).filter((s) => s.type === "paragraph");
  const currentStyle = paragraphStyles.find((s) => s.styleId === f.styleId);

  // Single capsule docked in its own row above the vendor's paged viewport (which follows it in
  // the vendor's flex column), so scrolled pages end at the row's hairline; the find capsule hangs
  // 8px under the row. Outline/zoom are merged into the one capsule. In Read only outline + zoom
  // remain (view-first).
  return (
    <div className="no-print vr-chrome vr-document-toolbar relative z-30 flex-shrink-0 text-chrome-xs text-muted">
      <DocBar>
        <div className="vr-capsule h-[42px] px-1">
          <DocumentToolbarShell
            ariaLabel="Word tools"
            variant="scroll"
            className="vr-capsule-scroll"
          >
            {/* View group. */}
            {onToggleOutline && (
              <TBtn
                label={<IconOutline />}
                title="Outline"
                active={!!outlineOpen}
                onRun={onToggleOutline}
                round
              />
            )}
            {/* Editing group — hidden in Read. */}
            {!readOnly && (
              <>
                {onToggleOutline && <Sep />}
                {/* History (Save lives in the File menu) */}
                <Group>
                  <TBtn
                    label={<IconUndo />}
                    title={`Undo (${shortcutLabel("mod+z")})`}
                    onRun={cmd.undo}
                    disabled={!context.canUndo}
                    round
                  />
                  <TBtn
                    label={<IconRedo />}
                    title={`Redo (${shortcutLabel("mod+y")})`}
                    onRun={cmd.redo}
                    disabled={!context.canRedo}
                    round
                  />
                </Group>
                <Sep />

                {/* Paragraph style */}
                <Dropdown
                  label={currentStyle?.name ?? currentStyle?.styleId ?? "Normal"}
                  title="Paragraph style"
                  minWidth={108}
                  panelWidth={180}
                  disabled={disabled}
                >
                  {(close) =>
                    paragraphStyles.length === 0 ? (
                      <div
                        role="menuitem"
                        aria-disabled="true"
                        tabIndex={-1}
                        className="px-3 py-1 text-chrome-xs text-muted-2"
                      >
                        No styles
                      </div>
                    ) : (
                      paragraphStyles.map((s) => (
                        <Item
                          key={s.styleId}
                          active={s.styleId === f.styleId}
                          onRun={() => {
                            cmd.fmt({ type: "applyStyle", value: s.styleId })();
                            close();
                          }}
                        >
                          {s.name ?? s.styleId}
                        </Item>
                      ))
                    )
                  }
                </Dropdown>
                <Sep />

                {/* Font family + size */}
                <Dropdown
                  label={f.fontFamily ?? "Font"}
                  title="Font family"
                  minWidth={104}
                  panelWidth={150}
                  disabled={disabled}
                >
                  {(close) =>
                    FONTS.map((name) => (
                      <Item
                        key={name}
                        active={f.fontFamily === name}
                        onRun={() => {
                          cmd.fmt({ type: "fontFamily", value: name })();
                          close();
                        }}
                        style={{ fontFamily: name }}
                      >
                        {name}
                      </Item>
                    ))
                  }
                </Dropdown>
                <Dropdown
                  label={sizePt ? String(sizePt) : "Size"}
                  title="Font size"
                  minWidth={56}
                  panelWidth={64}
                  disabled={disabled}
                >
                  {(close) =>
                    SIZES.map((pt) => (
                      <Item
                        key={pt}
                        active={sizePt === pt}
                        onRun={() => {
                          cmd.fmt({ type: "fontSize", value: ptToHalf(pt) })();
                          close();
                        }}
                      >
                        {pt}
                      </Item>
                    ))
                  }
                </Dropdown>
                <Sep />

                {/* Character marks */}
                <Group>
                  <TBtn
                    label="B"
                    title={`Bold (${shortcutLabel("mod+b")})`}
                    bold
                    active={!!f.bold}
                    onRun={cmd.fmt("bold")}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label="I"
                    title={`Italic (${shortcutLabel("mod+i")})`}
                    italic
                    active={!!f.italic}
                    onRun={cmd.fmt("italic")}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label="U"
                    title={`Underline (${shortcutLabel("mod+u")})`}
                    underline
                    active={!!f.underline}
                    onRun={cmd.fmt("underline")}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label="S"
                    title="Strikethrough"
                    strike
                    active={!!f.strike}
                    onRun={cmd.fmt("strikethrough")}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label="x²"
                    title="Superscript"
                    active={!!f.superscript}
                    onRun={cmd.fmt("superscript")}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label="x₂"
                    title="Subscript"
                    active={!!f.subscript}
                    onRun={cmd.fmt("subscript")}
                    disabled={disabled}
                    round
                  />
                </Group>
                <Sep />

                {/* Colors */}
                <Group>
                  <Dropdown
                    label="A"
                    title="Text color"
                    minWidth={40}
                    panelWidth={188}
                    disabled={disabled}
                  >
                    {(close) => (
                      <ColorGrid
                        colors={TEXT_COLORS}
                        onPick={(hex) => {
                          cmd.fmt({ type: "textColor", value: { rgb: hex } })();
                          close();
                        }}
                        onClear={() => {
                          cmd.fmt("clearFormatting")();
                          close();
                        }}
                        clearLabel="Clear formatting"
                      />
                    )}
                  </Dropdown>
                  <Dropdown
                    label={<IconHighlight />}
                    title="Highlight"
                    minWidth={40}
                    panelWidth={188}
                    disabled={disabled}
                  >
                    {(close) => (
                      <ColorGrid
                        colors={HIGHLIGHTS}
                        onPick={(hex) => {
                          cmd.fmt({ type: "highlightColor", value: hex })();
                          close();
                        }}
                        onClear={() => {
                          cmd.fmt({ type: "highlightColor", value: "none" })();
                          close();
                        }}
                        clearLabel="No highlight"
                      />
                    )}
                  </Dropdown>
                </Group>
                <Sep />

                {/* Alignment */}
                <Group>
                  <TBtn
                    label={<IconAlignLeft />}
                    title="Align left"
                    active={!align || align === "left"}
                    onRun={cmd.fmt({ type: "alignment", value: "left" })}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label={<IconAlignCenter />}
                    title="Align center"
                    active={align === "center"}
                    onRun={cmd.fmt({ type: "alignment", value: "center" })}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label={<IconAlignRight />}
                    title="Align right"
                    active={align === "right"}
                    onRun={cmd.fmt({ type: "alignment", value: "right" })}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label={<IconAlignJustify />}
                    title="Justify"
                    active={align === "both"}
                    onRun={cmd.fmt({ type: "alignment", value: "both" })}
                    disabled={disabled}
                    round
                  />
                </Group>
                <Sep />

                {/* Lists + indent */}
                <Group>
                  <TBtn
                    label={<IconBullet />}
                    title="Bulleted list"
                    active={list === "bullet"}
                    onRun={cmd.fmt("bulletList")}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label={<IconNumbered />}
                    title="Numbered list"
                    active={list === "numbered"}
                    onRun={cmd.fmt("numberedList")}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label={<IconIndent />}
                    title="Increase indent"
                    onRun={cmd.fmt("indent")}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label={<IconOutdent />}
                    title="Decrease indent"
                    onRun={cmd.fmt("outdent")}
                    disabled={disabled}
                    round
                  />
                </Group>
                <Sep />

                {/* Insert */}
                <Group>
                  <Dropdown
                    label="Table"
                    title="Insert table"
                    minWidth={56}
                    panelWidth={196}
                    disabled={disabled}
                  >
                    {(close) => (
                      <TableGridPicker
                        onPick={(r, c) => {
                          cmd.insertTable(r, c)();
                          close();
                        }}
                      />
                    )}
                  </Dropdown>
                  <TBtn
                    label={<IconImage />}
                    title="Insert image"
                    onRun={cmd.insertImage}
                    disabled={disabled}
                    round
                  />
                  <TBtn
                    label={<IconLink />}
                    title="Insert link"
                    onRun={cmd.fmt("insertLink")}
                    disabled={disabled}
                    round
                  />
                  <Dropdown
                    label="More"
                    title="Insert more"
                    minWidth={56}
                    panelWidth={160}
                    disabled={disabled}
                  >
                    {(close) => (
                      <>
                        <Item
                          onRun={() => {
                            cmd.insertPageBreak();
                            close();
                          }}
                        >
                          Page break
                        </Item>
                        <Item
                          onRun={() => {
                            cmd.insertTOC();
                            close();
                          }}
                        >
                          Table of contents
                        </Item>
                        <Item
                          onRun={() => {
                            cmd.pageSetup();
                            close();
                          }}
                        >
                          Page setup…
                        </Item>
                      </>
                    )}
                  </Dropdown>
                </Group>
              </>
            )}
            {/* Zoom group. */}
            {(onToggleOutline || !readOnly) && <Sep />}
            <TBtn label="−" title="Zoom out" onRun={cmd.zoomOut} round />
            <button
              type="button"
              title="Reset zoom"
              aria-label={`Reset zoom (currently ${Math.round(context.zoom * 100)} percent)`}
              onMouseDown={(e) => {
                e.preventDefault();
                cmd.resetZoom();
              }}
              onClick={(e) => {
                // Pointer clicks already ran via mousedown (detail > 0); keyboard/programmatic
                // activation reports detail === 0 and must still reset.
                if (e.detail > 0) return;
                cmd.resetZoom();
              }}
              className="w-12 flex-shrink-0 text-center font-mono tabular-nums text-muted-2 hover:text-paper"
            >
              {Math.round(context.zoom * 100)}%
            </button>
            <TBtn label="+" title="Zoom in" onRun={cmd.zoomIn} round />
            {/* Wave 2: Experimental chip at the end of the bar (per the redesign mockup). */}
            {!readOnly && (
              <span
                className="mx-1 flex-shrink-0 select-none whitespace-nowrap rounded-full border border-line px-2 py-0.5 text-chrome-xs uppercase tracking-wide text-muted-2"
                title="The DOCX editor is experimental"
              >
                Experimental
              </span>
            )}
          </DocumentToolbarShell>
        </div>
        <DocumentToolbarSearch
          findBar={findBar}
          activeMode={findMode}
          onRequestFind={onRequestFind}
          onRequestReplace={onRequestReplace}
        />
      </DocBar>
    </div>
  );
}

/** Hover-to-size table grid (max 8×10), like Word's table inserter. */
export function TableGridPicker({ onPick }: { onPick: (rows: number, cols: number) => void }) {
  const [hover, setHover] = useState<{ r: number; c: number }>({ r: 0, c: 0 });
  const [active, setActive] = useState({ r: 1, c: 1 });
  const cells = useRef<Array<HTMLButtonElement | null>>([]);
  const COLS = 8;
  const ROWS = 10;

  const focusCell = (r: number, c: number) => {
    const next = {
      r: Math.min(ROWS, Math.max(1, r)),
      c: Math.min(COLS, Math.max(1, c)),
    };
    setActive(next);
    setHover(next);
    cells.current[(next.r - 1) * COLS + next.c - 1]?.focus();
  };

  return (
    <div role="group" aria-label="Insert table" className="px-2 py-1.5">
      <div className="mb-1 text-center text-chrome-xs text-muted">
        {hover.r > 0 ? (
          <span className="font-mono">
            {hover.r} × {hover.c}
          </span>
        ) : (
          "Insert table"
        )}
      </div>
      <div
        role="group"
        aria-label="Table size"
        className="grid gap-0.5"
        style={{ gridTemplateColumns: `repeat(${COLS}, 1fr)` }}
        onMouseLeave={() => setHover({ r: 0, c: 0 })}
      >
        {Array.from({ length: ROWS * COLS }).map((_, i) => {
          const r = Math.floor(i / COLS) + 1;
          const c = (i % COLS) + 1;
          const on = r <= hover.r && c <= hover.c;
          return (
            <button
              key={i}
              ref={(cell) => {
                cells.current[i] = cell;
              }}
              type="button"
              aria-label={`Insert ${r} ${r === 1 ? "row" : "rows"} by ${c} ${c === 1 ? "column" : "columns"}`}
              tabIndex={active.r === r && active.c === c ? 0 : -1}
              onFocus={() => {
                setActive({ r, c });
                setHover({ r, c });
              }}
              onMouseEnter={() => setHover({ r, c })}
              onClick={(e) => {
                e.stopPropagation();
                onPick(r, c);
              }}
              onKeyDown={(e) => {
                if (e.key === "Enter" || e.key === " ") {
                  e.preventDefault();
                  e.stopPropagation();
                  onPick(r, c);
                  return;
                }
                if (e.key === "ArrowLeft") focusCell(r, c - 1);
                else if (e.key === "ArrowRight") focusCell(r, c + 1);
                else if (e.key === "ArrowUp") focusCell(r - 1, c);
                else if (e.key === "ArrowDown") focusCell(r + 1, c);
                else return;
                e.preventDefault();
              }}
              className={`h-6 w-6 border ${on ? "border-copper bg-copper/30" : "border-line"}`}
            />
          );
        })}
      </div>
    </div>
  );
}
