// Calendar's contextual sidebar: Create + a mini-month date picker + a "My
// calendars" toggle list. Mirrors calendar.google.com's left rail. The mini
// month drives the main grid's date cursor; the toggles drive which calendars
// overlay via the shared store state.
import { Button, IconButton, NavList, type NavGroup } from "@rightkit/react-shell/react";
import Icon from "../../components/Icons";
import { DAY, startOfDay } from "../../lib/calendarUtils";
import { setPref } from "../../lib/preferences";
import { useAppStore } from "../../store";
import { useState } from "react";

const ALL_ID = "__all";
const DOW = ["S", "M", "T", "W", "T", "F", "S"];

function MiniMonth() {
  const calCursor = useAppStore((s) => s.calCursor);
  const setCalCursor = useAppStore((s) => s.setCalCursor);
  const d = new Date(calCursor);
  const first = new Date(d.getFullYear(), d.getMonth(), 1);
  const gridStart = first.getTime() - first.getDay() * DAY;
  const [today] = useState(() => startOfDay(Date.now()));
  const cursorDay = startOfDay(calCursor);
  const month = d.getMonth();

  function shiftMonth(dir: -1 | 1) {
    setCalCursor(new Date(d.getFullYear(), d.getMonth() + dir, Math.min(d.getDate(), 28)).getTime());
  }

  return (
    <div className="mini-month">
      <div className="mini-head">
        <span className="mini-label">{d.toLocaleDateString([], { month: "long", year: "numeric" })}</span>
        <div className="mini-nav">
          <IconButton label="Previous month" className="mini-arrow" title="Previous month" onClick={() => shiftMonth(-1)}>
            <Icon name="chevron-left" size={15} />
          </IconButton>
          <IconButton label="Next month" className="mini-arrow" title="Next month" onClick={() => shiftMonth(1)}>
            <Icon name="chevron-right" size={15} />
          </IconButton>
        </div>
      </div>
      <div className="mini-dow">
        {DOW.map((w, i) => <span key={i}>{w}</span>)}
      </div>
      <div className="mini-grid">
        {Array.from({ length: 42 }, (_, i) => {
          const ms = gridStart + i * DAY;
          const cell = new Date(ms);
          const cls = [
            "mini-day",
            cell.getMonth() === month ? "" : "out",
            ms === today ? "today" : "",
            ms === cursorDay ? "sel" : "",
          ].filter(Boolean).join(" ");
          return (
            <Button variant="ghost" key={i} className={cls} onClick={() => setCalCursor(ms)}>
              {cell.getDate()}
            </Button>
          );
        })}
      </div>
    </div>
  );
}

export function CalendarNav() {
  const calendars = useAppStore((s) => s.calendars);
  const selectedCal = useAppStore((s) => s.selectedCal);
  const shownCals = useAppStore((s) => s.shownCals);
  const setShownCals = useAppStore((s) => s.setShownCals);
  const setEventSeed = useAppStore((s) => s.setEventSeed);

  const allIds = calendars.map((c) => c.id);
  const allShown = calendars.length > 0 && shownCals.length === calendars.length;

  function persist(next: string[]) {
    setShownCals(next);
    setPref("shownCals", next);
  }
  /** Checkbox: add/remove one calendar from the overlay set (fine multi-select). */
  function toggle(calId: string) {
    setShownCals((prev) => {
      const next = prev.includes(calId) ? prev.filter((c) => c !== calId) : [...prev, calId];
      setPref("shownCals", next);
      return next;
    });
  }
  /** Row click: show only this calendar (like clicking a single inbox in Mail). */
  function solo(calId: string) {
    persist([calId]);
  }
  /** "All calendars": combine every calendar into one view (like All inboxes). */
  function showAll() {
    persist(allIds);
  }

  const calendarGroups: NavGroup[] = [
    {
      items: [
        {
          id: ALL_ID,
          label: "All calendars",
          ariaLabel: "Combine all calendars into one view",
          icon: <span className="cal-swatch cal-swatch-all" />,
        },
        ...calendars.map((c) => ({
          id: c.id,
          label: c.summary,
          ariaLabel: `Show only ${c.summary}`,
          icon: <span className="cal-swatch" style={{ background: c.background_color || "var(--copper)" }} />,
        })),
      ],
    },
  ];

  return (
    <div className="appnav">
      <Button variant="primary" className="nav-new" title="Create event" onClick={() => setEventSeed({ summary: "", withMeet: false })}>
        <Icon name="plus" size={20} />
        <span>Create</span>
      </Button>

      <MiniMonth />

      <div className="labels-head"><span>My calendars</span></div>
      {calendars.length === 0 ? (
        <p className="hint nav-hint">No calendars yet.</p>
      ) : (
        <NavList
          label="My calendars"
          groups={calendarGroups}
          activeId={allShown ? ALL_ID : shownCals.length === 1 ? shownCals[0] : ""}
          onNavigate={(id) => (id === ALL_ID ? showAll() : solo(id))}
          itemCheck={(item) => {
            const c = calendars.find((x) => x.id === item.id);
            if (!c) return undefined;
            return {
              checked: shownCals.length ? shownCals.includes(c.id) : c.id === selectedCal,
              onChange: () => toggle(c.id),
            };
          }}
        />
      )}
    </div>
  );
}
