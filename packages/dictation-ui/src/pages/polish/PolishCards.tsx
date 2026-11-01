import { cx, ui } from "../../lib/ui";

// One smart-polish trigger. Same surface as every grouped list (ui.surface) so
// it contrasts with the page instead of nesting a darker box; equal height in
// a grid row via flex-col + the grid's default stretch.
export function SmartPolishCard({
  trigger,
  title,
  description,
}: {
  trigger: string;
  title: string;
  description: string;
}) {
  return (
    <div className={cx(ui.surface, "flex flex-col gap-[var(--space-2)] px-[var(--space-4)] py-[14px]")}>
      <div className="flex items-center justify-between gap-[var(--space-3)]">
        <span className="text-[length:var(--size-md)] font-semibold text-[var(--paper)]">
          {title}
        </span>
        <code
          className={cx(
            ui.mono,
            "shrink-0 rounded-[var(--radius-sm)] bg-[var(--surface-3)] px-[8px] py-[3px] text-[length:var(--size-xs)] text-[var(--muted)]",
          )}
        >
          {trigger}
        </code>
      </div>
      <p className="m-0 text-[length:var(--size-sm)] leading-[1.45] text-[var(--muted)] [text-wrap:pretty]">
        {description}
      </p>
    </div>
  );
}
