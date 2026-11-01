import { useMemo, useState, type KeyboardEvent, type ReactNode } from "react";
import type { RootContent } from "mdast";
import type { MarkdownDocumentCore } from "../../core/MarkdownDocumentCore";
import { buildBlocks } from "../../semanticBlocks";
import { parseToTree } from "../../parseMarkdown";
import type { FaqData } from "../../semanticBlocks";
import type { StepperData } from "../../semanticBlocks";
import type { ComparisonData } from "../../semanticBlocks";
import type { RichTableData } from "../../semanticBlocks";
import { useMarkdownBlockDraft } from "../useMarkdownBlockDraft";
import { MarkdownBlockDraftStatus } from "../MarkdownBlockDraftStatus";

interface SemanticEditorProps {
  core: MarkdownDocumentCore;
  range: { from: number; to: number };
  onClose?: () => void;
}

type BlockModel<T> = T | null;

function useBlockModel<T>(
  core: MarkdownDocumentCore,
  range: { from: number; to: number },
  kind: string,
): BlockModel<T> {
  return useMemo(() => {
    const snap = core.snapshot();
    const source = snap.logicalSource.slice(range.from, range.to);
    const tree = parseToTree(source);
    const blocks = buildBlocks(tree.children);
    for (const block of blocks) {
      if (block.kind === "semantic" && block.hit.kind === kind) {
        return block.hit.data as T;
      }
    }
    return null;
  }, [core, range.from, range.to, kind]);
}

type SemanticDraft = ReturnType<typeof useMarkdownBlockDraft>;

function DraftActions({ draft, onClose }: { draft: SemanticDraft; onClose?: () => void }) {
  const done = async () => {
    const result = await draft.done();
    if (result.outcome === "done") onClose?.();
  };
  const discard = async () => {
    const result = await draft.discard();
    if (result.outcome === "discarded" || result.outcome === "missing") onClose?.();
  };
  return (
    <>
      <MarkdownBlockDraftStatus snapshot={draft.snapshot} />
      <div className="vr-protected-editor-actions">
        <button type="button" className="vr-protected-editor-button" onClick={() => void discard()}>
          Cancel
        </button>
        <button
          type="button"
          className="vr-protected-editor-button vr-protected-editor-button--primary"
          onClick={() => void done()}
        >
          Done
        </button>
      </div>
    </>
  );
}

function discardOnEscape(event: KeyboardEvent, draft: SemanticDraft, onClose?: () => void) {
  if ((event.metaKey || event.ctrlKey) && event.key === "Enter") {
    event.preventDefault();
    void draft.done().then((result) => {
      if (result.outcome === "done") onClose?.();
    });
    return;
  }
  if (event.key !== "Escape") return;
  event.preventDefault();
  void draft.discard().then((result) => {
    if (result.outcome === "discarded" || result.outcome === "missing") onClose?.();
  });
}

function textFromRootContent(nodes: RootContent[]): string {
  return nodes
    .map((node) => {
      if ("children" in node && Array.isArray(node.children)) {
        return node.children.map((child) => ("value" in child ? child.value : "")).join("");
      }
      return "";
    })
    .join("\n");
}

export function MarkdownFaqEditor({ core, range, onClose }: SemanticEditorProps): ReactNode {
  const initial = useBlockModel<FaqData>(core, range, "faq");
  const [items, setItems] = useState<{ question: string; answer: string }[]>(
    () =>
      initial?.items.map((item) => ({
        question: item.question,
        answer: textFromRootContent(item.answer),
      })) ?? [],
  );
  const draft = useMarkdownBlockDraft(core, "faq", range);

  const commit = (next: { question: string; answer: string }[]) => {
    setItems(next);
    draft.update(
      serializeFaq({
        items: next.map((item) => ({
          question: item.question,
          answer: [paragraphNode(item.answer)],
        })),
      }),
    );
  };

  return (
    <div
      data-testid="markdown-faq-editor"
      className="space-y-2"
      onKeyDown={(event) => discardOnEscape(event, draft, onClose)}
    >
      {items.map((item, i) => (
        <div key={i} className="rounded border border-line p-2">
          <input
            type="text"
            value={item.question}
            placeholder="Question"
            onChange={(e) =>
              commit(items.map((it, j) => (j === i ? { ...it, question: e.target.value } : it)))
            }
            className="vr-protected-editor-input mb-1 w-full"
          />
          <textarea
            value={item.answer}
            placeholder="Answer"
            onChange={(e) =>
              commit(items.map((it, j) => (j === i ? { ...it, answer: e.target.value } : it)))
            }
            rows={3}
            className="vr-protected-editor-textarea w-full"
          />
        </div>
      ))}
      <DraftActions draft={draft} onClose={onClose} />
    </div>
  );
}

export function MarkdownStepperEditor({ core, range, onClose }: SemanticEditorProps): ReactNode {
  const initial = useBlockModel<StepperData>(core, range, "stepper");
  const [title, setTitle] = useState(initial?.title ?? "");
  const [steps, setSteps] = useState(initial?.steps ?? []);
  const draft = useMarkdownBlockDraft(core, "stepper", range);

  const commit = (nextTitle: string, nextSteps: string[]) => {
    setTitle(nextTitle);
    setSteps(nextSteps);
    draft.update(serializeStepper({ title: nextTitle || null, steps: nextSteps }));
  };

  return (
    <div
      data-testid="markdown-stepper-editor"
      className="space-y-2"
      onKeyDown={(event) => discardOnEscape(event, draft, onClose)}
    >
      <input
        type="text"
        value={title}
        placeholder="Title"
        onChange={(e) => commit(e.target.value, steps)}
        className="vr-protected-editor-input w-full font-semibold"
      />
      {steps.map((step, i) => (
        <input
          key={i}
          type="text"
          value={step}
          placeholder={`Step ${i + 1}`}
          onChange={(e) =>
            commit(
              title,
              steps.map((s, j) => (j === i ? e.target.value : s)),
            )
          }
          className="vr-protected-editor-input w-full"
        />
      ))}
      <DraftActions draft={draft} onClose={onClose} />
    </div>
  );
}

export function MarkdownComparisonEditor({ core, range, onClose }: SemanticEditorProps): ReactNode {
  const initial = useBlockModel<ComparisonData>(core, range, "comparison");
  const [leftTitle, setLeftTitle] = useState(initial?.left.title ?? "");
  const [rightTitle, setRightTitle] = useState(initial?.right.title ?? "");
  const [leftItems, setLeftItems] = useState(initial?.left.items ?? []);
  const [rightItems, setRightItems] = useState(initial?.right.items ?? []);
  const draft = useMarkdownBlockDraft(core, "comparison", range);

  const commit = (next: {
    leftTitle: string;
    rightTitle: string;
    leftItems: string[];
    rightItems: string[];
  }) => {
    setLeftTitle(next.leftTitle);
    setRightTitle(next.rightTitle);
    setLeftItems(next.leftItems);
    setRightItems(next.rightItems);
    draft.update(
      serializeComparison({
        left: { title: next.leftTitle, items: next.leftItems },
        right: { title: next.rightTitle, items: next.rightItems },
        tone: initial?.tone ?? "neutral",
      }),
    );
  };

  return (
    <div
      data-testid="markdown-comparison-editor"
      className="grid grid-cols-2 gap-2"
      onKeyDown={(event) => discardOnEscape(event, draft, onClose)}
    >
      <div>
        <input
          type="text"
          value={leftTitle}
          onChange={(e) => commit({ leftTitle: e.target.value, rightTitle, leftItems, rightItems })}
          className="vr-protected-editor-input w-full font-semibold"
        />
        {leftItems.map((item, i) => (
          <input
            key={i}
            type="text"
            value={item}
            onChange={(e) =>
              commit({
                leftTitle,
                rightTitle,
                leftItems: leftItems.map((it, j) => (j === i ? e.target.value : it)),
                rightItems,
              })
            }
            className="vr-protected-editor-input w-full"
          />
        ))}
      </div>
      <DraftActions draft={draft} onClose={onClose} />
      <div>
        <input
          type="text"
          value={rightTitle}
          onChange={(e) => commit({ leftTitle, rightTitle: e.target.value, leftItems, rightItems })}
          className="vr-protected-editor-input w-full font-semibold"
        />
        {rightItems.map((item, i) => (
          <input
            key={i}
            type="text"
            value={item}
            onChange={(e) =>
              commit({
                leftTitle,
                rightTitle,
                leftItems,
                rightItems: rightItems.map((it, j) => (j === i ? e.target.value : it)),
              })
            }
            className="vr-protected-editor-input w-full"
          />
        ))}
      </div>
    </div>
  );
}

export function MarkdownRichTableEditor({ core, range, onClose }: SemanticEditorProps): ReactNode {
  const initial = useBlockModel<RichTableData>(core, range, "richTable");
  const [headers, setHeaders] = useState(initial?.headers ?? []);
  const [rows, setRows] = useState(initial?.rows ?? []);
  const draft = useMarkdownBlockDraft(core, "richTable", range);

  const commit = (nextHeaders: string[], nextRows: string[][]) => {
    setHeaders(nextHeaders);
    setRows(nextRows);
    draft.update(
      serializeRichTable({
        headers: nextHeaders,
        rows: nextRows,
        numericColumns: [],
        align: [],
      }),
    );
  };

  return (
    <div
      data-testid="markdown-rich-table-editor"
      onKeyDown={(event) => discardOnEscape(event, draft, onClose)}
    >
      <table className="w-full border-collapse">
        <thead>
          <tr>
            {headers.map((h, i) => (
              <th key={i}>
                <input
                  type="text"
                  value={h}
                  onChange={(e) =>
                    commit(
                      headers.map((h2, j) => (j === i ? e.target.value : h2)),
                      rows,
                    )
                  }
                  className="vr-protected-editor-input w-full"
                />
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, ri) => (
            <tr key={ri}>
              {row.map((cell, ci) => (
                <td key={ci}>
                  <input
                    type="text"
                    value={cell}
                    onChange={(e) =>
                      commit(
                        headers,
                        rows.map((r, rj) =>
                          rj === ri ? r.map((c, ck) => (ck === ci ? e.target.value : c)) : r,
                        ),
                      )
                    }
                    className="vr-protected-editor-input w-full"
                  />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      <DraftActions draft={draft} onClose={onClose} />
    </div>
  );
}

function paragraphNode(text: string): {
  type: "paragraph";
  children: { type: "text"; value: string }[];
} {
  return { type: "paragraph", children: [{ type: "text", value: text }] };
}

function serializeFaq(data: FaqData): string {
  return data.items
    .map((item) => `### Q: ${item.question}\n\n${textFromRootContent(item.answer)}`)
    .join("\n\n");
}

function serializeStepper(data: StepperData): string {
  const lines: string[] = data.title ? [`# ${data.title}`] : [];
  data.steps.forEach((step, i) => lines.push(`${i + 1}. ${step}`));
  return lines.join("\n");
}

function serializeComparison(data: ComparisonData): string {
  const lines: string[] = [];
  lines.push(`# ${data.left.title}`, "");
  data.left.items.forEach((item) => lines.push(`- ${item}`));
  lines.push("", `# ${data.right.title}`, "");
  data.right.items.forEach((item) => lines.push(`- ${item}`));
  return lines.join("\n");
}

function escapeMarkdownCell(text: string): string {
  return text.replace(/\|/g, "\\|").replace(/\n/g, " ");
}

function serializeRichTable(data: RichTableData): string {
  const rows: string[] = [];
  rows.push(`| ${data.headers.map(escapeMarkdownCell).join(" | ")} |`);
  rows.push(`| ${data.headers.map(() => "---").join(" | ")} |`);
  data.rows.forEach((row) => {
    rows.push(`| ${row.map(escapeMarkdownCell).join(" | ")} |`);
  });
  return rows.join("\n");
}
