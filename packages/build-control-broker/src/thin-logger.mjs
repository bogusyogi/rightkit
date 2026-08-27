import { mkdir, open } from 'node:fs/promises';
import { dirname } from 'node:path';

export class ThinLogger {
  #handle; #pending = Promise.resolve(); #closed = false; #now;

  static async open({ path, now = () => new Date() } = {}) {
    if (typeof path !== 'string' || !path) throw new TypeError('log path is required');
    await mkdir(dirname(path), { recursive: true });
    const handle = await open(path, 'a', 0o600);
    return new ThinLogger({ handle, now });
  }

  constructor({ handle, now }) { this.#handle = handle; this.#now = now; }

  append(event) {
    if (this.#closed) return Promise.reject(new Error('thin logger is closed'));
    if (!event || typeof event !== 'object' || Array.isArray(event) || typeof event.type !== 'string' || !event.type) return Promise.reject(new TypeError('typed log event is required'));
    const value = Object.freeze({ at: this.#timestamp(), ...event });
    this.#pending = this.#pending.then(() => this.#handle.write(`${JSON.stringify(value)}\n`, null, 'utf8')).then(() => value);
    return this.#pending;
  }

  async flush() {
    if (this.#closed) throw new Error('thin logger is closed');
    await this.#pending;
    await this.#handle.sync();
  }

  async close() {
    if (this.#closed) return;
    this.#closed = true;
    await this.#pending;
    await this.#handle.close();
  }

  #timestamp() {
    const value = this.#now();
    return (value instanceof Date ? value : new Date(value)).toISOString();
  }
}
