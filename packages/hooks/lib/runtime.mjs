const EVENT_FIELDS = Object.freeze({
  event: ['hook_event_name', 'hookEventName', 'event'],
  sessionId: ['thread_id', 'session_id', 'sessionId'],
  toolName: ['tool_name', 'toolName'],
  toolUseId: ['tool_use_id', 'toolUseId'],
});

function firstString(payload, names) {
  for (const name of names) {
    if (typeof payload[name] === 'string' && payload[name].trim()) return payload[name];
  }
  return null;
}

export function normalizeHookEvent(payload) {
  if (!payload || typeof payload !== 'object' || Array.isArray(payload)) {
    throw new TypeError('HookHost payload must be an object');
  }
  const normalized = Object.fromEntries(
    Object.entries(EVENT_FIELDS).map(([field, names]) => [field, firstString(payload, names)]),
  );
  if (!normalized.event) throw new TypeError('HookHost event is required');
  return Object.freeze({ schemaVersion: 1, ...normalized, payload: Object.freeze({ ...payload }) });
}

async function invoke(module, event, timeoutMs) {
  if (!module || typeof module.id !== 'string' || typeof module.handle !== 'function') {
    throw new TypeError('HookHost modules require id and handle');
  }
  const controller = new AbortController();
  let timer;
  try {
    const result = await Promise.race([
      Promise.resolve(module.handle(event, { signal: controller.signal })),
      new Promise((_, reject) => {
        timer = setTimeout(() => {
          controller.abort();
          reject(new Error(`HookHost module timed out: ${module.id}`));
        }, timeoutMs);
      }),
    ]);
    return Object.freeze({ id: module.id, status: 'ok', output: result ?? null });
  } catch (error) {
    return Object.freeze({ id: module.id, status: 'error', error: String(error?.message ?? error) });
  } finally {
    clearTimeout(timer);
  }
}

export async function dispatchHookEvent(payload, modules, { timeoutMs = 3000 } = {}) {
  if (!Array.isArray(modules)) throw new TypeError('HookHost modules must be an array');
  if (!Number.isInteger(timeoutMs) || timeoutMs < 1) throw new TypeError('HookHost timeout must be positive');
  const event = normalizeHookEvent(payload);
  const results = [];
  for (const module of modules) results.push(await invoke(module, event, timeoutMs));
  return Object.freeze({
    schemaVersion: 1,
    event: event.event,
    status: results.some(({ status }) => status === 'error') ? 'error' : 'ok',
    results: Object.freeze(results),
  });
}
