import { createHash } from 'node:crypto';
import fs from 'node:fs';

export type Obj = Record<string, unknown>;

export interface Problem {
  code: string;
  id: string | null;
  detail: string;
}

export const isObj = (value: unknown): value is Obj =>
  typeof value === 'object' && value !== null && !Array.isArray(value);
export const isStr = (value: unknown): value is string => typeof value === 'string' && value.length > 0;
export const isInt = (value: unknown): value is number =>
  typeof value === 'number' && Number.isInteger(value) && value >= 0;
export const isHex = (value: unknown, len: number): value is string =>
  typeof value === 'string' && new RegExp(`^[0-9a-f]{${len}}$`).test(value);
export const isSha = (value: unknown): value is string => isHex(value, 64);

export function sha256Text(text: string | Buffer): string {
  return createHash('sha256').update(text).digest('hex');
}

export function sha256File(file: string): string | null {
  try {
    return sha256Text(fs.readFileSync(file));
  } catch {
    return null;
  }
}

/** Canonical JSON: object keys sorted, no whitespace. */
export function canonicalJson(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(',')}]`;
  if (isObj(value)) {
    return `{${Object.keys(value)
      .sort()
      .filter((k) => value[k] !== undefined)
      .map((k) => `${JSON.stringify(k)}:${canonicalJson(value[k])}`)
      .join(',')}}`;
  }
  return JSON.stringify(value ?? null);
}

/** Glob from the app root: `**` crosses directories, `*` and `?` do not. */
export function globToRegExp(glob: string): RegExp {
  let out = '';
  for (let i = 0; i < glob.length; i++) {
    const c = glob[i]!;
    if (c === '*') {
      if (glob[i + 1] === '*') {
        i++;
        if (glob[i + 1] === '/') {
          i++;
          out += '(?:.*/)?';
        } else out += '.*';
      } else out += '[^/]*';
    } else if (c === '?') out += '[^/]';
    else out += c.replace(/[.+^${}()|[\]\\]/g, '\\$&');
  }
  return new RegExp(`^${out}$`);
}

export const normPath = (p: string): string => p.replace(/\\/g, '/').replace(/^\.\//, '');

export function matchesAny(globs: readonly string[], file: string): boolean {
  const f = normPath(file);
  return globs.some((g) => globToRegExp(normPath(g)).test(f));
}

export const problem = (code: string, id: string | null, detail: string): Problem => ({ code, id, detail });
