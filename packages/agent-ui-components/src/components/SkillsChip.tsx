import { Popover } from "@rightkit/react-shell/react";
import { useEffect, useMemo, useRef, useState } from "react";
import Icon from "@rightkit/agent-ui-core/components/Icon";
import type { SkillItem } from "@rightkit/agent-ui-core/lib/types";
import { skillScopeLabel, skillStatusLabel } from "./app/composerBarHelpers";

// Composer footer chip that opens a Skills popover wired to the existing
// MentionPopover anatomy (icon + name + description + scope tag). Lets the
// user search, keyboard-navigate, refresh, insert a @skill mention, or jump
// straight to Settings > Skills. Matches the Composer popover primitives in
// index.css (cr-popover, cr-popover-row) so visual rules stay consistent.
export interface SkillsChipProps {
  skills: SkillItem[];
  onInsert: (skill: SkillItem) => void;
  onRefresh: () => void;
  onOpenManage: () => void;
  disabled?: boolean;
}

function filterSkills(skills: SkillItem[], query: string): SkillItem[] {
  const trimmed = query.trim().toLowerCase();
  if (!trimmed) return skills;
  return skills.filter((skill) => {
    const haystack = [skill.name, skill.slug, skill.description, skill.trigger, skill.category]
      .filter(Boolean)
      .join(" ")
      .toLowerCase();
    return haystack.includes(trimmed);
  });
}

export default function SkillsChip({
  skills,
  onInsert,
  onRefresh,
  onOpenManage,
  disabled,
}: SkillsChipProps) {
  const [open, setOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [activeIndex, setActiveIndex] = useState(0);
  const chipButtonRef = useRef<HTMLButtonElement | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);

  const filtered = useMemo(() => filterSkills(skills, query), [skills, query]);
  const totalCount = skills.length;

  // biome-ignore lint/correctness/useExhaustiveDependencies: reset keyboard selection when the popover or query changes.
  useEffect(() => {
    setActiveIndex(0);
  }, [query, open]);

  useEffect(() => {
    if (!open) return;
    searchRef.current?.focus();
  }, [open]);

  function handleChipClick() {
    setOpen((value) => !value);
    setQuery("");
  }

  function handleKeyDownOnList(event: React.KeyboardEvent<HTMLDivElement>) {
    if (filtered.length === 0) return;
    if (event.key === "ArrowDown") {
      event.preventDefault();
      setActiveIndex((index) => (index + 1) % filtered.length);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      setActiveIndex((index) => (index - 1 + filtered.length) % filtered.length);
    } else if (event.key === "Enter") {
      event.preventDefault();
      const next = filtered[activeIndex];
      if (next) {
        onInsert(next);
        setOpen(false);
      }
    } else if (event.key === "Home") {
      event.preventDefault();
      setActiveIndex(0);
    } else if (event.key === "End") {
      event.preventDefault();
      setActiveIndex(filtered.length - 1);
    }
  }

  function handleInsert(skill: SkillItem) {
    onInsert(skill);
    setOpen(false);
  }

  return (
    <div className="relative" data-testid="skills-chip">
      <button
        ref={chipButtonRef}
        type="button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label="Open skills"
        title="Skills"
        disabled={disabled}
        onClick={handleChipClick}
        className="cr-control-pill inline-flex h-6 items-center gap-1.5 rounded-md px-1.5 text-[12px] text-text-2 cursor-pointer hover:bg-panel hover:text-text disabled:opacity-50 disabled:cursor-not-allowed"
      >
        <Icon name="spark" size={11} />
        <span>Skills</span>
        <span className="inline-flex h-4 min-w-[16px] items-center justify-center rounded-full border border-line bg-panel px-1 font-mono text-[10px] tabular-nums text-text-3">
          {totalCount}
        </span>
      </button>
      {open ? (
        <Popover
          anchor={chipButtonRef.current}
          onClose={() => setOpen(false)}
          label="Skills"
          placement="top-end"
          width={340}
        >
        {/* biome-ignore lint/a11y/noStaticElementInteractions: key delegation only: the handler sees key events bubbling up from the focusable search input and option buttons inside, so the wrapper is not itself a widget and takes no role. */}
        <div onKeyDown={handleKeyDownOnList}>
          <div className="flex items-center gap-2 border-b border-line px-3 py-2">
            <Icon name="search" size={12} />
            <input
              ref={searchRef}
              type="text"
              aria-label="Search installed skills"
              value={query}
              placeholder="Search installed skills"
              onChange={(event) => setQuery(event.currentTarget.value)}
              className="flex-1 bg-transparent text-[12px] text-text outline-none placeholder:text-text-3"
            />
            <button
              type="button"
              aria-label="Refresh skills"
              title="Refresh"
              onClick={() => {
                onRefresh();
              }}
              className="cr-control-pill grid h-6 w-6 place-items-center rounded-md border border-transparent text-text-3 hover:bg-panel-2 hover:text-text"
            >
              <Icon name="refresh" size={12} />
            </button>
          </div>
          <div className="max-h-[260px] overflow-y-auto py-1">
            {filtered.length === 0 ? (
              <div className="px-3 py-3 text-[12px] text-text-3">
                {totalCount === 0 ? "No skills installed yet." : "No skills match that search."}
              </div>
            ) : (
              <div role="listbox" aria-label="Skills" className="m-0 list-none p-0">
                {filtered.map((skill, index) => {
                  const selected = index === activeIndex;
                  return (
                    <li key={skill.slug} role="presentation">
                      <button
                        type="button"
                        role="option"
                        aria-selected={selected}
                        data-skill-slug={skill.slug}
                        onMouseEnter={() => setActiveIndex(index)}
                        onFocus={() => setActiveIndex(index)}
                        onClick={() => handleInsert(skill)}
                        className={`cr-popover-row mx-1 flex w-[calc(100%-8px)] items-center gap-2.5 rounded-lg border-0 px-2.5 py-2 text-left ${
                          selected
                            ? "bg-panel-2 text-text"
                            : "bg-transparent text-text-2 hover:bg-panel-2 hover:text-text"
                        }`}
                        title={skill.description || skill.trigger || skill.slug}
                      >
                        <span className="shrink-0 text-text-3">
                          <Icon name="spark" size={14} />
                        </span>
                        <span className="min-w-0 flex-1">
                          <span className="block truncate text-[13px] font-medium text-text">
                            {skill.name || skill.slug}
                          </span>
                          <span className="block truncate text-[11px] text-text-3">
                            {skill.description || skill.trigger || `@${skill.slug}`}
                          </span>
                        </span>
                        <span className="shrink-0 text-[11px] text-text-3">
                          {skillScopeLabel(skill)}
                        </span>
                        <span
                          className={`shrink-0 rounded-full border px-1.5 py-0.5 text-[10px] uppercase tracking-wide ${
                            skillStatusLabel(skill) === "Active"
                              ? "border-line bg-panel-2 text-text-3"
                              : "border-warning/40 text-warning"
                          }`}
                        >
                          {skillStatusLabel(skill)}
                        </span>
                      </button>
                    </li>
                  );
                })}
              </div>
            )}
          </div>
          <div className="flex items-center justify-between gap-2 border-t border-line px-3 py-2 text-[11px] text-text-3">
            <button
              type="button"
              onClick={() => {
                onOpenManage();
                setOpen(false);
              }}
              className="inline-flex items-center gap-1 text-accent hover:underline"
            >
              Manage in Settings
            </button>
            <span className="text-text-3/70">
              {filtered.length} of {totalCount}
            </span>
          </div>
        </div>
        </Popover>
      ) : null}
    </div>
  );
}
