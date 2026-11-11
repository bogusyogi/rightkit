import { Button, Dialog } from "@rightkit/react-shell/react";
import { eventDay, hhmm } from "../lib/calendarUtils.js";
import { useWorkspaceClient } from "../host.js";
import type { CalEvent } from "../client.js";

type CalendarEventPopoverProps = {
  openEvent: CalEvent;
  setOpenEvent: (ev: CalEvent | null) => void;
  openEventForm: (ev: CalEvent) => void;
  deleteEv: (ev: CalEvent) => void;
  rsvp: (ev: CalEvent, response: string) => void;
};

export function CalendarEventPopover({
  openEvent,
  setOpenEvent,
  openEventForm,
  deleteEv,
  rsvp,
}: CalendarEventPopoverProps) {
  const { openExternal } = useWorkspaceClient();
  return (
    <Dialog title={openEvent.summary || "(no title)"} width={420} onClose={() => setOpenEvent(null)}>
        <p className="event-when">
          {openEvent.all_day
            ? new Date(eventDay(openEvent)).toLocaleDateString([], { weekday: "long", month: "long", day: "numeric" }) + " · all day"
            : `${new Date(openEvent.start).toLocaleDateString([], { weekday: "long", month: "long", day: "numeric" })} · ${hhmm(openEvent.start)}${openEvent.end ? `–${hhmm(openEvent.end)}` : ""}`}
          {openEvent.is_recurring ? " · repeats" : ""}
        </p>
        {openEvent.location && <p className="hint">{openEvent.location}</p>}
        {openEvent.description && <p className="event-desc">{openEvent.description}</p>}
        {openEvent.attendees.length > 0 && (
          <p className="hint">{openEvent.attendees.length} guest{openEvent.attendees.length > 1 ? "s" : ""}</p>
        )}
        <div className="ws-detail-actions">
          {openEvent.meet_link && (
            <Button variant="primary" size="sm" onClick={() => openExternal(openEvent.meet_link)}>Join Meet</Button>
          )}
          {openEvent.my_response && (
            <>
              <Button variant="ghost" size="sm" onClick={() => rsvp(openEvent, "accepted")}>Yes</Button>
              <Button variant="ghost" size="sm" onClick={() => rsvp(openEvent, "tentative")}>Maybe</Button>
              <Button variant="ghost" size="sm" onClick={() => rsvp(openEvent, "declined")}>No</Button>
            </>
          )}
          <Button variant="ghost" size="sm" onClick={() => openEventForm(openEvent)}>Edit</Button>
          <Button variant="danger" size="sm" onClick={() => deleteEv(openEvent)}>Delete</Button>
        </div>
    </Dialog>
  );
}
