// Contacts' contextual sidebar (interim): Contacts/groups nav. The list, detail
// and CRUD live in ContactsView for now; Phase 3 lifts those into a shared hook
// and fills this nav with Create + import/export. Kept minimal so the column
// reads as intentional rather than empty.
import { useEffect } from "react";
import { NavList } from "@rightkit/react-shell/react";
import Icon from "../../components/Icons";
import { useAppStore } from "../../store";
import { listContactGroups } from "../../lib/ipc";

export function ContactsNav() {
  const activeAccount = useAppStore((s) => s.activeAccount);
  const contactGroups = useAppStore((s) => s.contactGroups);
  const setContactGroups = useAppStore((s) => s.setContactGroups);

  useEffect(() => {
    if (!activeAccount) return;
    listContactGroups(activeAccount).then(setContactGroups).catch(() => setContactGroups([]));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- setContactGroups is a store setter with unstable identity; tracking it would refetch contact groups on every render instead of only on account change.
  }, [activeAccount]);

  return (
    <div className="appnav">
      <NavList
        label="Contacts"
        groups={[{ items: [{ id: "contacts", label: "Contacts", ariaLabel: "Contacts", icon: <Icon name="people" size={18} className="f-ic" /> }] }]}
        activeId="contacts"
        onNavigate={() => {}}
        itemProps={(_item, { active }) => ({ className: `nav-shelf${active ? " active" : ""}` })}
      />
      {contactGroups.length > 0 && (
        <>
          <div className="labels-head"><span>Labels</span></div>
          <div className="nav-shelves">
            {contactGroups.map((g) => (
              <div className="nav-shelf" key={g.resource_name} title={g.name}>
                <Icon name="tag" size={16} className="f-ic" />
                <span className="f-name">{g.name}</span>
                {g.member_count > 0 && <span className="badge">{g.member_count}</span>}
              </div>
            ))}
          </div>
        </>
      )}
      <p className="hint nav-hint">Search, import/export and dedupe are in the panel.</p>
    </div>
  );
}
