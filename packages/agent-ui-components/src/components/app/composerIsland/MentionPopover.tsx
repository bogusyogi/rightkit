import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { CommandInfo } from "@rightkit/agent-ui-core/lib/daemon";
import type { WorkspaceMention } from "@rightkit/agent-ui-core/lib/daemon";
import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";

export type MentionMode = "off" | "slash-command" | "workspace-mention" | "skill-mention";

export interface MentionState {
  mode: MentionMode;
  query: string;
  anchor: { start: number; end: number };
}

export type PopoverItem =
  | { kind: "command"; key: string; command: CommandInfo }
  | { kind: "mention"; key: string; mention: WorkspaceMention }
  | { kind: "skill"; key: string; skill: SkillItem };

/** "code-review" → "Code review" for the friendly row title; the raw slash
 * form still completes into the input on pick. */
function commandTitle(command: CommandInfo): string {
  const name = command.name.replace(/-/g, " ");
  return name.charAt(0).toUpperCase() + name.slice(1);
}

function skillScope(skill: SkillItem): string {
  const scope = skill.source?.trim() || skill.category;
  return scope.charAt(0).toUpperCase() + scope.slice(1);
}

export function MentionPopover(props: {
  state: MentionState;
  items: PopoverItem[];
  selectedIndex: number;
  onSelectIndex: (index: number) => void;
  onPickCommand: (command: CommandInfo) => void;
  onPickMention: (mention: WorkspaceMention) => void;
  onPickSkill: (skill: SkillItem) => void;
  onClose: () => void;
}) {
  if (props.state.mode === "off") return null;
  if (props.items.length === 0) return null;

  const label =
    props.state.mode === "slash-command"
      ? "Slash commands"
      : props.state.mode === "workspace-mention"
        ? "Workspace mentions"
        : "Skill mentions";

  const rowClass = (selected: boolean) =>
    `cr-popover-row mx-1 flex w-[calc(100%-8px)] items-center gap-2.5 rounded-lg border-0 px-2.5 py-2 text-left ${
      selected
        ? "bg-panel-2 text-text"
        : "bg-transparent text-text-2 hover:bg-panel-2 hover:text-text"
    }`;

  return (
    <div
      role="listbox"
      aria-label={label}
      data-testid="composer-completion-menu"
      onMouseDown={(event) => event.preventDefault()}
      className="cr-panel-reveal absolute left-3 right-3 bottom-full z-40 mb-1.5 max-h-[300px] overflow-y-auto rounded-xl border border-line bg-panel py-1 shadow-app"
    >
      {props.items.map((item, index) => {
        const selected = index === props.selectedIndex;
        if (item.kind === "command") {
          return (
            <button
              key={item.key}
              data-testid={`composer-completion-command-${index}`}
              type="button"
              role="option"
              aria-selected={selected}
              className={rowClass(selected)}
              onMouseEnter={() => props.onSelectIndex(index)}
              onFocus={() => props.onSelectIndex(index)}
              onClick={() => {
                props.onPickCommand(item.command);
                props.onClose();
              }}
            >
              <span className="shrink-0 text-text-3">
                <Icon name="terminal" size={14} />
              </span>
              <span className="shrink-0 text-[13px] font-medium text-text">
                {commandTitle(item.command)}
              </span>
              <span className="min-w-0 flex-1 truncate text-[12px] text-text-3">
                {item.command.description}
              </span>
            </button>
          );
        }
        if (item.kind === "mention") {
          return (
            <button
              key={item.key}
              data-testid={`composer-completion-${item.kind}-${index}`}
              type="button"
              role="option"
              aria-selected={selected}
              className={rowClass(selected)}
              onMouseEnter={() => props.onSelectIndex(index)}
              onFocus={() => props.onSelectIndex(index)}
              onClick={() => {
                props.onPickMention(item.mention);
                props.onClose();
              }}
            >
              <span className="shrink-0 text-text-3">
                <Icon name={item.mention.kind === "file" ? "folder" : "spark"} size={14} />
              </span>
              <span className="shrink-0 text-[13px] font-medium text-text">
                {item.mention.label}
              </span>
              <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-text-3">
                {item.mention.detail}
              </span>
            </button>
          );
        }
        return (
          <button
            key={item.key}
            data-testid={`composer-completion-${item.kind}-${index}`}
            type="button"
            role="option"
            aria-selected={selected}
            className={rowClass(selected)}
            onMouseEnter={() => props.onSelectIndex(index)}
            onFocus={() => props.onSelectIndex(index)}
            onClick={() => {
              props.onPickSkill(item.skill);
              props.onClose();
            }}
            title={item.skill.description || item.skill.trigger || item.skill.slug}
          >
            <span className="shrink-0 text-text-3">
              <Icon name="spark" size={14} />
            </span>
            <span className="shrink-0 text-[13px] font-medium text-text">
              {item.skill.name || item.skill.slug}
            </span>
            <span className="min-w-0 flex-1 truncate text-[12px] text-text-3">
              {item.skill.description}
            </span>
            <span className="shrink-0 text-[11px] text-text-3">{skillScope(item.skill)}</span>
          </button>
        );
      })}
    </div>
  );
}
