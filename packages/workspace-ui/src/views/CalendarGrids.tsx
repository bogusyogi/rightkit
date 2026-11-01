import { Button } from "@rightkit/react-shell/react";
import { useEffect, useMemo, useRef } from "react";
import type { CalEvent } from "@rightkit/mail-ui/lib/ipc";
import { DAY, WEEKDAY_LABELS, eventDay, hhmm, sameDay, startOfWeek } from "@rightkit/mail-ui/lib/calendarUtils";

export function MonthGrid({ cursor, events, today, onDay, onEvent }: {
  cursor: number; events: CalEvent[]; today: number; onDay: (ms: number) => void; onEvent: (e: CalEvent) => void;
}) {
  const d = new Date(cursor);
  const first = new Date(d.getFullYear(), d.getMonth(), 1).getTime();
  const gridStart = startOfWeek(first);
  const month = d.getMonth();
  const byDay = useMemo(() => {
    const m = new Map<number, CalEvent[]>();
    for (const e of events) {
      const k = eventDay(e);
      (m.get(k) ?? m.set(k, []).get(k)!).push(e);
    }
    return m;
  }, [events]);
  return (
    <div className="month-grid">
      <div className="month-dow">
        {WEEKDAY_LABELS.map((w) => <div key={w} className="dow-cell">{w}</div>)}
      </div>
      <div className="month-cells">
        {Array.from({ length: 42 }, (_, i) => {
          const ms = gridStart + i * DAY;
          const day = new Date(ms);
          const inMonth = day.getMonth() === month;
          const isToday = ms === today;
          const dayEvents = byDay.get(ms) ?? [];
          return (
            <div
              key={i}
              className={`month-cell ${inMonth ? "" : "out"} ${isToday ? "today" : ""}`}
              onClick={() => onDay(ms)}
            >
              <Button variant="ghost" className="hit-overlay" aria-label={day.toDateString()} />
              <div className="month-daynum">
                <span className={isToday ? "today-pill" : ""}>{day.getDate()}</span>
              </div>
              <div className="month-events">
                {dayEvents.slice(0, 4).map((e) => (
                  <Button variant="ghost"
                    key={e.id}
                    className={`cal-chip ${e.all_day ? "allday" : ""}`}
                    title={e.summary}
                    onClick={(ev) => { ev.stopPropagation(); onEvent(e); }}
                  >
                    {!e.all_day && <span className="chip-dot" />}
                    {!e.all_day && <span className="chip-time">{hhmm(e.start)}</span>}
                    <span className="chip-name">{e.summary || "(no title)"}</span>
                  </Button>
                ))}
                {dayEvents.length > 4 && <span className="chip-more">+{dayEvents.length - 4} more</span>}
              </div>
            </div>
          );
        })}
      </div>
    </div>
  );
}

const HOUR_PX = 48;
const HOURS = Array.from({ length: 24 }, (_, h) => h);
const formatHour = (h: number) => `${(h % 12) || 12} ${h < 12 ? "AM" : "PM"}`;
const minOf = (iso: string) => {
  const d = new Date(iso);
  return d.getHours() * 60 + d.getMinutes();
};

function packLanes(evs: CalEvent[]): Map<string, { lane: number; lanes: number }> {
  const res = new Map<string, { lane: number; lanes: number }>();
  const items = evs
    .map((e) => {
      const s = new Date(e.start).getTime();
      const en = e.end ? new Date(e.end).getTime() : s + 3600000;
      return { id: e.id, s, en: Math.max(en, s + 15 * 60000) };
    })
    .sort((a, b) => a.s - b.s);
  let i = 0;
  while (i < items.length) {
    let clusterEnd = items[i].en;
    const cluster = [items[i]];
    let j = i;
    while (j + 1 < items.length && items[j + 1].s < clusterEnd) {
      j++;
      cluster.push(items[j]);
      clusterEnd = Math.max(clusterEnd, items[j].en);
    }
    const laneEnds: number[] = [];
    for (const it of cluster) {
      let lane = laneEnds.findIndex((end) => end <= it.s);
      if (lane === -1) {
        lane = laneEnds.length;
        laneEnds.push(it.en);
      } else laneEnds[lane] = it.en;
      res.set(it.id, { lane, lanes: 0 });
    }
    for (const it of cluster) res.get(it.id)!.lanes = laneEnds.length;
    i = j + 1;
  }
  return res;
}

export function TimeGrid({ days, events, today, nowMin, onSlot, onEvent }: {
  days: number[]; events: CalEvent[]; today: number; nowMin: number; onSlot: (ms: number) => void; onEvent: (e: CalEvent) => void;
}) {
  const bodyRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (bodyRef.current) bodyRef.current.scrollTop = 7 * HOUR_PX;
  }, []);
  const cols = `64px repeat(${days.length}, 1fr)`;
  return (
    <div className="time-grid">
      <div className="tg-head" style={{ gridTemplateColumns: cols }}>
        <div className="tg-corner" />
        {days.map((ms) => {
          const d = new Date(ms);
          return (
            <div key={ms} className={`tg-day-head ${ms === today ? "today" : ""}`}>
              <span className="tg-dow">{WEEKDAY_LABELS[d.getDay()]}</span>
              <span className={`tg-daynum ${ms === today ? "today-pill" : ""}`}>{d.getDate()}</span>
            </div>
          );
        })}
      </div>
      <div className="tg-allday" style={{ gridTemplateColumns: cols }}>
        <div className="tg-corner tg-allday-label">all-day</div>
        {days.map((ms) => {
          const ad = events.filter((e) => e.all_day && sameDay(eventDay(e), ms));
          return (
            <div key={ms} className="tg-allday-col">
              {ad.map((e) => (
                <Button variant="ghost" key={e.id} className="cal-chip allday" title={e.summary} onClick={() => onEvent(e)}>
                  <span className="chip-name">{e.summary || "(no title)"}</span>
                </Button>
              ))}
            </div>
          );
        })}
      </div>
      <div className="tg-body" ref={bodyRef} style={{ gridTemplateColumns: cols }}>
        <div className="tg-gutter">
          {HOURS.map((h) => (
            <div key={h} className="tg-hour-label" style={{ height: HOUR_PX }}>
              {h === 0 ? "" : formatHour(h)}
            </div>
          ))}
        </div>
        {days.map((ms) => {
          const dayEvents = events
            .filter((e) => !e.all_day && sameDay(eventDay(e), ms))
            .sort((a, b) => new Date(a.start).getTime() - new Date(b.start).getTime());
          const lanes = packLanes(dayEvents);
          return (
            <div
              key={ms}
              className={`tg-col ${ms === today ? "today" : ""}`}
              style={{ height: 24 * HOUR_PX }}
              onClick={(ev) => {
                const rect = (ev.currentTarget as HTMLElement).getBoundingClientRect();
                const hour = Math.max(0, Math.min(23, Math.floor((ev.clientY - rect.top) / HOUR_PX)));
                onSlot(ms + hour * 3600000);
              }}
            >
              {HOURS.map((h) => (
                <div key={h} className="tg-hline" style={{ top: h * HOUR_PX }} />
              ))}
              {ms === today && <div className="tg-now" style={{ top: (nowMin / 60) * HOUR_PX }} />}
              {dayEvents.map((e) => {
                const startMin = minOf(e.start);
                const rawEnd = e.end ? minOf(e.end) + (new Date(e.end).getDate() !== new Date(e.start).getDate() ? 1440 : 0) : startMin + 60;
                const endMin = Math.max(startMin + 20, rawEnd);
                const li = lanes.get(e.id) ?? { lane: 0, lanes: 1 };
                const w = 100 / li.lanes;
                return (
                  <Button variant="ghost"
                    key={e.id}
                    className="tg-event"
                    style={{
                      top: (startMin / 60) * HOUR_PX,
                      height: ((endMin - startMin) / 60) * HOUR_PX,
                      left: `calc(${li.lane * w}% + 2px)`,
                      width: `calc(${w}% - 4px)`,
                    }}
                    title={e.summary}
                    onClick={(ev) => { ev.stopPropagation(); onEvent(e); }}
                  >
                    <span className="tg-event-name">{e.summary || "(no title)"}</span>
                    <span className="tg-event-time">{hhmm(e.start)}{e.meet_link ? " · Meet" : ""}</span>
                  </Button>
                );
              })}
            </div>
          );
        })}
      </div>
    </div>
  );
}
