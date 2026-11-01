import type { RuntimeTab } from "../RuntimePane";

export function renderRows<T>(rows: T[], pick: (row: T) => [string, string, string]) {
  if (rows.length === 0)
    return (
      <div className="flex flex-col gap-2 m-auto text-center max-w-[400px] py-10">
        <p className="text-[11px] uppercase tracking-widest text-text-3 font-semibold">Empty</p>
        <h2 className="text-[20px] font-bold text-text">No runtime data yet</h2>
        <p className="text-[13px] text-text-2 leading-relaxed">
          Artifacts, learnings, benchmark results, and task graph nodes appear after a run.
        </p>
      </div>
    );

  return rows.map((row, index) => {
    const [title, detail, chip] = pick(row);
    return (
      <div
        className="grid grid-cols-[minmax(0,1fr)_auto_auto] items-center gap-4 px-5 py-3"
        key={title || String(index)}
      >
        <strong className="text-[13px] font-semibold text-text truncate">{title}</strong>
        <span className="text-[12px] text-text-3">{detail}</span>
        <code className="text-[11px] font-mono text-accent-2">{chip}</code>
      </div>
    );
  });
}

export function runtimeLabel(tab: RuntimeTab) {
  switch (tab) {
    case "taskgraph":
      return "Task graph";
    case "taste":
      return "Taste";
    case "metrics":
      return "Metrics";
    case "rollouts":
      return "Rollouts";
    case "finetuning":
      return "Fine-tuning";
    case "proposals":
      return "Proposals";
    case "compare":
      return "Compare";
    case "audit":
      return "Audit";
    case "messages":
      return "Messages";
    case "observability":
      return "Observability";
    case "harness":
      return "Harness";
    case "specs":
      return "Specs";
    case "recipes":
      return "Recipes";
    default:
      return tab[0].toUpperCase() + tab.slice(1);
  }
}
