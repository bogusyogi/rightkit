import { useState } from "react";
import { IconButton, NavList, type NavGroup } from "@rightkit/react-shell/react";
import type { DragEvent as ReactDragEvent } from "react";
import Icon from "../components/Icons";
import { FOLDERS, FOLDER_ICON, userLabelsOf } from "../lib/mail";
import { setPref } from "../lib/preferences";
import { removeSavedSearch as removeSavedSearchFrom, SAVED_SEARCHES_KEY } from "../lib/savedSearches";
import type { ThreadRef } from "../lib/ipc";
import { useAppStore, APP_OF } from "../store";
import { DriveNav } from "./nav/DriveNav";
import { CalendarNav } from "./nav/CalendarNav";
import { ContactsNav } from "./nav/ContactsNav";
import { TasksNav } from "./nav/TasksNav";

interface Props {
  unreadFor: (id: string) => number;
  applyView: (q: string) => void;
  createLabelUi: () => void;
  dropThreadOnLabel: (ref: ThreadRef, labelId: string, labelName: string) => void;
  openLabel: (id: string) => void;
}

export function Sidebar({
  unreadFor, applyView, createLabelUi, dropThreadOnLabel, openLabel,
}: Props) {
  const accounts = useAppStore((s) => s.accounts);
  const activeAccount = useAppStore((s) => s.activeAccount);
  const activeLabel = useAppStore((s) => s.activeLabel);
  const collapsedLabels = useAppStore((s) => s.collapsedLabels);
  const setCollapsedLabels = useAppStore((s) => s.setCollapsedLabels);
  const counts = useAppStore((s) => s.counts);
  const draftCount = useAppStore((s) => s.draftCount);
  const folder = useAppStore((s) => s.folder);
  const hiddenLabels = useAppStore((s) => s.hiddenLabels);
  const labelColors = useAppStore((s) => s.labelColors);
  const labels = useAppStore((s) => s.labels);
  const savedSearches = useAppStore((s) => s.savedSearches);
  const setSavedSearches = useAppStore((s) => s.setSavedSearches);
  const setActiveLabel = useAppStore((s) => s.setActiveLabel);
  const setCategory = useAppStore((s) => s.setCategory);
  const setCtx = useAppStore((s) => s.setCtx);
  const setFolder = useAppStore((s) => s.setFolder);
  const setOpenThread = useAppStore((s) => s.setOpenThread);
  const setErr = useAppStore((s) => s.setErr);
  const setView = useAppStore((s) => s.setView);
  const view = useAppStore((s) => s.view);
  const app = APP_OF[view];
  const [dropLabelId, setDropLabelId] = useState<string | null>(null);

  const unreadAll = counts.total;

  function todayStr() {
    const d = new Date();
    const mm = String(d.getMonth() + 1).padStart(2, "0");
    const dd = String(d.getDate()).padStart(2, "0");
    return `${d.getFullYear()}/${mm}/${dd}`;
  }
  // User labels, minus hidden, sorted so children sit under parents.
  const userLabels = userLabelsOf(labels);
  const visibleLabels = [...userLabels.filter((l) => !hiddenLabels.has(l.id))].sort((a, b) =>
    a.name.localeCompare(b.name)
  );
  const parentNames = new Set<string>();
  for (const l of visibleLabels) {
    const parts = l.name.split("/");
    for (let i = 1; i < parts.length; i++) parentNames.add(parts.slice(0, i).join("/"));
  }
  const hiddenByCollapse = (name: string): boolean => {
    const parts = name.split("/");
    for (let i = 1; i < parts.length; i++) {
      if (collapsedLabels.has(parts.slice(0, i).join("/"))) return true;
    }
    return false;
  };
  const treeLabels = visibleLabels.filter((l) => !hiddenByCollapse(l.name));
  function toggleLabelCollapsed(name: string) {
    setCollapsedLabels((prev) => {
      const n = new Set(prev);
      if (n.has(name)) n.delete(name);
      else n.add(name);
      setPref("collapsedLabels", [...n]);
      return n;
    });
  }

  function removeSavedSearch(query: string) {
    setSavedSearches((prev) => {
      const next = removeSavedSearchFrom(prev, query);
      setPref(SAVED_SEARCHES_KEY, next);
      return next;
    });
  }

  function labelDropHandlers(labelId: string, labelName: string) {
    return {
      onDragEnter: (e: ReactDragEvent<HTMLButtonElement>) => {
        if (e.dataTransfer.types.includes("application/mailright-thread")) {
          setDropLabelId(labelId);
        }
      },
      onDragLeave: (e: ReactDragEvent<HTMLButtonElement>) => {
        if (!e.currentTarget.contains(e.relatedTarget as Node | null)) {
          setDropLabelId((current) => (current === labelId ? null : current));
        }
      },
      onDragOver: (e: ReactDragEvent<HTMLButtonElement>) => {
        if (e.dataTransfer.types.includes("application/mailright-thread")) {
          e.preventDefault();
          e.dataTransfer.dropEffect = "move";
        }
      },
      onDrop: (e: ReactDragEvent<HTMLButtonElement>) => {
        const raw = e.dataTransfer.getData("application/mailright-thread");
        if (!raw) return;
        e.preventDefault();
        e.stopPropagation();
        let ref: ThreadRef;
        try {
          ref = JSON.parse(raw) as ThreadRef;
        } catch {
          setErr("Could not read the dragged thread.");
          return;
        }
        setDropLabelId(null);
        dropThreadOnLabel(ref, labelId, labelName);
      },
    };
  }

  const viewQueries: Record<string, string> = {
    unread: "is:unread",
    attachments: "has:attachment",
    today: `after:${todayStr()}`,
  };
  const folderGroups: NavGroup[] = [
    {
      items: [
        {
          id: "all",
          label: "All inboxes",
          icon: <Icon name="inboxes" size={17} className="f-ic" />,
          badge: unreadAll > 0 ? unreadAll : undefined,
          ariaLabel: `Open All inboxes${unreadAll > 0 ? `, ${unreadAll} unread` : ""}`,
        },
      ],
    },
    {
      items: FOLDERS.map((f) => {
        const badge =
          f.key === "inbox" && activeAccount
            ? unreadFor(activeAccount)
            : f.key === "drafts"
            ? draftCount
            : 0;
        return {
          id: f.key,
          label: f.name,
          icon: <Icon name={FOLDER_ICON[f.key]} size={17} className="f-ic" />,
          badge: badge > 0 ? badge : undefined,
          ariaLabel: `Open ${f.name}${badge > 0 ? `, ${badge} unread` : ""}`,
        };
      }),
    },
  ];
  const folderActiveId = view === "inbox" && !activeLabel ? folder : "";
  function openFolderItem(id: string) {
    setFolder(id as typeof folder);
    setActiveLabel(null);
    setCategory(null);
    setView("inbox");
    setOpenThread(null);
  }
  const viewGroups: NavGroup[] = [
    {
      items: [
        { id: "unread", label: "Unread", ariaLabel: "Open Unread view", icon: <Icon name="mailClosed" size={16} className="f-ic" /> },
        { id: "attachments", label: "Has attachments", ariaLabel: "Open Has attachments view", icon: <Icon name="paperclip" size={16} className="f-ic" /> },
        { id: "today", label: "Today", ariaLabel: "Open Today view", icon: <Icon name="clock" size={16} className="f-ic" /> },
      ],
    },
  ];
  const savedGroups: NavGroup[] = [
    {
      items: savedSearches.map((q) => ({
        id: q.query,
        label: q.name,
        ariaLabel: `Run saved search ${q.name}`,
        icon: <Icon name="clock" size={16} className="f-ic" />,
      })),
    },
  ];
  const activeLabelId = view === "inbox" && activeLabel ? activeLabel : "";
  const labelById = new Map(treeLabels.map((l) => [l.id, l]));
  const labelGroups: NavGroup[] = [
    {
      items: treeLabels.map((l) => {
        const color = l.color ?? labelColors[l.id];
        return {
          id: l.id,
          label: l.name.split("/").pop() || l.name,
          icon: parentNames.has(l.name) ? undefined : color ? (
            <span className="label-dot" style={{ background: color }} aria-hidden="true" />
          ) : (
            <Icon name="tag" size={16} className="f-ic" />
          ),
          badge: l.unread > 0 ? l.unread : undefined,
          ariaLabel: `Open label ${l.name}${l.unread > 0 ? `, ${l.unread} unread` : ""}`,
        };
      }),
    },
  ];

  return (
    <aside className="sidebar">

      {/* Contextual app nav. */}
      {app === "drive" ? (
        <DriveNav />
      ) : app === "calendar" ? (
        <CalendarNav />
      ) : app === "contacts" ? (
        <ContactsNav />
      ) : app === "tasks" ? (
        <TasksNav />
      ) : app === "settings" ? (
        <div className="appnav settings-nav">
          <div className="labels-head"><span>Settings</span></div>
          <p className="hint nav-hint">Accounts, filters, signatures, security and appearance.</p>
        </div>
      ) : (
        <>
          <div className="sidebar-fixed-nav">
            <NavList
              label="Mailboxes"
              groups={folderGroups}
              activeId={folderActiveId}
              onNavigate={openFolderItem}
              itemProps={(item, { active }) => ({
                className: `${item.id === "all" ? "folder all-inboxes" : "folder"}${active ? " active" : ""}`,
              })}
            />
            {accounts.length > 0 && (
              <>
                <div className="labels-head"><span>Views</span></div>
                <NavList
                  label="Views"
                  groups={viewGroups}
                  activeId=""
                  onNavigate={(id) => applyView(viewQueries[id])}
                  itemProps={(_item, { active }) => ({ className: `folder${active ? " active" : ""}` })}
                />
              </>
            )}
          </div>

          {savedSearches.length > 0 && (
            <section className="sidebar-saved-searches" aria-label="Saved searches">
              <div className="labels-head">
                <span>Saved searches</span>
              </div>
              <NavList
                label="Saved searches"
                groups={savedGroups}
                activeId=""
                onNavigate={(id) => applyView(id)}
                itemProps={() => ({ className: "folder" })}
                renderItem={(item, _state, row) => (
                  <div className="saved-search-row-wrap" title={item.id}>
                    {row}
                    <IconButton label="Remove saved search"
                      className="saved-search-remove"
                      title="Remove saved search"
                      aria-label={`Remove saved search ${item.label}`}
                      onClick={() => removeSavedSearch(item.id)}
                    >
                      x
                    </IconButton>
                  </div>
                )}
              />
            </section>
          )}

          {visibleLabels.length > 0 && (
            <section className="sidebar-labels" aria-label="Labels">
              <div className="labels-head labels-head--sticky">
                <span>Labels</span>
                <IconButton label="New label"
                  className="labels-add"
                  title="New label"
                  aria-label="New label"
                  onClick={createLabelUi}
                >
                  +
                </IconButton>
              </div>
              {/* Label twisties are per row (renderItem below): NavList's collapsedGroups/renderTwisty act on titled groups only (M2 exception B9). */}
              <NavList
                label="Labels"
                groups={labelGroups}
                activeId={activeLabelId}
                onNavigate={openLabel}
                itemProps={(item, { active }) => {
                  const l = labelById.get(item.id);
                  if (!l) return undefined;
                  const leaf = l.name.split("/").pop() || l.name;
                  return {
                    className: `folder label-row${dropLabelId === l.id ? " label-drop-target" : ""}${active ? " active label-active" : ""}`,
                    title: l.name,
                    ...labelDropHandlers(l.id, leaf),
                    onContextMenu: (e) => {
                      e.preventDefault();
                      e.stopPropagation();
                      setCtx({ x: e.clientX, y: e.clientY, kind: "label", labelId: l.id });
                    },
                  };
                }}
                renderItem={(item, _state, row) => {
                  const l = labelById.get(item.id);
                  if (!l) return row;
                  const depth = l.name.split("/").length - 1;
                  const collapsed = collapsedLabels.has(l.name);
                  return (
                    <div
                      className="label-row-wrap"
                      style={depth ? { paddingLeft: depth * 14 } : undefined}
                    >
                      {parentNames.has(l.name) && (
                        <IconButton label={collapsed ? "Expand" : "Collapse"}
                          className="label-twisty"
                          title={collapsed ? "Expand" : "Collapse"}
                          aria-label={collapsed ? "Expand" : "Collapse"}
                          onClick={() => toggleLabelCollapsed(l.name)}
                        >
                          <Icon name={collapsed ? "chevron-right" : "chevron-down"} size={14} />
                        </IconButton>
                      )}
                      {row}
                    </div>
                  );
                }}
              />
            </section>
          )}
        </>
      )}

    </aside>
  );
}
