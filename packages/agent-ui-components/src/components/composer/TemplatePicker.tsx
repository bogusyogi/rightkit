import { useDismiss } from "@rightkit/platform-ui/headless";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  TEMPLATE_CATEGORIES,
  type PromptTemplate,
  type TemplateCategory,
} from "@rightkit/agent-ui-core/lib/promptTemplates/templates";

export interface TemplatePickerProps {
  builtInTemplates: PromptTemplate[];
  userTemplates: PromptTemplate[];
  onSelect: (template: PromptTemplate) => void;
  onDeleteUser: (id: string) => void;
  open: boolean;
  onToggle: () => void;
  pinnedIds: Set<string>;
  onTogglePin: (id: string) => void;
  onExportTemplate?: (template: PromptTemplate) => void;
  onImportBundle?: () => void;
  onExportAll?: () => void;
  templateGroups: string[];
  onCreateGroup: (name: string) => void;
  onRenameGroup: (oldName: string, newName: string) => void;
  onDeleteGroup: (name: string) => void;
  onMoveToGroup: (templateId: string, group: string | null) => void;
}

type TabId = "built-in" | "my-templates";

const buttonClass = "rounded border border-line px-2 py-1 text-xs text-text-2 hover:bg-panel-2";

/** Port of Hermes IDE's grouped template browser, adapted to the host app token classes. */
export function TemplatePicker({
  builtInTemplates,
  userTemplates,
  onSelect,
  onDeleteUser,
  open,
  onToggle,
  pinnedIds,
  onTogglePin,
  onExportTemplate,
  onImportBundle,
  onExportAll,
  templateGroups,
  onCreateGroup,
  onRenameGroup,
  onDeleteGroup,
  onMoveToGroup,
}: TemplatePickerProps) {
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());
  const [search, setSearch] = useState("");
  const [activeTab, setActiveTab] = useState<TabId>("built-in");
  const [hoveredId, setHoveredId] = useState<string | null>(null);
  const [moveMenuId, setMoveMenuId] = useState<string | null>(null);
  const [newGroup, setNewGroup] = useState("");
  const [showNewGroup, setShowNewGroup] = useState(false);
  const [editingGroup, setEditingGroup] = useState<string | null>(null);
  const [editingName, setEditingName] = useState("");
  const buttonRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const searchRef = useRef<HTMLInputElement>(null);
  const newGroupRef = useRef<HTMLInputElement>(null);
  const editGroupRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (!open) return;
    setSearch("");
    setMoveMenuId(null);
    setShowNewGroup(false);
    setEditingGroup(null);
    requestAnimationFrame(() => searchRef.current?.focus());
  }, [open]);

  useDismiss(
    (reason) => {
      if (reason === "escape" && search) setSearch("");
      else onToggle();
    },
    { enabled: open, container: panelRef, ignore: [buttonRef], preventDefault: false },
  );

  useEffect(() => {
    if (showNewGroup) requestAnimationFrame(() => newGroupRef.current?.focus());
  }, [showNewGroup]);
  useEffect(() => {
    if (editingGroup) requestAnimationFrame(() => editGroupRef.current?.focus());
  }, [editingGroup]);

  const allTemplates = useMemo(
    () => [...builtInTemplates, ...userTemplates],
    [builtInTemplates, userTemplates],
  );
  const filtered = useMemo(() => {
    const query = search.trim().toLowerCase();
    if (!query) return null;
    const source = activeTab === "built-in" ? builtInTemplates : userTemplates;
    return source.filter((template) => {
      const category = TEMPLATE_CATEGORIES[template.category];
      return [template.name, template.description, category?.label, template.group]
        .filter(Boolean)
        .some((field) => field?.toLowerCase().includes(query));
    });
  }, [activeTab, builtInTemplates, search, userTemplates]);
  const builtInGroups = useMemo(() => {
    const groups = new Map<TemplateCategory, PromptTemplate[]>();
    for (const category of Object.keys(TEMPLATE_CATEGORIES) as TemplateCategory[]) {
      const templates = builtInTemplates.filter((template) => template.category === category);
      if (templates.length) groups.set(category, templates);
    }
    return groups;
  }, [builtInTemplates]);
  const userGroups = useMemo(() => {
    const groups = new Map<string, PromptTemplate[]>(templateGroups.map((group) => [group, []]));
    const ungrouped: PromptTemplate[] = [];
    for (const template of userTemplates) {
      if (!template.group) ungrouped.push(template);
      else groups.set(template.group, [...(groups.get(template.group) ?? []), template]);
    }
    return { groups, ungrouped };
  }, [templateGroups, userTemplates]);
  const pinned = useMemo(() => {
    const byId = new Map(allTemplates.map((template) => [template.id, template]));
    return [...pinnedIds].map((id) => byId.get(id)).filter(Boolean) as PromptTemplate[];
  }, [allTemplates, pinnedIds]);
  const hovered = hoveredId ? allTemplates.find((template) => template.id === hoveredId) : null;

  const select = useCallback(
    (template: PromptTemplate) => {
      onSelect(template);
      onToggle();
    },
    [onSelect, onToggle],
  );
  const createGroup = useCallback(() => {
    const name = newGroup.trim();
    if (!name) return;
    onCreateGroup(name);
    setNewGroup("");
    setShowNewGroup(false);
  }, [newGroup, onCreateGroup]);
  const renameGroup = useCallback(() => {
    if (!editingGroup) return;
    const name = editingName.trim();
    if (name && name !== editingGroup) onRenameGroup(editingGroup, name);
    setEditingGroup(null);
    setEditingName("");
  }, [editingGroup, editingName, onRenameGroup]);

  const renderTemplate = (template: PromptTemplate, showCategory = false) => {
    const user = !template.builtIn;
    const pinnedTemplate = pinnedIds.has(template.id);
    return (
      // biome-ignore lint/a11y/useSemanticElements: composite selectable row contains independent action buttons.
      <div
        key={template.id}
        role="button"
        tabIndex={0}
        className="flex cursor-pointer items-center gap-2 px-2.5 py-2 text-left text-sm hover:bg-panel-2"
        onClick={() => select(template)}
        onKeyDown={(event) => {
          if (event.key === "Enter" || event.key === " ") {
            event.preventDefault();
            select(template);
          }
        }}
        onMouseEnter={() => setHoveredId(template.id)}
        onMouseLeave={() => {
          setHoveredId(null);
          setMoveMenuId(null);
        }}
      >
        <span className="min-w-0 flex-1 truncate text-text">{template.name}</span>
        {showCategory && (
          <span className="shrink-0 text-[10px] text-text-3">
            {template.group || TEMPLATE_CATEGORIES[template.category]?.label}
          </span>
        )}
        <span className="flex shrink-0 items-center gap-1">
          <button
            type="button"
            className={pinnedTemplate ? "text-accent" : "text-text-3"}
            onClick={(event) => {
              event.stopPropagation();
              onTogglePin(template.id);
            }}
            aria-label={pinnedTemplate ? "Unpin template" : "Pin template"}
          >
            📌
          </button>
          {user && (
            <>
              <button
                type="button"
                className="text-text-3"
                onClick={(event) => {
                  event.stopPropagation();
                  setMoveMenuId(moveMenuId === template.id ? null : template.id);
                }}
                aria-label="Move template"
              >
                📁
              </button>
              {onExportTemplate && (
                <button
                  type="button"
                  className="text-text-3"
                  onClick={(event) => {
                    event.stopPropagation();
                    onExportTemplate(template);
                  }}
                  aria-label="Export template"
                >
                  ↗
                </button>
              )}
              <button
                type="button"
                className="text-danger"
                onClick={(event) => {
                  event.stopPropagation();
                  onDeleteUser(template.id);
                }}
                aria-label="Delete template"
              >
                ×
              </button>
            </>
          )}
        </span>
        {user && moveMenuId === template.id && (
          <div className="absolute z-10 rounded border border-line bg-panel p-1 shadow-app">
            <button
              type="button"
              className="block w-full px-2 py-1 text-left text-xs hover:bg-panel-2"
              onClick={(event) => {
                event.stopPropagation();
                onMoveToGroup(template.id, null);
                setMoveMenuId(null);
              }}
            >
              Ungrouped
            </button>
            {templateGroups.map((group) => (
              <button
                type="button"
                key={group}
                className="block w-full px-2 py-1 text-left text-xs hover:bg-panel-2"
                onClick={(event) => {
                  event.stopPropagation();
                  onMoveToGroup(template.id, group);
                  setMoveMenuId(null);
                }}
              >
                {group}
              </button>
            ))}
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="relative inline-block">
      <button
        ref={buttonRef}
        type="button"
        className={buttonClass}
        onClick={onToggle}
        aria-expanded={open}
      >
        ☷ Templates
      </button>
      {open && (
        <div
          ref={panelRef}
          role="dialog"
          aria-label="Prompt templates"
          className="absolute bottom-full left-0 z-50 mb-1 w-[min(30rem,calc(100vw-1.5rem))] rounded-xl border border-line bg-panel p-2 shadow-app"
        >
          <fieldset className="mb-2 flex gap-1 border-0 p-0">
            <input
              ref={searchRef}
              aria-label="Search templates"
              className="min-w-0 flex-1 rounded border border-line bg-transparent px-2 py-1 text-sm text-text"
              placeholder="Search templates"
              value={search}
              onChange={(event) => setSearch(event.target.value)}
            />
            <button
              type="button"
              className={buttonClass}
              onClick={() => setSearch("")}
              disabled={!search}
            >
              ×
            </button>
          </fieldset>
          <div className="mb-1 flex border-b border-line">
            <button
              type="button"
              className={`px-2 py-1 text-xs ${activeTab === "built-in" ? "border-b-2 border-accent text-text" : "text-text-3"}`}
              onClick={() => setActiveTab("built-in")}
            >
              Built-in
            </button>
            <button
              type="button"
              className={`px-2 py-1 text-xs ${activeTab === "my-templates" ? "border-b-2 border-accent text-text" : "text-text-3"}`}
              onClick={() => setActiveTab("my-templates")}
            >
              My templates{userTemplates.length ? ` (${userTemplates.length})` : ""}
            </button>
          </div>
          <div className="max-h-[min(28rem,60vh)] overflow-y-auto">
            {pinned.length > 0 && !search && (
              <section>
                <h3 className="px-2 py-1 text-[10px] uppercase tracking-wide text-text-3">
                  📌 Pinned
                </h3>
                {pinned.map((template) => renderTemplate(template))}
              </section>
            )}
            {filtered ? (
              filtered.length ? (
                filtered.map((template) => renderTemplate(template, true))
              ) : (
                <p className="p-3 text-xs text-text-3">No templates match “{search}”.</p>
              )
            ) : activeTab === "built-in" ? (
              [...builtInGroups.entries()].map(([category, templates]) => (
                <section key={category}>
                  <button
                    type="button"
                    className="flex w-full items-center gap-2 px-2 py-1 text-left text-xs font-semibold text-text-2"
                    onClick={() =>
                      setCollapsed((current) => {
                        const next = new Set(current);
                        next.has(category) ? next.delete(category) : next.add(category);
                        return next;
                      })
                    }
                  >
                    <span>{collapsed.has(category) ? "▸" : "▾"}</span>
                    <span>{TEMPLATE_CATEGORIES[category].icon}</span>
                    <span>{TEMPLATE_CATEGORIES[category].label}</span>
                    <span className="text-text-3">{templates.length}</span>
                  </button>
                  {!collapsed.has(category) &&
                    templates.map((template) => renderTemplate(template))}
                </section>
              ))
            ) : (
              <>
                <fieldset className="mb-2 flex gap-1 border-0 p-0">
                  {onImportBundle && (
                    <button type="button" className={buttonClass} onClick={onImportBundle}>
                      Import
                    </button>
                  )}
                  {onExportAll && userTemplates.length > 0 && (
                    <button type="button" className={buttonClass} onClick={onExportAll}>
                      Export all
                    </button>
                  )}
                </fieldset>
                {showNewGroup ? (
                  <fieldset className="flex gap-1 border-0 p-0">
                    <input
                      ref={newGroupRef}
                      aria-label="New group name"
                      className="min-w-0 flex-1 rounded border border-line bg-transparent px-2 py-1 text-xs"
                      placeholder="Group name"
                      value={newGroup}
                      onChange={(event) => setNewGroup(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.key === "Enter") createGroup();
                        if (event.key === "Escape") setShowNewGroup(false);
                      }}
                    />
                    <button type="button" className={buttonClass} onClick={createGroup}>
                      +
                    </button>
                  </fieldset>
                ) : (
                  <button
                    type="button"
                    className="mb-1 px-2 py-1 text-left text-xs text-accent"
                    onClick={() => setShowNewGroup(true)}
                  >
                    + New group
                  </button>
                )}
                {[...userGroups.groups.entries()].map(([group, templates]) => (
                  <section key={group}>
                    <header
                      className="flex items-center gap-1 px-2 py-1 text-xs font-semibold text-text-2"
                    >
                      <button
                        type="button"
                        onClick={() =>
                          setCollapsed((current) => {
                            const next = new Set(current);
                            const key = `group:${group}`;
                            next.has(key) ? next.delete(key) : next.add(key);
                            return next;
                          })
                        }
                      >
                        {collapsed.has(`group:${group}`) ? "▸" : "▾"}
                      </button>
                      {editingGroup === group ? (
                        <input
                          ref={editGroupRef}
                          aria-label={`Rename ${group}`}
                          value={editingName}
                          onChange={(event) => setEditingName(event.target.value)}
                          onBlur={renameGroup}
                          onKeyDown={(event) => {
                            if (event.key === "Enter") renameGroup();
                            if (event.key === "Escape") setEditingGroup(null);
                          }}
                        />
                      ) : (
                        <span className="flex-1">
                          📁 {group} ({templates.length})
                        </span>
                      )}
                      <button
                        type="button"
                        onClick={() => {
                          setEditingGroup(group);
                          setEditingName(group);
                        }}
                        aria-label={`Rename ${group}`}
                      >
                        ✎
                      </button>
                      <button
                        type="button"
                        onClick={() => onDeleteGroup(group)}
                        aria-label={`Delete ${group}`}
                      >
                        ×
                      </button>
                    </header>
                    {!collapsed.has(`group:${group}`) &&
                      (templates.length ? (
                        templates.map((template) => renderTemplate(template))
                      ) : (
                        <p className="px-2 py-1 text-xs text-text-3">No templates in group.</p>
                      ))}
                  </section>
                ))}
                {userGroups.ungrouped.length > 0 && (
                  <section>
                    <h3 className="px-2 py-1 text-[10px] uppercase text-text-3">Ungrouped</h3>
                    {userGroups.ungrouped.map((template) => renderTemplate(template))}
                  </section>
                )}
                {userTemplates.length === 0 && templateGroups.length === 0 && (
                  <p className="p-3 text-xs text-text-3">No saved templates.</p>
                )}
              </>
            )}
          </div>
          {hovered?.description && (
            <div className="border-t border-line px-2 py-2 text-xs">
              <strong className="block text-text">{hovered.name}</strong>
              <span className="text-text-3">{hovered.description}</span>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

export default TemplatePicker;
