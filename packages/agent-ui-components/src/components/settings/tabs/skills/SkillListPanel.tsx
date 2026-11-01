import { EmptyState } from "@rightkit/react-shell/react";
import { useMemo, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";
import type { SkillListPanelProps } from "./types";

export function SkillEffectsText(skill: SkillItem): string {
  const effects = skill.effects;
  const parts: (string | null)[] = [skill.disableModelInvocation ? "user-only" : null];
  if (!effects) return parts.filter(Boolean).join(" · ");
  parts.push(
    effects.executionMode ? `mode ${effects.executionMode}` : null,
    effects.allowedTools?.length ? `allow ${effects.allowedTools.join(",")}` : null,
    effects.disallowedTools?.length ? `deny ${effects.disallowedTools.join(",")}` : null,
    effects.prompt ? effects.prompt : null,
  );
  return parts.filter(Boolean).join(" · ");
}

export function SkillListPanel({ skills, selectedSlug, onSelect }: SkillListPanelProps) {
  const [query, setQuery] = useState("");
  const visibleSkills = useMemo(() => {
    const normalized = query.trim().toLowerCase();
    if (!normalized) return skills;
    return skills.filter((skill) =>
      `${skill.name} ${skill.slug} ${skill.description} ${skill.trigger} ${skill.tags.join(" ")} ${skill.path}`
        .toLowerCase()
        .includes(normalized),
    );
  }, [skills, query]);

  return (
    <div className="flex flex-col border border-line rounded-xl overflow-hidden min-h-0">
      <label className="flex h-10 items-center gap-2 border-b border-line bg-panel px-3 text-text-3">
        <Icon name="search" size={13} />
        <input
          className="min-w-0 flex-1 border-0 bg-transparent text-[13px] text-text outline-none placeholder:text-text-3"
          aria-label="Search installed skills"
          placeholder="Search installed skills"
          value={query}
          onChange={(event) => setQuery(event.target.value)}
        />
      </label>
      <div className="min-h-0 flex-1 divide-y divide-line overflow-y-auto">
        {visibleSkills.length > 0 ? (
          visibleSkills.map((skill) => (
            <SkillListRow
              key={skill.slug}
              skill={skill}
              selected={selectedSlug === skill.slug}
              onSelect={onSelect}
            />
          ))
        ) : (
          <EmptyState icon={<Icon name="spark" size={18} />} title={skills.length > 0 ? "No matching skills." : "No local skills found."}>{skills.length > 0
                ? "Clear the search to restore the installed list."
                : "Use New skill to add a local markdown skill."}</EmptyState>
        )}
      </div>
    </div>
  );
}

function SkillListRow({
  skill,
  selected,
  onSelect,
}: {
  skill: SkillItem;
  selected: boolean;
  onSelect: (slug: string) => void;
}) {
  return (
    <button
      type="button"
      className={`grid grid-cols-[minmax(0,1fr)_auto] gap-3 px-4 py-3 text-left border-0 cursor-pointer ${
        selected
          ? "bg-panel-2 text-text [box-shadow:inset_2px_0_0_var(--color-accent)]"
          : "bg-transparent text-text-2 hover:bg-panel hover:text-text"
      }`}
      onClick={() => onSelect(skill.slug)}
    >
      <span className="flex flex-col gap-1 min-w-0">
        <strong className="text-[13px] font-semibold truncate">{skill.name || skill.slug}</strong>
        <span className="text-[12px] text-text-3 truncate">{skill.description}</span>
        <span className="text-[11px] text-text-3 truncate">{skill.path}</span>
      </span>
      <span className="flex flex-col items-end gap-1">
        <code className="text-[11px] font-mono text-text-3">{skill.kind}</code>
        <code className={`text-[11px] font-mono ${skill.valid ? "text-text-3" : "text-danger"}`}>
          {(skill.active ?? skill.enabled) ? "active" : (skill.lifecycle ?? "disabled")}
        </code>
      </span>
    </button>
  );
}
