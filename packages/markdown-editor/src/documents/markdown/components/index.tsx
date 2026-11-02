import { useMemo, useState } from "react";
import type { RootContent } from "mdast";
import type { Block } from "../semanticBlocks.js";
import { renderNodes } from "../renderNodes.js";
import { parseToTree } from "../parseMarkdown.js";
import type { CalloutData } from "../semanticBlocks.js";
import type { FrontmatterData } from "../semanticBlocks.js";
import type { StepperData } from "../semanticBlocks.js";
import type { ComparisonData } from "../semanticBlocks.js";
import type { FaqData } from "../semanticBlocks.js";
import type { RichTableData } from "../semanticBlocks.js";
import type { DescriptionListData } from "../semanticBlocks.js";

const CALLOUT_TINT: Record<string, string> = {
  NOTE: "border-muted-2",
  TIP: "border-copper",
  IMPORTANT: "border-copper",
  WARNING: "border-amber-500/60",
  CAUTION: "border-red-500/60",
};

export function SemanticBlock({ block }: { block: Extract<Block, { kind: "semantic" }> }) {
  const { hit } = block;
  switch (hit.kind) {
    case "frontmatter":
      return <Frontmatter data={hit.data as FrontmatterData} />;
    case "callout":
      return <Callout data={hit.data as CalloutData} />;
    case "stepper":
      return <Stepper data={hit.data as StepperData} />;
    case "comparison":
      return <Comparison data={hit.data as ComparisonData} />;
    case "faq":
      return <Faq data={hit.data as FaqData} />;
    case "richTable":
      return <RichTable data={hit.data as RichTableData} />;
    case "descriptionList":
      return <DescriptionList data={hit.data as DescriptionListData} />;
    default:
      return null;
  }
}

function Frontmatter({ data }: { data: FrontmatterData }) {
  return (
    <div className="my-4 rounded-lg border border-line bg-panel p-4">
      <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-1 font-mono text-sm">
        {data.fields.map((f) => (
          <FrontmatterRow key={f.key} k={f.key} v={f.value} />
        ))}
      </dl>
    </div>
  );
}
function FrontmatterRow({ k, v }: { k: string; v: string }) {
  // Render the value as inline Markdown so a link/emphasis a user adds in the title actually shows
  // (e.g. [Tour](url) → a real link), instead of literal "[Tour](url)" text.
  const rendered = useMemo(() => {
    const first = parseToTree(v).children[0];
    return first && first.type === "paragraph"
      ? renderNodes((first.children as RootContent[]) ?? [])
      : v;
  }, [v]);
  return (
    <>
      <dt className="text-muted-2">{k}</dt>
      <dd className="text-paper">{rendered}</dd>
    </>
  );
}

function Callout({ data }: { data: CalloutData }) {
  return (
    <div
      className={`my-4 rounded-lg border bg-panel p-4 ${CALLOUT_TINT[data.type] ?? "border-muted-2"}`}
    >
      <div className="mb-1 font-mono text-xs uppercase tracking-[0.2em] text-copper">
        {data.title ?? data.type}
      </div>
      <div className="text-paper/90">{renderNodes(data.body)}</div>
    </div>
  );
}

function Stepper({ data }: { data: StepperData }) {
  return (
    <div className="my-4">
      {data.title && <div className="mb-3 font-serif text-lg font-semibold">{data.title}</div>}
      <ol className="space-y-3">
        {data.steps.map((s, i) => (
          <li key={i} className="flex gap-3">
            <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-copper/20 font-mono text-xs text-copper-hi">
              {i + 1}
            </span>
            <span className="pt-0.5 leading-relaxed">{s}</span>
          </li>
        ))}
      </ol>
    </div>
  );
}

function Comparison({ data }: { data: ComparisonData }) {
  const tint = (side: "left" | "right") =>
    data.tone === "pro-con"
      ? side === "left"
        ? "border-emerald-500/40"
        : "border-red-500/40"
      : "border-line";
  return (
    <div className="my-4 grid grid-cols-1 gap-4 sm:grid-cols-2">
      {(["left", "right"] as const).map((side) => (
        <div key={side} className={`rounded-lg border bg-panel p-4 ${tint(side)}`}>
          <div className="mb-2 font-serif font-semibold">{data[side].title}</div>
          <ul className="list-disc space-y-1 pl-5 text-sm">
            {data[side].items.map((it, i) => (
              <li key={i}>{it}</li>
            ))}
          </ul>
        </div>
      ))}
    </div>
  );
}

function Faq({ data }: { data: FaqData }) {
  return (
    <div className="my-4 divide-y divide-line rounded-lg border border-line">
      {data.items.map((item, i) => (
        <FaqRow key={i} q={item.question} a={item.answer} />
      ))}
    </div>
  );
}
function FaqRow({ q, a }: { q: string; a: FaqData["items"][number]["answer"] }) {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button
        type="button"
        onClick={() => setOpen((o) => !o)}
        aria-expanded={open}
        title={open ? "Collapse answer" : "Expand answer"}
        className="vr-press vr-focus flex w-full items-center justify-between rounded px-4 py-3 text-left font-medium hover:text-copper-hi"
      >
        {q}
        <span className="font-mono text-muted-2">{open ? "−" : "+"}</span>
      </button>
      {open && <div className="px-4 pb-3 text-paper/90">{renderNodes(a)}</div>}
    </div>
  );
}

function RichTable({ data }: { data: RichTableData }) {
  const [sort, setSort] = useState<{ col: number; dir: 1 | -1 } | null>(null);

  const rows = [...data.rows];
  if (sort) {
    const { col, dir } = sort;
    const numeric = data.numericColumns[col];
    rows.sort((a, b) => {
      const av = a[col] ?? "";
      const bv = b[col] ?? "";
      if (numeric)
        return (Number(av.replace(/[,$%\s]/g, "")) - Number(bv.replace(/[,$%\s]/g, ""))) * dir;
      return av.localeCompare(bv) * dir;
    });
  }

  const colMax = data.numericColumns.map((isNum, c) =>
    isNum
      ? Math.max(...data.rows.map((r) => Number((r[c] ?? "0").replace(/[,$%\s]/g, "")) || 0))
      : 0,
  );

  return (
    <div className="my-4 overflow-auto rounded-lg border border-line">
      <table className="w-full border-collapse text-sm">
        <thead className="sticky top-0 bg-panel">
          <tr>
            {data.headers.map((h, c) => (
              <th
                key={c}
                onClick={() =>
                  setSort((s) => ({ col: c, dir: s?.col === c && s.dir === 1 ? -1 : 1 }))
                }
                onKeyDown={(e) => {
                  if (e.key === "Enter" || e.key === " ") {
                    e.preventDefault();
                    setSort((s) => ({ col: c, dir: s?.col === c && s.dir === 1 ? -1 : 1 }));
                  }
                }}
                aria-sort={sort?.col === c ? (sort.dir === 1 ? "ascending" : "descending") : "none"}
                title={`Sort by ${h}`}
                className="vr-focus cursor-pointer select-none px-3 py-2 text-left font-mono text-xs uppercase tracking-wide text-muted hover:text-paper"
              >
                {h}
                {sort?.col === c ? (sort.dir === 1 ? " ▲" : " ▼") : ""}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((r, ri) => (
            <tr key={ri} className="border-t border-line">
              {r.map((cell, c) => (
                <td key={c} className="px-3 py-1.5">
                  {data.numericColumns[c] && colMax[c] > 0 ? (
                    <div className="flex items-center gap-2">
                      <span className="tabular-nums">{cell}</span>
                      <span
                        className="h-1.5 rounded bg-copper/50"
                        style={{
                          width: `${Math.max(2, (Number(cell.replace(/[,$%\s]/g, "")) / colMax[c]) * 60)}px`,
                        }}
                      />
                    </div>
                  ) : (
                    cell
                  )}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function DescriptionList({ data }: { data: DescriptionListData }) {
  return (
    <dl className="my-4 space-y-2">
      {data.items.map((item, i) => (
        <div key={i} className="grid grid-cols-1 gap-1 sm:grid-cols-[max-content_1fr]">
          {item.terms.map((term, ti) => (
            <dt key={ti} className="font-semibold text-paper">
              {term}
            </dt>
          ))}
          {item.details.map((detail, di) => (
            <dd key={di} className="text-paper/90">
              {detail}
            </dd>
          ))}
        </div>
      ))}
    </dl>
  );
}
