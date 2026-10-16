import { authenticate, type Principal } from './auth.js';
import { compactTombstones, pruneHousekeeping } from './compact.js';
import { QueryBudget } from './shards.js';
import { eraseAccount } from './erase.js';
import { admit } from './guard.js';
import { errorResponse, HttpError, isObject, json, readJson } from './http.js';
import { config, MAX_BATCH, PROTOCOL, type Env } from './env.js';
import { fetchChanges, fetchRecord, prepareZone, saveRecords } from './records.js';

const ZONE = /^[A-Za-z0-9._-]{1,64}$/;

function segment(raw: string, what: string): string {
  try {
    return decodeURIComponent(raw);
  } catch {
    throw new HttpError(400, `bad_${what}`, `invalid ${what}`);
  }
}

function method(req: Request, ...allowed: string[]): void {
  if (!allowed.includes(req.method)) {
    throw new HttpError(405, 'method_not_allowed', `use ${allowed.join(' or ')}`);
  }
}

async function route(req: Request, env: Env, nowMs: number): Promise<Response> {
  const url = new URL(req.url);
  const path = url.pathname.replace(/\/+$/, '');

  if (path === '/v1/sync/health') {
    method(req, 'GET');
    return json({ ok: true, protocol: PROTOCOL, max_save_records: config(env).maxSaveRecords });
  }

  if (path === '/v1/sync/account') {
    method(req, 'DELETE');
    const principal = await authenticate(req, env, nowMs);
    if (req.headers.get('x-rightkit-confirm') !== 'erase-account') {
      throw new HttpError(400, 'confirmation_required', 'send header x-rightkit-confirm: erase-account');
    }
    await admit(env, principal, 1, nowMs, { allowRevoked: true });
    return json({ erased: await eraseAccount(env, principal, nowMs) });
  }

  const m = /^\/v1\/sync\/zones\/([^/]+)(?:\/(changes|save)|\/records\/([^/]+))?$/.exec(path);
  if (!m) throw new HttpError(404, 'not_found', 'unknown route');
  const zone = segment(m[1]!, 'zone');
  if (!ZONE.test(zone)) throw new HttpError(400, 'bad_zone', 'invalid zone');

  const principal: Principal = await authenticate(req, env, nowMs);

  if (m[3] !== undefined) {
    method(req, 'GET');
    await admit(env, principal, 1, nowMs);
    return json(await fetchRecord(env, principal, zone, segment(m[3], 'name')));
  }
  switch (m[2]) {
    case 'changes':
      method(req, 'GET');
      await admit(env, principal, 1, nowMs);
      return json(await fetchChanges(env, principal, zone, url.searchParams.get('since'), url.searchParams.get('limit')));
    case 'save': {
      method(req, 'POST');
      const body = await readJson(req);
      const count = isObject(body) && Array.isArray(body.records) ? Math.min(body.records.length, MAX_BATCH) : 1;
      await admit(env, principal, Math.max(1, count), nowMs);
      return json(await saveRecords(env, principal, zone, body, nowMs));
    }
    default:
      method(req, 'POST');
      await admit(env, principal, 1, nowMs);
      return json(await prepareZone(env, principal, zone, nowMs));
  }
}

export default {
  async fetch(req: Request, env: Env): Promise<Response> {
    try {
      return await route(req, env, Date.now());
    } catch (e) {
      if (e instanceof HttpError) return errorResponse(e);
      // Content-free: log the error class only, never request data.
      console.log(JSON.stringify({ event: 'sync.internal_error', error: e instanceof Error ? e.name : 'unknown' }));
      return errorResponse(new HttpError(500, 'internal', 'internal error'));
    }
  },

  async scheduled(_event: ScheduledController, env: Env, ctx: ExecutionContext): Promise<void> {
    const now = Date.now();
    ctx.waitUntil(
      (async () => {
        // One invocation shares one D1 query budget across housekeeping and compaction.
        const budget = new QueryBudget(config(env).queryBudget);
        await pruneHousekeeping(env, now, budget);
        await compactTombstones(env, now, budget);
      })(),
    );
  },
} satisfies ExportedHandler<Env>;
