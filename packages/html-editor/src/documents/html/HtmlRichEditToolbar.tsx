import type { RefObject } from "react";
import { ColorGrid, Dropdown, Group, Item, Sep, TBtn } from "@rightkit/document-core/documents/toolbarPrimitives";
import { DocumentToolbarShell } from "@rightkit/document-core/documents/DocumentToolbarShell";
import type { RichBlockOp } from "./htmlOperationPlanner.js";
import type { HtmlEditorToolbarState, HtmlSafeBlockEditorHandle } from "./HtmlSafeBlockEditor.js";
import {
  HTML_HIGHLIGHT_COLORS,
  HTML_RICH_FONTS,
  HTML_RICH_SIZES,
  HTML_TEXT_COLORS,
} from "./htmlTextStyle.js";
import {
  IconAddLink,
  IconCancel,
  IconDone,
  IconEditLink,
  IconHighlight,
  IconInsertImage,
  IconRemoveLink,
  IconReplaceImage,
} from "./htmlToolbarIcons.js";

/**
 * Inline FORMAT bar for an active HTML block session — marks, font/color, links, image, and the
 * session Cancel/Done controls. Block-shape commands (type, move, insert, wrap, delete) live in
 * the separate `HtmlBlockEditToolbar` (findings §2 item 5: split contextual block bar from inline
 * format bar instead of one flat scrolling strip).
 */
export function HtmlRichEditToolbar({
  handleRef,
  anchorHref,
  anchorSrc,
  onRichOp,
  canRichOp,
  formatting,
}: {
  handleRef: RefObject<HtmlSafeBlockEditorHandle | null>;
  anchorHref?: string;
  anchorSrc?: string;
  onRichOp?: (op: RichBlockOp) => void;
  /** Planner-backed applicability supplied by HtmlDocument; never duplicate HTML rules here. */
  canRichOp?: (op: RichBlockOp) => boolean;
  /** Derived from current transient PM state, not copied into HTML source semantics. */
  formatting?: HtmlEditorToolbarState;
}) {
  const runRich = (op: RichBlockOp) => onRichOp?.(op);
  const richEnabled = (op: RichBlockOp) => canRichOp?.(op) ?? true;
  const runHandle = (fn: (handle: HtmlSafeBlockEditorHandle) => void) => {
    const handle = handleRef.current;
    if (handle) fn(handle);
  };
  const closeAfter = (close: () => void, fn: () => void) => {
    fn();
    close();
  };

  return (
    <DocumentToolbarShell
      ariaLabel="HTML rich edit"
      variant="scroll"
      className="vr-capsule-scroll text-xs"
      onEscape={() => runHandle((handle) => void handle.cancel())}
    >
      <span className="shrink-0 rounded border border-line bg-panel-2 px-2 py-1 font-mono text-chrome-xs uppercase tracking-wide text-muted">
        HTML
      </span>

      <Dropdown
        label={
          formatting?.textStyle.fontFamily.mixed
            ? "Mixed"
            : (formatting?.textStyle.fontFamily.value ?? "Font")
        }
        title="Font family"
        minWidth={104}
        panelWidth={150}
      >
        {(close) => (
          <>
            {HTML_RICH_FONTS.map((name) => (
              <Item
                key={name}
                active={formatting?.textStyle.fontFamily.value === name}
                onRun={() =>
                  closeAfter(close, () =>
                    runHandle((handle) => handle.applyTextStyle("fontFamily", name)),
                  )
                }
                style={{ fontFamily: name }}
              >
                {name}
              </Item>
            ))}
            <Item
              onRun={() =>
                closeAfter(close, () =>
                  runHandle((handle) => handle.applyTextStyle("fontFamily", null)),
                )
              }
            >
              Clear font
            </Item>
          </>
        )}
      </Dropdown>
      <Dropdown
        label={
          formatting?.textStyle.fontSize.mixed
            ? "Mixed"
            : (formatting?.textStyle.fontSize.value?.replace(/pt$/i, "") ?? "Size")
        }
        title="Font size"
        minWidth={56}
        panelWidth={72}
      >
        {(close) => (
          <>
            {HTML_RICH_SIZES.map((size) => (
              <Item
                key={size}
                active={formatting?.textStyle.fontSize.value === `${size}pt`}
                onRun={() =>
                  closeAfter(close, () =>
                    runHandle((handle) => handle.applyTextStyle("fontSize", `${size}pt`)),
                  )
                }
              >
                {size}
              </Item>
            ))}
            <Item
              onRun={() =>
                closeAfter(close, () =>
                  runHandle((handle) => handle.applyTextStyle("fontSize", null)),
                )
              }
            >
              Clear size
            </Item>
          </>
        )}
      </Dropdown>

      <Sep />
      <Group>
        <TBtn
          label="B"
          ariaLabel="Bold"
          title="Bold"
          bold
          active={formatting?.marks.strong.active}
          disabled={formatting ? !formatting.marks.strong.enabled : false}
          onRun={() => runHandle((handle) => handle.toggleMark("strong"))}
        />
        <TBtn
          label="I"
          ariaLabel="Italic"
          title="Italic"
          italic
          active={formatting?.marks.em.active}
          disabled={formatting ? !formatting.marks.em.enabled : false}
          onRun={() => runHandle((handle) => handle.toggleMark("em"))}
        />
        <TBtn
          label="</>"
          ariaLabel="Inline code"
          title="Inline code"
          width={36}
          active={formatting?.marks.code.active}
          disabled={formatting ? !formatting.marks.code.enabled : false}
          onRun={() => runHandle((handle) => handle.toggleMark("code"))}
        />
        <TBtn
          label="S"
          ariaLabel="Strikethrough"
          title="Strikethrough"
          strike
          active={formatting?.marks.strike.active}
          disabled={formatting ? !formatting.marks.strike.enabled : false}
          onRun={() => runHandle((handle) => handle.toggleMark("strike"))}
        />
        <TBtn
          label="U"
          ariaLabel="Underline"
          title="Underline"
          underline
          active={formatting?.marks.underline.active}
          disabled={formatting ? !formatting.marks.underline.enabled : false}
          onRun={() => runHandle((handle) => handle.toggleMark("underline"))}
        />
      </Group>

      <Sep />
      <Group>
        <Dropdown label="A" title="Text color" minWidth={40} panelWidth={188}>
          {(close) => (
            <ColorGrid
              colors={[...HTML_TEXT_COLORS]}
              onPick={(hex) =>
                closeAfter(close, () =>
                  runHandle((handle) => handle.applyTextStyle("color", `#${hex}`)),
                )
              }
              onClear={() =>
                closeAfter(close, () => runHandle((handle) => handle.applyTextStyle("color", null)))
              }
              clearLabel="Automatic color"
            />
          )}
        </Dropdown>
        <Dropdown label={<IconHighlight />} title="Highlight" minWidth={40} panelWidth={188}>
          {(close) => (
            <ColorGrid
              colors={[...HTML_HIGHLIGHT_COLORS]}
              onPick={(hex) =>
                closeAfter(close, () =>
                  runHandle((handle) => handle.applyTextStyle("backgroundColor", `#${hex}`)),
                )
              }
              onClear={() =>
                closeAfter(close, () =>
                  runHandle((handle) => handle.applyTextStyle("backgroundColor", null)),
                )
              }
              clearLabel="No highlight"
            />
          )}
        </Dropdown>
      </Group>

      <Sep />
      <Group>
        <TBtn
          label={<IconInsertImage />}
          ariaLabel="Insert image"
          title="Insert an image at the end of this block"
          disabled={!richEnabled({ kind: "imageInsert", src: "image.png", position: "end" })}
          onRun={() => runHandle((handle) => handle.requestInsertImage())}
        />
        <TBtn
          label={<IconAddLink />}
          ariaLabel="Add link"
          title="Wrap the selected text in a link"
          disabled={formatting ? !formatting.canAddLink : false}
          onRun={() => runHandle((handle) => handle.requestAddLink())}
        />
        {anchorHref && (
          <>
            <TBtn
              label={<IconEditLink />}
              ariaLabel="Edit link"
              title="Change the link URL"
              onRun={() => runHandle((handle) => handle.requestEditLink(anchorHref))}
            />
            <TBtn
              label={<IconRemoveLink />}
              ariaLabel="Remove link"
              title="Remove the link but keep its text"
              onRun={() => runRich({ kind: "removeLink" })}
            />
          </>
        )}
        {anchorSrc && (
          <TBtn
            label={<IconReplaceImage />}
            ariaLabel="Replace image"
            title="Replace the image src"
            onRun={() => runHandle((handle) => handle.requestReplaceImage(anchorSrc))}
          />
        )}
      </Group>

      <Sep />
      <Group>
        <TBtn
          label={<IconCancel />}
          ariaLabel="Cancel"
          title="Discard changes and close"
          onRun={() => runHandle((handle) => void handle.cancel())}
        />
        <TBtn
          label={<IconDone />}
          ariaLabel="Done"
          title="Commit changes and close"
          onRun={() => runHandle((handle) => void handle.done())}
        />
      </Group>
    </DocumentToolbarShell>
  );
}
