import { Button, Dialog, Input, Checkbox, Select } from "@rightkit/react-shell/react";
import { WEEKDAYS } from "@rightkit/mail-ui/lib/mail";
import type { EventForm } from "@rightkit/mail-ui/store/types";

type CalendarEventFormModalProps = {
  eventForm: EventForm;
  setEventForm: (form: EventForm | null) => void;
  freeBusyMsg: string;
  checkAvailability: () => void;
  submitEvent: () => void;
};

export function CalendarEventFormModal({
  eventForm,
  setEventForm,
  freeBusyMsg,
  checkAvailability,
  submitEvent,
}: CalendarEventFormModalProps) {
  return (
    <Dialog title={eventForm.id ? "Edit event" : "New event"} width={460} onClose={() => setEventForm(null)}>
        <div className="ws-form">
          <Input
            className="event-title-input"
            placeholder="Add title"
            autoFocus
            value={eventForm.summary}
            onChange={(e) => setEventForm({ ...eventForm, summary: e.target.value })}
          />
          <label className="ws-form-row">
            <span>Start</span>
            <Input
              type={eventForm.allDay ? "date" : "datetime-local"}
              value={eventForm.allDay ? eventForm.start.slice(0, 10) : eventForm.start}
              onChange={(e) => setEventForm({ ...eventForm, start: eventForm.allDay ? `${e.target.value}T09:00` : e.target.value })}
            />
          </label>
          <label className="ws-form-row">
            <span>End</span>
            <Input
              type={eventForm.allDay ? "date" : "datetime-local"}
              value={eventForm.allDay ? eventForm.end.slice(0, 10) : eventForm.end}
              onChange={(e) => setEventForm({ ...eventForm, end: eventForm.allDay ? `${e.target.value}T10:00` : e.target.value })}
            />
          </label>
          <div className="label-toggle">
            <Checkbox label="All day" checked={eventForm.allDay} onChange={(on) => setEventForm({ ...eventForm, allDay: on })} />
          </div>
          <Input
            placeholder="Guests (comma-separated emails)"
            value={eventForm.attendees}
            onChange={(e) => setEventForm({ ...eventForm, attendees: e.target.value })}
          />
          <label className="ws-form-row">
            <span>Remind (min before)</span>
            <Input type="number" style={{ width: 80 }} value={eventForm.reminderMinutes} onChange={(e) => setEventForm({ ...eventForm, reminderMinutes: e.target.value })} />
          </label>
          <div className="label-toggle">
            <Checkbox label="Add Google Meet video call" checked={eventForm.addMeet} onChange={(on) => setEventForm({ ...eventForm, addMeet: on })} />
          </div>
          <label className="ws-form-row">
            <span>Repeats</span>
            <Select
              label="Repeats"
              value={eventForm.recurrence}
              options={[
                { value: "", label: "Does not repeat" },
                { value: "DAILY", label: "Daily" },
                { value: "WEEKLY", label: "Weekly" },
                { value: "MONTHLY", label: "Monthly" },
                { value: "YEARLY", label: "Yearly" },
              ]}
              onChange={(v) => setEventForm({ ...eventForm, recurrence: v })}
            />
          </label>
          {eventForm.recurrence === "WEEKLY" && (
            <div className="ws-form-row">
              <span>On</span>
              <div className="byday-row">
                {WEEKDAYS.map(([code, label]) => {
                  const on = eventForm.recurByday.includes(code);
                  return (
                    <Button variant="ghost"
                      key={code}
                      type="button"
                      className={`chip ${on ? "on" : ""}`}
                      title={label}
                      onClick={() => setEventForm({ ...eventForm, recurByday: on ? eventForm.recurByday.filter((d) => d !== code) : [...eventForm.recurByday, code] })}
                    >
                      {label[0]}
                    </Button>
                  );
                })}
              </div>
            </div>
          )}
          {eventForm.recurrence && (
            <div className="ws-form-row">
              <span>Ends</span>
              <Select
                label="Ends"
                value={eventForm.recurEnd}
                options={[
                  { value: "never", label: "Never" },
                  { value: "until", label: "On date" },
                  { value: "count", label: "After…" },
                ]}
                onChange={(v) => setEventForm({ ...eventForm, recurEnd: v })}
              />
              {eventForm.recurEnd === "until" && (
                <Input type="date" min={eventForm.start.slice(0, 10)} value={eventForm.recurUntil} onChange={(e) => setEventForm({ ...eventForm, recurUntil: e.target.value })} />
              )}
              {eventForm.recurEnd === "count" && (
                <span className="recur-count">
                  <Input type="number" min={1} max={999} value={eventForm.recurCount} onChange={(e) => setEventForm({ ...eventForm, recurCount: e.target.value })} />
                  occurrences
                </span>
              )}
            </div>
          )}
          {eventForm.attendees.trim() && (
            <div className="ws-form-row">
              <Button variant="ghost" size="sm" onClick={checkAvailability}>Check availability</Button>
              {freeBusyMsg && <span className="hint">{freeBusyMsg}</span>}
            </div>
          )}
          <div className="ws-form-foot">
            <Button variant="primary" onClick={submitEvent}>{eventForm.id ? "Save" : "Create"}</Button>
            <Button variant="ghost" size="sm" onClick={() => setEventForm(null)}>Cancel</Button>
          </div>
        </div>
    </Dialog>
  );
}
