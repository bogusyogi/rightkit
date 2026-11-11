import { Button, IconButton, Dialog, SegmentedControl } from "@rightkit/react-shell/react";
import { memo, useEffect, useRef, useState } from "react";
import { Icon } from "../icons.js";
import { MonthGrid, TimeGrid } from "./CalendarGrids.js";
import { DAY, rangeFor, startOfDay, startOfWeek } from "../lib/calendarUtils.js";
import { CalendarEventFormModal } from "./CalendarEventFormModal.js";
import { CalendarEventPopover } from "./CalendarEventPopover.js";
import { HostAccountSwitcher, useWorkspaceClient, useWorkspaceHost, useWorkspaceState } from "../host.js";
import type { CalEvent, EventInput } from "../client.js";
const CALENDAR_VIEWS = ["month", "week", "day"] as const;
function CalendarViewInner() {
  const [recurScope, setRecurScope] = useState<{ ev: CalEvent; mode: "edit" | "delete" } | null>(null);
  const { listCalendars, listEvents, createEvent, updateEvent, deleteEvent, truncateEventSeries, rsvpEvent, freeBusy } = useWorkspaceClient();
  const showDialog = useWorkspaceHost().dialog;
  const setPref = useWorkspaceHost().savePreference;
  const activeAccount = useWorkspaceState((s) => s.activeAccount);
  const setErr = useWorkspaceHost().reportError;
  const showUndo = useWorkspaceHost().notify;
  const setCalendars = useWorkspaceState((s) => s.setCalendars);
  const selectedCal = useWorkspaceState((s) => s.selectedCal);
  const setSelectedCal = useWorkspaceState((s) => s.setSelectedCal);
  const shownCals = useWorkspaceState((s) => s.shownCals);
  const setShownCals = useWorkspaceState((s) => s.setShownCals);
  const calView = useWorkspaceState((s) => s.calView);
  const setCalView = useWorkspaceState((s) => s.setCalView);
  const calCursor = useWorkspaceState((s) => s.calCursor);
  const setCalCursor = useWorkspaceState((s) => s.setCalCursor);
  const events = useWorkspaceState((s) => s.events);
  const setEvents = useWorkspaceState((s) => s.setEvents);
  const openEvent = useWorkspaceState((s) => s.openEvent);
  const setOpenEvent = useWorkspaceState((s) => s.setOpenEvent);
  const eventForm = useWorkspaceState((s) => s.eventForm);
  const setEventForm = useWorkspaceState((s) => s.setEventForm);
  const freeBusyMsg = useWorkspaceState((s) => s.freeBusyMsg);
  const setFreeBusyMsg = useWorkspaceState((s) => s.setFreeBusyMsg);
  const setCalendarBusy = useWorkspaceState((s) => s.setCalendarBusy);
  const eventSeed = useWorkspaceState((s) => s.eventSeed);
  const setEventSeed = useWorkspaceState((s) => s.setEventSeed);
  const [calendarNow] = useState(() => {
    const now = new Date();
    return {
      today: startOfDay(now.getTime()),
      minute: now.getHours() * 60 + now.getMinutes(),
    };
  });
  async function reloadEvents() {
    if (!activeAccount) return;
    const [minMs, maxMs] = rangeFor(calCursor, calView);
    const min = new Date(minMs).toISOString();
    const max = new Date(maxMs).toISOString();
    const cals = Array.from(new Set(shownCals.length ? shownCals : [selectedCal]));
    setCalendarBusy(true);
    try {
      const lists = await Promise.all(
        cals.map((c) => listEvents(activeAccount, c, min, max).catch(() => []))
      );
      setEvents(lists.flat().sort((a, b) => {
        const ax = a.start ? new Date(a.start).getTime() : 0;
        const bx = b.start ? new Date(b.start).getTime() : 0;
        return ax - bx;
      }));
    } catch (e) {
      setErr(String(e));
    } finally {
      setCalendarBusy(false);
    }
  }
  function toLocalInput(s: string): string {
    try {
      const d = new Date(s);
      return new Date(d.getTime() - d.getTimezoneOffset() * 60000).toISOString().slice(0, 16);
    } catch {
      return "";
    }
  }
  function buildRrule(f: NonNullable<typeof eventForm>): string | null {
    if (!f.recurrence) return null;
    const parts = [`FREQ=${f.recurrence}`];
    if (f.recurrence === "WEEKLY" && f.recurByday.length > 0) parts.push(`BYDAY=${f.recurByday.join(",")}`);
    if (f.recurEnd === "until" && f.recurUntil) {
      const ymd = f.recurUntil.replace(/-/g, "");
      parts.push(f.allDay ? `UNTIL=${ymd}` : `UNTIL=${ymd}T235959Z`);
    } else if (f.recurEnd === "count") {
      const n = Math.max(1, parseInt(f.recurCount, 10) || 0);
      if (n > 0) parts.push(`COUNT=${n}`);
    }
    return `RRULE:${parts.join(";")}`;
  }
  function fillEditForm(ev: CalEvent, targetId: string) {
    setRecurScope(null);
    setEventForm({
      id: targetId,
      summary: ev.summary,
      start: ev.all_day ? `${ev.start.slice(0, 10)}T09:00` : toLocalInput(ev.start),
      end: ev.all_day ? `${ev.end.slice(0, 10)}T10:00` : toLocalInput(ev.end),
      allDay: ev.all_day,
      attendees: ev.attendees.map((a) => a.email).join(", "),
      reminderMinutes: "",
      addMeet: !!ev.meet_link,
      recurrence: "",
      recurEnd: "never",
      recurUntil: "",
      recurCount: "",
      recurByday: [],
    });
    setFreeBusyMsg("");
  }
  /** Open the new/edit form. `seedDay` (ms) pre-dates a new event from a grid click. */
  function openEventForm(ev?: CalEvent, prefillSummary?: string, withMeet = false, seedDay?: number) {
    setOpenEvent(null);
    if (ev) {
      if (ev.is_recurring && ev.recurring_event_id) {
        setRecurScope({ ev, mode: "edit" });
        return;
      }
      fillEditForm(ev, ev.id);
      return;
    }
    // seedDay is the exact start time for a grid-click (month cells pass 9am,
    // the time grid passes the clicked hour); otherwise round to the next hour.
    const base = seedDay !== undefined ? new Date(seedDay) : new Date();
    if (seedDay === undefined) base.setMinutes(0, 0, 0);
    const start = new Date(base.getTime() - base.getTimezoneOffset() * 60000);
    const end = new Date(start.getTime() + 3600000);
    setEventForm({
      summary: prefillSummary ?? "",
      start: start.toISOString().slice(0, 16),
      end: end.toISOString().slice(0, 16),
      allDay: false,
      attendees: "",
      reminderMinutes: "10",
      addMeet: withMeet,
      recurrence: "",
      recurEnd: "never",
      recurUntil: "",
      recurCount: "",
      recurByday: [],
    });
    setFreeBusyMsg("");
  }
  async function checkAvailability() {
    if (!activeAccount || !eventForm) return;
    const guests = eventForm.attendees.split(",").map((s) => s.trim()).filter(Boolean);
    if (guests.length === 0) {
      setFreeBusyMsg("Add guests first.");
      return;
    }
    const startIso = eventForm.allDay ? `${eventForm.start.slice(0, 10)}T00:00:00Z` : new Date(eventForm.start).toISOString();
    const endIso = eventForm.allDay ? `${eventForm.end.slice(0, 10)}T23:59:59Z` : new Date(eventForm.end || eventForm.start).toISOString();
    setFreeBusyMsg("Checking…");
    try {
      const res = await freeBusy(activeAccount, guests, startIso, endIso);
      const busy = res.filter((r) => r.busy.length > 0).map((r) => r.calendar_id);
      setFreeBusyMsg(busy.length === 0 ? "All guests are free." : `Busy: ${busy.join(", ")}`);
    } catch (e) {
      setFreeBusyMsg(String(e));
    }
  }
  async function submitEvent() {
    if (!activeAccount || !eventForm) return;
    if (!eventForm.summary.trim() || !eventForm.start) {
      setErr("Event needs a title and start time.");
      return;
    }
    const f = eventForm;
    const ev: EventInput = {
      summary: f.summary,
      startRfc3339: f.allDay ? f.start.slice(0, 10) : new Date(f.start).toISOString(),
      endRfc3339: f.allDay ? f.end.slice(0, 10) : new Date(f.end || f.start).toISOString(),
      allDay: f.allDay,
      timeZone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
      attendees: f.attendees.split(/[,;\s]+/).map((s) => s.trim()).filter(Boolean),
      reminderMinutes: f.reminderMinutes ? Number(f.reminderMinutes) : null,
      addMeet: f.addMeet,
      recurrence: buildRrule(f),
    };
    try {
      if (f.id) await updateEvent(activeAccount, selectedCal, f.id, ev);
      else await createEvent(activeAccount, selectedCal, ev);
      setEventForm(null);
      await reloadEvents();
      showUndo(f.id ? "Event updated" : "Event created");
    } catch (e) {
      setErr(String(e));
    }
  }
  function performDelete(ev: CalEvent, targetId: string) {
    if (!activeAccount) return;
    setRecurScope(null);
    deleteEvent(activeAccount, ev.calendar_id || selectedCal, targetId)
      .then(() => {
        setOpenEvent(null);
        return reloadEvents();
      })
      .then(() => showUndo("Event deleted"))
      .catch((e) => setErr(String(e)));
  }
  function performTruncate(ev: CalEvent) {
    if (!activeAccount) return;
    setRecurScope(null);
    truncateEventSeries(activeAccount, ev.calendar_id || selectedCal, ev.recurring_event_id, ev.start)
      .then(() => {
        setOpenEvent(null);
        return reloadEvents();
      })
      .then(() => showUndo("This and following events removed"))
      .catch((e) => setErr(String(e)));
  }
  async function deleteEv(ev: CalEvent) {
    if (!activeAccount) return;
    if (ev.is_recurring && ev.recurring_event_id) {
      setRecurScope({ ev, mode: "delete" });
      return;
    }
    if ((await showDialog({ kind: "confirm", message: `Delete "${ev.summary}"? Guests will be notified.` })) !== "ok") return;
    performDelete(ev, ev.id);
  }
  function rsvp(ev: CalEvent, response: string) {
    if (!activeAccount) return;
    rsvpEvent(activeAccount, ev.calendar_id || selectedCal, ev.id, activeAccount, response)
      .then(() => reloadEvents())
      .then(() => {
        setOpenEvent(null);
        showUndo(`RSVP: ${response}`);
      })
      .catch((e) => setErr(String(e)));
  }
  // Load calendars once per account; pick the primary as the create target.
  useEffect(() => {
    setEventForm(null);
    setOpenEvent(null);
    if (!activeAccount) return;
    setCalendarBusy(true);
    listCalendars(activeAccount)
      .then((cals) => {
        setCalendars(cals);
        const cal = cals.find((c) => c.primary)?.id || cals[0]?.id || "primary";
        setSelectedCal(cal);
        // Like Google web, show ALL calendars by default the first time (so the
        // grid isn't blank when events live on secondary calendars). Only seed
        // when the user hasn't already chosen a subset.
        if (shownCals.length === 0 && cals.length > 0) {
          const all = cals.map((c) => c.id);
          setShownCals(all);
          setPref("shownCals", all);
        }
      })
      .catch((e) => setErr(String(e)))
      .finally(() => setCalendarBusy(false));
    // eslint-disable-next-line react-hooks/exhaustive-deps -- setEventForm/setOpenEvent/setErr/shownCals.length/setShownCals are store setters or a derived read; tracking them would refetch calendars on every render instead of only on account change.
  }, [activeAccount]);
  // Re-fetch whenever the focused period, view, account, or visible calendars change.
  useEffect(() => {
    if (!activeAccount) return;
    reloadEvents();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- reloadEvents is a plain function redefined every render (closes over selectedCal/setErr); tracking it would refetch events on every render instead of only on the five real scope inputs.
  }, [activeAccount, calCursor, calView, shownCals, selectedCal]);
  // "Create event from email" / "Schedule meeting" → open the prefilled form.
  useEffect(() => {
    if (!eventSeed) return;
    const id = window.setTimeout(() => {
      openEventForm(undefined, eventSeed.summary, eventSeed.withMeet);
      setEventSeed(null);
    }, 0);
    return () => window.clearTimeout(id);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- openEventForm/setEventSeed are recreated every render; tracking them would clear+rearm the timeout on every render instead of only when a new eventSeed arrives.
  }, [eventSeed]);
  function step(dir: -1 | 1) {
    const d = new Date(calCursor);
    if (calView === "day") setCalCursor(calCursor + dir * DAY);
    else if (calView === "week") setCalCursor(calCursor + dir * 7 * DAY);
    else setCalCursor(new Date(d.getFullYear(), d.getMonth() + dir, Math.min(d.getDate(), 28)).getTime());
  }
  // Wheel over the grid pages the period (Google-style), throttled so one
  // gesture = one step rather than flying through months.
  const wheelLock = useRef(0);
  function onWheel(e: React.WheelEvent) {
    if (Math.abs(e.deltaY) < 4) return;
    const now = Date.now();
    if (now - wheelLock.current < 320) return;
    wheelLock.current = now;
    step(e.deltaY > 0 ? 1 : -1);
  }
  function pickView(v: "month" | "week" | "day") {
    setCalView(v);
    setPref("calView", v);
  }
  const headerLabel = (() => {
    const d = new Date(calCursor);
    if (calView === "day") return d.toLocaleDateString([], { weekday: "long", month: "long", day: "numeric", year: "numeric" });
    if (calView === "week") {
      const s = new Date(startOfWeek(calCursor));
      const e = new Date(startOfWeek(calCursor) + 6 * DAY);
      const sameMonth = s.getMonth() === e.getMonth();
      return sameMonth
        ? `${s.toLocaleDateString([], { month: "long" })} ${s.getDate()}–${e.getDate()}, ${e.getFullYear()}`
        : `${s.toLocaleDateString([], { month: "short", day: "numeric" })} – ${e.toLocaleDateString([], { month: "short", day: "numeric", year: "numeric" })}`;
    }
    return d.toLocaleDateString([], { month: "long", year: "numeric" });
  })();
  return (
    <div className="calview">
      <div className="cal-bar">
        <Button variant="ghost" className="cal-today" onClick={() => setCalCursor(Date.now())}>Today</Button>
        <IconButton label="Previous" className="cal-step" title="Previous" onClick={() => step(-1)}>
          <Icon name="chevron-left" size={18} />
        </IconButton>
        <IconButton label="Next" className="cal-step" title="Next" onClick={() => step(1)}>
          <Icon name="chevron-right" size={18} />
        </IconButton>
        <h2 className="cal-label">{headerLabel}</h2>
        <div className="cal-bar-spacer" />
        <div className="cal-viewtoggle">
          <SegmentedControl
            label="Calendar view"
            value={calView}
            options={CALENDAR_VIEWS.map((v) => ({ value: v, label: v[0].toUpperCase() + v.slice(1) }))}
            onChange={(v) => pickView(v)}
          />
        </div>
        <HostAccountSwitcher />
      </div>
      <div className="cal-grid" onWheel={onWheel}>
        {calView === "month" ? (
          <MonthGrid
            cursor={calCursor}
            events={events}
            today={calendarNow.today}
            onDay={(ms) => openEventForm(undefined, undefined, false, ms + 9 * 3600000)}
            onEvent={setOpenEvent}
          />
        ) : (
          <TimeGrid
            days={
              calView === "week"
                ? Array.from({ length: 7 }, (_, i) => startOfWeek(calCursor) + i * DAY)
                : [startOfDay(calCursor)]
            }
            events={events}
            today={calendarNow.today}
            nowMin={calendarNow.minute}
            onSlot={(ms) => openEventForm(undefined, undefined, false, ms)}
            onEvent={setOpenEvent}
          />
        )}
      </div>
      {openEvent && (
        <CalendarEventPopover
          openEvent={openEvent}
          setOpenEvent={setOpenEvent}
          openEventForm={openEventForm}
          deleteEv={deleteEv}
          rsvp={rsvp}
        />
      )}
      {eventForm && (
        <CalendarEventFormModal
          eventForm={eventForm}
          setEventForm={setEventForm}
          freeBusyMsg={freeBusyMsg}
          checkAvailability={checkAvailability}
          submitEvent={submitEvent}
        />
      )}
      {recurScope && (
        <Dialog title={recurScope.mode === "edit" ? "Edit recurring event" : "Delete recurring event"} width={420} onClose={() => setRecurScope(null)}>
            <p className="hint">“{recurScope.ev.summary}” repeats. Apply to:</p>
            <div className="recur-choices">
              <Button variant="primary" onClick={() => recurScope.mode === "edit" ? fillEditForm(recurScope.ev, recurScope.ev.id) : performDelete(recurScope.ev, recurScope.ev.id)}>This event</Button>
              {recurScope.mode === "delete" && (
                <Button variant="primary" onClick={() => performTruncate(recurScope.ev)}>This and following events</Button>
              )}
              <Button variant="primary" onClick={() => recurScope.mode === "edit" ? fillEditForm(recurScope.ev, recurScope.ev.recurring_event_id) : performDelete(recurScope.ev, recurScope.ev.recurring_event_id)}>All events</Button>
            </div>
        </Dialog>
      )}
    </div>
  );
}
export const CalendarView = memo(CalendarViewInner);
