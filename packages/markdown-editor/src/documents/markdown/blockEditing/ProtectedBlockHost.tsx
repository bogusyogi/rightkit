/**
 * Protected-block host.
 *
 * Mounts the right editor shell for a given block kind. The host
 * is the only component that knows about every protected-block
 * editor; new editors are added by extending the `switch`
 * statement below.
 */

import type { ReactNode } from "react";
import { MarkdownTableEditor } from "./table/MarkdownTableEditor.js";
import { MarkdownRawFenceEditor } from "./code/MarkdownRawFenceEditor.js";
import { MarkdownMathBlockEditor } from "./math/MarkdownMathBlockEditor.js";
import { MarkdownMermaidEditor } from "./mermaid/MarkdownMermaidEditor.js";
import { MarkdownRawBlockEditor } from "./raw/MarkdownRawBlockEditor.js";
import {
  MarkdownFaqEditor,
  MarkdownStepperEditor,
  MarkdownComparisonEditor,
  MarkdownRichTableEditor,
} from "./semantic/MarkdownSemanticEditors.js";
import type { MarkdownDocumentCore } from "../core/MarkdownDocumentCore.js";
import type { BlockKind } from "./types.js";

export interface ProtectedBlockHostProps {
  readonly core: MarkdownDocumentCore;
  readonly kind: BlockKind;
  readonly range: { from: number; to: number };
  readonly onClose?: () => void;
}

/**
 * Render the editor shell for `kind`. The host returns `null`
 * for kinds that have no dedicated editor; the PM NodeView then
 * falls back to the default source-text rendering.
 */
export function ProtectedBlockHost({
  core,
  kind,
  range,
  onClose,
}: ProtectedBlockHostProps): ReactNode {
  let editor: ReactNode;
  switch (kind) {
    case "table":
      editor = <MarkdownTableEditor core={core} range={range} onClose={onClose} />;
      break;
    case "code-raw-fallback":
      editor = <MarkdownRawFenceEditor core={core} range={range} onClose={onClose} />;
      break;
    case "math":
      editor = <MarkdownMathBlockEditor core={core} range={range} onClose={onClose} />;
      break;
    case "mermaid":
      editor = <MarkdownMermaidEditor core={core} range={range} onClose={onClose} />;
      break;
    case "faq":
      editor = <MarkdownFaqEditor core={core} range={range} onClose={onClose} />;
      break;
    case "stepper":
      editor = <MarkdownStepperEditor core={core} range={range} onClose={onClose} />;
      break;
    case "comparison":
      editor = <MarkdownComparisonEditor core={core} range={range} onClose={onClose} />;
      break;
    case "richTable":
      editor = <MarkdownRichTableEditor core={core} range={range} onClose={onClose} />;
      break;
    case "raw":
      editor = <MarkdownRawBlockEditor core={core} range={range} onClose={onClose} />;
      break;
    case "footnote-def":
    case "footnote-ref":
      // Footnote definitions/refs render inline; no dedicated
      // shell.
      return null;
    default:
      return null;
  }
  return (
    <section className="vr-protected-editor-shell" data-vr-protected-editor={kind}>
      {editor}
    </section>
  );
}
