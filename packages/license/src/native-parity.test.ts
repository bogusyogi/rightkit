import { readFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { describe, expect, it } from 'vitest';
import { LICENSE_MESSAGES } from './index.js';
import { createLicenseMock } from './testing.js';

// Drift guard. The native crate is the authority for the closed enums and the fixed English
// text (crates/rightkit-license/src/client/view.rs); this package holds a renderer copy of each.
const packageRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const viewRs = readFileSync(path.join(packageRoot, '../../crates/rightkit-license/src/client/view.rs'), 'utf8');

function camel(name: string): string { return name.charAt(0).toLowerCase() + name.slice(1); }
function snakeToCamel(name: string): string { return name.replace(/_(\w)/g, (_, c: string) => c.toUpperCase()); }

function body(keyword: 'enum' | 'struct', name: string): string {
  const start = viewRs.indexOf(`pub ${keyword} ${name} {`);
  if (start < 0) throw new Error(`native ${keyword} ${name} not found`);
  return viewRs.slice(start, viewRs.indexOf('\n}', start));
}

function variants(name: string): string[] {
  return [...body('enum', name).matchAll(/^    ([A-Z]\w*),$/gm)].map(match => camel(match[1]!)).sort();
}

function englishTable(): Map<string, string> {
  const start = viewRs.indexOf('pub fn english(self)');
  const end = viewRs.indexOf('\n    }\n}', start);
  const table = new Map<string, string>();
  for (const match of viewRs.slice(start, end).matchAll(/Self::(\w+) =>\s*(?:\{\s*)?"((?:[^"\\]|\\.)*)"/g)) {
    table.set(camel(match[1]!), match[2]!);
  }
  return table;
}

describe('native DTO parity with rightkit-license', () => {
  it('fixed English text for every message code matches the native table', () => {
    const native = englishTable();
    expect([...native.keys()].sort()).toEqual(Object.keys(LICENSE_MESSAGES).sort());
    for (const [code, text] of Object.entries(LICENSE_MESSAGES)) {
      expect(text, code).toBe(native.get(code));
    }
  });

  it('message code wire values match the native MessageCode enum', () => {
    expect(variants('MessageCode')).toEqual(Object.keys(LICENSE_MESSAGES).sort());
  });

  it('reject reason and command code wire values match the native enums', () => {
    expect(variants('RejectReason')).toEqual(['deviceLimit', 'expired', 'keyNotAccepted', 'other', 'rateLimited', 'revoked']);
    expect(variants('CommandCode')).toEqual(
      ['busy', 'clock', 'forbidden', 'network', 'rejected', 'store', 'unconfigured', 'unresolved', 'unsupported', 'verify'],
    );
  });

  it('projected LicenseView carries exactly the native struct fields', async () => {
    const view = await createLicenseMock().client.status();
    const native = [...body('struct', 'LicenseView').matchAll(/^    pub (\w+):/gm)]
      .map(match => snakeToCamel(match[1]!))
      .sort();
    expect(Object.keys(view).sort()).toEqual(native);
  });
});
