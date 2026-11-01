import Icon, { type IconName } from "@rightkit/agent-ui-core/components/Icon";
import type { ComposerCompletion, ComposerCompletionKind } from "@rightkit/agent-ui-core/lib/composer/completionRanking";

export interface ComposerCompletionMenuProps {
  items: readonly ComposerCompletion[];
  selectedIndex: number;
  onSelectIndex: (index: number) => void;
  onPick: (item: ComposerCompletion) => void;
  onClose?: () => void;
  ariaLabel?: string;
}

function iconFor(kind: ComposerCompletionKind): IconName {
  return kind === "command" ? "terminal" : kind === "skill" ? "skill" : "folder";
}

function kindLabel(kind: ComposerCompletionKind) {
  return kind === "command" ? "Command" : kind === "skill" ? "Skill" : "Workspace";
}

export function ComposerCompletionMenu({
  items,
  selectedIndex,
  onSelectIndex,
  onPick,
  onClose,
  ariaLabel = "Composer completions",
}: ComposerCompletionMenuProps) {
  if (items.length === 0) return null;
  return (
    <div
      role="listbox"
      aria-label={ariaLabel}
      data-testid="composer-completion-menu"
      onMouseDown={(event) => event.preventDefault()}
      className="absolute bottom-full left-3 right-3 z-40 mb-1.5 max-h-[300px] overflow-y-auto rounded-xl border border-line bg-panel py-1 shadow-app"
    >
      {items.map((item, index) => {
        const selected = index === selectedIndex;
        return (
          <button
            key={item.key}
            type="button"
            role="option"
            aria-selected={selected}
            data-testid={`composer-completion-${item.kind}-${index}`}
            className={`mx-1 flex w-[calc(100%-8px)] items-center gap-2.5 rounded-lg px-2.5 py-2 text-left ${
              selected ? "bg-panel-2 text-text" : "text-text-2 hover:bg-panel-2 hover:text-text"
            }`}
            onMouseEnter={() => onSelectIndex(index)}
            onFocus={() => onSelectIndex(index)}
            onClick={() => {
              onPick(item);
              onClose?.();
            }}
          >
            <span className="shrink-0 text-text-3"><Icon name={iconFor(item.kind)} size={14} /></span>
            <span className="min-w-0 flex-1">
              <span className="block truncate text-[13px] font-medium text-text">{item.label}</span>
              <span className="block truncate text-[11px] text-text-3">{item.description}</span>
            </span>
            <span className="shrink-0 text-[10px] uppercase tracking-wide text-text-3">{kindLabel(item.kind)}</span>
          </button>
        );
      })}
    </div>
  );
}

export default ComposerCompletionMenu;
