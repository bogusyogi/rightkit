// Structured lifecycle events: one JSON object per line on stdout, so a schedule log is machine-readable.

export type EventName = "update.run.start" | "update.run.end" | "update.gate.result" | "update.land.result";

export type EventSink = (event: EventName, data?: Record<string, unknown>) => void;

export function jsonLineSink(write: (text: string) => void, now: () => Date = () => new Date()): EventSink {
  return (event, data = {}) => {
    write(`${JSON.stringify({ event, ts: now().toISOString(), ...data })}\n`);
  };
}

export const noEvents: EventSink = () => undefined;
