import { createRightKitTauriClient } from '../src/index.js';

type License = { key: string };
type Device = { id: string };
type LogBundle = { events: unknown[] };
type LogEvent = { event: string };
type Diagnostics = string;

const client = createRightKitTauriClient<License, Device, LogBundle, LogEvent, Diagnostics>({
  invoke: async <T>() => undefined as T,
});

const logs: Promise<LogBundle> = client.logs.collect();
const diagnostics: Promise<Diagnostics> = client.diagnostics.collect();

void logs;
void diagnostics;
