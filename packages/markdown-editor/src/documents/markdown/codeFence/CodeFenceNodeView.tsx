import type { Node as ProseMirrorNode } from "prosemirror-model";
import type { EditorView, NodeView, ViewMutationRecord } from "prosemirror-view";
import { createRoot, type Root as ReactRoot } from "react-dom/client";
import { Dropdown, Item } from "@rightkit/document-core/documents/toolbarPrimitives";
import { setMarkdownTransactionIntent } from "../projection/markdownTransactionIntent";

export const CODE_FENCE_LANGUAGES: readonly string[] = [
  "ts",
  "tsx",
  "js",
  "jsx",
  "json",
  "yaml",
  "toml",
  "rust",
  "python",
  "go",
  "sh",
  "bash",
  "css",
  "scss",
  "html",
  "sql",
  "java",
  "kotlin",
  "c",
  "cpp",
  "csharp",
  "php",
  "ruby",
  "swift",
  "graphql",
  "markdown",
  "diff",
  "dockerfile",
  "text",
];

/** Options shown after the common ones, including the current language when custom. */
function languageOptions(language: string): readonly string[] {
  if (CODE_FENCE_LANGUAGES.includes(language)) return CODE_FENCE_LANGUAGES;
  return [language, ...CODE_FENCE_LANGUAGES];
}

/**
 * Compact header for a native `code_fence` NodeView (contract 14 §5.2).
 * A small "language ▾" chip uses the shared Dropdown/Item behavior; selecting a
 * language updates the node attrs through one ProseMirror transaction stamped
 * with a `Format` intent. The chip never steals the code-block selection.
 */
function CodeFenceLanguageChip({
  language,
  onChange,
}: {
  language: string;
  onChange: (language: string) => void;
}) {
  return (
    <Dropdown label={language || "text"} title="Code language" minWidth={0} panelWidth={180}>
      {(close) => (
        <>
          {languageOptions(language).map((lang) => (
            <Item
              key={lang}
              active={lang === language}
              onRun={() => {
                if (lang !== language) onChange(lang);
                close();
              }}
            >
              {lang || "text"}
            </Item>
          ))}
        </>
      )}
    </Dropdown>
  );
}

export interface CodeFenceNodeViewOptions {
  /** When set, dispatch a node-attr transaction with a format intent (CE-2A). */
  stampIntent?: boolean;
}

/**
 * ProseMirror NodeView for the native editable `code_fence` block.
 *
 * - `contentDOM` is the `<code>` inside the `<pre>`; ProseMirror owns text.
 * - The language chip is compact chrome that does not enlarge the block and
 *   never steals the caret/selection.
 * - No nested CodeMirror/ProseMirror editor is mounted (contract 14 §5.2).
 */
export class CodeFenceNodeView implements NodeView {
  dom: HTMLElement;
  contentDOM: HTMLElement;
  private node: ProseMirrorNode;
  private view: EditorView;
  private getPos: () => number | undefined;
  private options: CodeFenceNodeViewOptions;
  private header: HTMLElement;
  private headerRoot: ReactRoot | null = null;

  constructor(
    node: ProseMirrorNode,
    view: EditorView,
    getPos: () => number | undefined,
    options: CodeFenceNodeViewOptions = {},
  ) {
    this.node = node;
    this.view = view;
    this.getPos = getPos;
    this.options = options;

    const wrapper = document.createElement("div");
    wrapper.className = "vr-pm-code-fence";
    wrapper.dataset.vrCodeFence = "true";

    this.header = document.createElement("div");
    this.header.className = "vr-pm-code-fence__header";
    wrapper.appendChild(this.header);

    const pre = document.createElement("pre");
    pre.className = "vr-pm-code-fence__pre";
    const code = document.createElement("code");
    code.className = "vr-pm-code-fence__code";
    pre.appendChild(code);
    wrapper.appendChild(pre);

    this.dom = wrapper;
    this.contentDOM = code;
    this.renderHeader();
  }

  update(node: ProseMirrorNode): boolean {
    if (node.type.name !== "code_fence") return false;
    this.node = node;
    this.renderHeader();
    return true;
  }

  stopEvent(event: Event): boolean {
    // Interactions inside the header chip (dropdown) are handled by the chip, not PM.
    const target = event.target as HTMLElement | null;
    if (target && this.header.contains(target)) return true;
    return false;
  }

  ignoreMutation(mutation: ViewMutationRecord): boolean {
    return this.header.contains(mutation.target as Node);
  }

  destroy(): void {
    const root = this.headerRoot;
    this.headerRoot = null;
    if (root) queueMicrotask(() => root.unmount());
  }

  private renderHeader(): void {
    if (!this.headerRoot) {
      this.headerRoot = createRoot(this.header);
    }
    const language = String(this.node.attrs.language ?? "");
    const onChange = (next: string) => {
      const pos = this.getPos();
      if (typeof pos !== "number") return;
      let tr = this.view.state.tr.setNodeMarkup(pos, undefined, {
        ...this.node.attrs,
        language: next,
      });
      if (this.options.stampIntent) tr = setMarkdownTransactionIntent(tr, "format");
      this.view.dispatch(tr);
      this.view.focus();
    };
    this.headerRoot.render(<CodeFenceLanguageChip language={language} onChange={onChange} />);
  }
}
