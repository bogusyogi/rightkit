import type { Obj } from './util.js';

export class TomlError extends Error {
  constructor(
    message: string,
    readonly line: number,
  ) {
    super(`TOML line ${line}: ${message}`);
    this.name = 'TomlError';
  }
}

/**
 * Minimal TOML reader for the journeys manifest and map: comments, tables, arrays of tables, basic and literal
 * strings, numbers, booleans, arrays and inline tables. Dates and multi-line strings are refused, never guessed.
 */
export function parseToml(text: string): Obj {
  let pos = 0;
  const src = text.replace(/^﻿/, '');
  const fail = (m: string): never => {
    throw new TomlError(m, src.slice(0, pos).split('\n').length);
  };
  const root: Obj = {};
  let current: Obj = root;

  const skipInline = (): void => {
    while (pos < src.length && (src[pos] === ' ' || src[pos] === '\t')) pos++;
  };
  const skipComment = (): void => {
    if (src[pos] === '#') while (pos < src.length && src[pos] !== '\n') pos++;
  };
  const skipBlank = (): void => {
    for (;;) {
      skipInline();
      skipComment();
      if (src[pos] === '\r' || src[pos] === '\n') pos++;
      else return;
    }
  };

  function readBasic(): string {
    if (src.startsWith('"""', pos)) return fail('multi-line strings are not supported');
    pos++;
    let out = '';
    while (pos < src.length) {
      const c = src[pos]!;
      if (c === '"') {
        pos++;
        return out;
      }
      if (c === '\n') return fail('unterminated string');
      if (c === '\\') {
        const n = src[pos + 1] ?? '';
        const map: Record<string, string> = { n: '\n', t: '\t', r: '\r', '"': '"', '\\': '\\', b: '\b', f: '\f' };
        if (n in map) {
          out += map[n];
          pos += 2;
        } else if (n === 'u' || n === 'U') {
          const len = n === 'u' ? 4 : 8;
          const hex = src.slice(pos + 2, pos + 2 + len);
          if (!/^[0-9a-fA-F]+$/.test(hex) || hex.length !== len) return fail('bad unicode escape');
          out += String.fromCodePoint(parseInt(hex, 16));
          pos += 2 + len;
        } else return fail('bad escape');
      } else {
        out += c;
        pos++;
      }
    }
    return fail('unterminated string');
  }

  function readLiteral(): string {
    if (src.startsWith("'''", pos)) return fail('multi-line strings are not supported');
    const end = src.indexOf("'", pos + 1);
    const nl = src.indexOf('\n', pos);
    if (end < 0 || (nl >= 0 && nl < end)) return fail('unterminated literal string');
    const out = src.slice(pos + 1, end);
    pos = end + 1;
    return out;
  }

  function readKey(): string {
    skipInline();
    const c = src[pos];
    if (c === '"') return readBasic();
    if (c === "'") return readLiteral();
    const m = /^[A-Za-z0-9_-]+/.exec(src.slice(pos));
    if (!m) return fail('expected a key');
    pos += m[0].length;
    return m[0];
  }

  function readKeyPath(): string[] {
    const keys = [readKey()];
    skipInline();
    while (src[pos] === '.') {
      pos++;
      keys.push(readKey());
      skipInline();
    }
    return keys;
  }

  function setPath(table: Obj, keys: string[], value: unknown): void {
    let t = table;
    for (const k of keys.slice(0, -1)) {
      const next = t[k];
      if (next === undefined) t = t[k] = {} as Obj;
      else if (next && typeof next === 'object' && !Array.isArray(next)) t = next as Obj;
      else return fail(`key ${k} is not a table`);
    }
    const last = keys[keys.length - 1]!;
    if (last in t) return fail(`duplicate key ${last}`);
    t[last] = value;
  }

  function readValue(): unknown {
    skipInline();
    const c = src[pos];
    if (c === '"') return readBasic();
    if (c === "'") return readLiteral();
    if (c === '[') {
      pos++;
      const arr: unknown[] = [];
      for (;;) {
        skipBlank();
        if (src[pos] === ']') {
          pos++;
          return arr;
        }
        arr.push(readValue());
        skipBlank();
        if (src[pos] === ',') pos++;
        else if (src[pos] !== ']') return fail('expected , or ] in array');
      }
    }
    if (c === '{') {
      pos++;
      const table: Obj = {};
      for (;;) {
        skipInline();
        if (src[pos] === '}') {
          pos++;
          return table;
        }
        const keys = readKeyPath();
        skipInline();
        if (src[pos] !== '=') return fail('expected =');
        pos++;
        setPath(table, keys, readValue());
        skipInline();
        if (src[pos] === ',') pos++;
        else if (src[pos] !== '}') return fail('expected , or } in inline table');
      }
    }
    const rest = src.slice(pos);
    let m: RegExpExecArray | null;
    if ((m = /^(true|false)(?![A-Za-z0-9_])/.exec(rest))) {
      pos += m[0].length;
      return m[0] === 'true';
    }
    if ((m = /^[+-]?\d[\d_]*(\.\d[\d_]*)?([eE][+-]?\d+)?(?![A-Za-z0-9_:-])/.exec(rest))) {
      pos += m[0].length;
      return Number(m[0].replace(/_/g, ''));
    }
    return fail('unsupported value');
  }

  function descend(keys: string[]): Obj {
    let t = root;
    for (const k of keys) {
      const next = t[k];
      if (next === undefined) t = t[k] = {} as Obj;
      else if (Array.isArray(next)) {
        const tail = next[next.length - 1];
        if (!tail || typeof tail !== 'object') return fail(`cannot extend ${k}`);
        t = tail as Obj;
      } else if (typeof next === 'object' && next) t = next as Obj;
      else return fail(`${k} is not a table`);
    }
    return t;
  }

  for (;;) {
    skipBlank();
    if (pos >= src.length) break;
    if (src.startsWith('[[', pos)) {
      pos += 2;
      const keys = readKeyPath();
      if (!src.startsWith(']]', pos)) fail('expected ]]');
      pos += 2;
      const parent = descend(keys.slice(0, -1));
      const name = keys[keys.length - 1]!;
      const existing = parent[name];
      const entry: Obj = {};
      if (existing === undefined) parent[name] = [entry];
      else if (Array.isArray(existing)) existing.push(entry);
      else fail(`${name} is not an array of tables`);
      current = entry;
    } else if (src[pos] === '[') {
      pos++;
      const keys = readKeyPath();
      if (src[pos] !== ']') fail('expected ]');
      pos++;
      current = descend(keys);
    } else {
      const keys = readKeyPath();
      skipInline();
      if (src[pos] !== '=') fail('expected =');
      pos++;
      setPath(current, keys, readValue());
    }
    skipInline();
    skipComment();
    if (pos < src.length && src[pos] !== '\n' && src[pos] !== '\r') fail('unexpected text after value');
  }
  return root;
}
