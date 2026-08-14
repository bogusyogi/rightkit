import { mkdir as defaultMkdir, open as defaultOpen, readFile as defaultReadFile, rename as defaultRename } from 'node:fs/promises';
import { dirname } from 'node:path';

const encoder = new TextEncoder();

function positive(value, label) {
  if (!Number.isSafeInteger(value) || value < 1) throw new TypeError(`${label} must be a positive safe integer`);
  return value;
}

function eventLine(event, now) {
  if (!event || typeof event !== 'object' || Array.isArray(event) || typeof event.type !== 'string' || !event.type) throw new TypeError('event requires a non-empty type');
  const value = { ...event, at: event.at ?? new Date(now()).toISOString() };
  if (typeof value.at !== 'string') throw new TypeError('event at must be an ISO string');
  const line = `${JSON.stringify(value)}\n`;
  return { event: Object.freeze(value), line, bytes: encoder.encode(line).byteLength };
}

/** A typed error: write may be visible, but durability was not proven. */
export class EventLogDurabilityError extends Error {
  constructor({ event, stage, cause }) {
    super(`event log durability was not confirmed at ${stage}`, { cause });
    this.name = 'EventLogDurabilityError';
    this.code = 'RK_EVENT_LOG_DURABILITY_UNCONFIRMED';
    this.event = event;
    this.stage = stage;
    this.persisted = 'unknown';
  }
}

function durabilityError(event, stage, cause) {
  return cause instanceof EventLogDurabilityError ? cause : new EventLogDurabilityError({ event, stage, cause });
}

/** A serial append-only NDJSON log with bounded retained tail & explicit durability. */
export class BoundedEventLog {
  #path; #maxBytes; #now; #io; #tail = ''; #tailBytes = 0; #loaded = false; #writing = Promise.resolve();
  constructor({ path, maxBytes = 1_048_576, now = Date.now, io = {} } = {}) {
    if (typeof path !== 'string' || !path) throw new TypeError('event log path is required');
    this.#path = path; this.#maxBytes = positive(maxBytes, 'maxBytes'); if (typeof now !== 'function') throw new TypeError('now must be a function'); this.#now = now;
    this.#io = { mkdir: defaultMkdir, open: defaultOpen, readFile: defaultReadFile, rename: defaultRename, ...io };
  }
  get path() { return this.#path; }
  get maxBytes() { return this.#maxBytes; }

  /** Compatibility API: resolves to event only after file & containing directory are synced. */
  append(event) { return this.appendResult(event).then(({ event: value }) => value); }

  /** Resolves only when durable; exposes proof for lifecycle callers. */
  appendResult(event) {
    const item = eventLine(event, this.#now);
    const operation = this.#writing.then(() => this.#append(item));
    // A failed write must reject only its own caller, never poison later lifecycle writes.
    this.#writing = operation.catch(() => undefined);
    return operation.then(() => Object.freeze({ event: item.event, durable: true, fileSynced: true, directorySynced: true }));
  }

  async flush() { await this.#writing; }

  async #append(item) {
    try {
      await this.#load(item.event);
      if (item.bytes > this.#maxBytes) throw new RangeError('event exceeds maxBytes');
      const combined = `${this.#tail}${item.line}`;
      if (this.#tailBytes + item.bytes <= this.#maxBytes) {
        await this.#appendFile(item.line, item.event);
        this.#tail = combined; this.#tailBytes += item.bytes;
        return;
      }
      const tail = boundedTail(combined, this.#maxBytes);
      await this.#replaceFile(tail, item.event);
      this.#tail = tail; this.#tailBytes = encoder.encode(tail).byteLength;
    } catch (error) {
      // A write can have reached page cache before its fsync fails. Reload before next event.
      this.#loaded = false; this.#tail = ''; this.#tailBytes = 0;
      throw error;
    }
  }

  async #load(event) {
    if (this.#loaded) return;
    await this.#io.mkdir(dirname(this.#path), { recursive: true });
    const current = await this.#io.readFile(this.#path, 'utf8').catch((error) => error.code === 'ENOENT' ? '' : Promise.reject(error));
    this.#tail = boundedTail(current, this.#maxBytes); this.#tailBytes = encoder.encode(this.#tail).byteLength;
    if (this.#tail !== current) await this.#replaceFile(this.#tail, event);
    this.#loaded = true;
  }

  async #appendFile(line, event) {
    let file;
    try {
      file = await this.#io.open(this.#path, 'a');
      await file.writeFile(line, 'utf8');
      await file.sync();
    } catch (error) {
      throw durabilityError(event, 'file', error);
    } finally {
      if (file) await file.close();
    }
    await this.#syncDirectory(event);
  }

  async #replaceFile(contents, event) {
    const temporary = `${this.#path}.rewrite`;
    let file;
    try {
      file = await this.#io.open(temporary, 'w');
      await file.writeFile(contents, 'utf8');
      await file.sync();
    } catch (error) {
      throw durabilityError(event, 'rewrite-file', error);
    } finally {
      if (file) await file.close();
    }
    try {
      await this.#io.rename(temporary, this.#path);
    } catch (error) {
      throw durabilityError(event, 'rename', error);
    }
    await this.#syncDirectory(event);
  }

  async #syncDirectory(event) {
    let directory;
    try {
      directory = await this.#io.open(dirname(this.#path), 'r');
      await directory.sync();
    } catch (error) {
      throw durabilityError(event, 'directory', error);
    } finally {
      if (directory) await directory.close();
    }
  }
}

function boundedTail(value, maxBytes) {
  if (encoder.encode(value).byteLength <= maxBytes) return value;
  const lines = value.split('\n');
  lines.pop(); // Trailing empty entry or partial record: neither belongs in a retained NDJSON tail.
  while (lines.length && encoder.encode(`${lines.join('\n')}\n`).byteLength > maxBytes) lines.shift();
  return lines.length ? `${lines.join('\n')}\n` : '';
}
