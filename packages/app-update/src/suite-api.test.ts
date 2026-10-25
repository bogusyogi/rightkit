import { describe, expect, it } from 'vitest';
import * as suite from './suite-api.js';
import { RIGHT_SUITE_API_CONTRACT } from './suite-api.generated.js';
import type { UpdaterKey } from './suite-api.js';
import contractJson from '../../../contracts/right-suite-api.v1.json' with { type: 'json' };
import signatureVectors from '../../../contracts/updater-signature-vectors.v1.json' with { type: 'json' };

describe('suite-api contract projection', () => {
  it('matches the vendored contract exactly', () => {
    // The projection carries the contract minus its comment and the legacy hosts, which are served only by the gate.
    const { _comment, legacyHosts, ...projected } = contractJson;
    expect(_comment).toEqual(expect.any(String));
    expect(RIGHT_SUITE_API_CONTRACT).toEqual(projected);
    expect(suite.RIGHT_SUITE_API_HOST).toBe(contractJson.host);
    expect(suite.RIGHT_SUITE_API_ORIGIN).toBe(contractJson.origin);
    expect([...suite.RIGHT_SUITE_LEGACY_HOSTS]).toEqual(contractJson.legacyHosts);
  });
});

describe('suite-api helpers', () => {
  it('returns the exact release status URL', () => {
    expect(suite.releaseStatusUrl('viewright')).toBe('https://api.orthiclabs.com/v1/apps/viewright/releases/status.json');
  });

  it('returns the exact updater endpoint and keeps the Tauri target placeholder', () => {
    expect(suite.updaterEndpoint('mailright')).toBe('https://api.orthiclabs.com/v1/apps/mailright/releases/latest.json?platform={{target}}');
  });

  it('returns per-app artifact hosts as fresh arrays', () => {
    expect(suite.artifactHosts('viewright')).toEqual(['pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev']);
    expect(suite.artifactHosts('mailright')).toEqual([
      'rightapps-license-gate.adrdsouza.workers.dev',
      'pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev',
    ]);
    expect(suite.artifactHosts('scraperight')).toEqual([]);
    expect(suite.artifactHosts('constructor')).toEqual([]);
    const copy = suite.artifactHosts('viewright');
    copy.push('api.spoares.com');
    expect(suite.artifactHosts('viewright')).not.toContain('api.spoares.com');
  });

  it('returns the exact public installer URL and refuses traversal', () => {
    expect(suite.publicInstallerUrl('viewright', 'mac/current/ViewRight_universal.dmg')).toBe(
      'https://pub-6c73208d46c245a9b4881d5e02f6b618.r2.dev/viewright/installers/mac/current/ViewRight_universal.dmg',
    );
    for (const bad of ['../secret', 'mac/../x', '/abs/x', 'mac//x', '', 'mac/current/a b.dmg', 'mac/current/']) {
      expect(() => suite.publicInstallerUrl('viewright', bad)).toThrow(TypeError);
    }
  });

  it('refuses app keys outside the contract key pattern', () => {
    for (const bad of ['', 'View Right', 'ViewRight', '1right', '../viewright', 'viewright/x', 'viewright?x=1', 'view_right', 'viewright.']) {
      expect(() => suite.releaseStatusUrl(bad)).toThrow(TypeError);
      expect(() => suite.updaterEndpoint(bad)).toThrow(TypeError);
      expect(() => suite.artifactHosts(bad)).toThrow(TypeError);
      expect(() => suite.publicInstallerUrl(bad, 'mac/current/x.dmg')).toThrow(TypeError);
    }
  });

  it('does not import Tauri or Node built-ins', async () => {
    const { readFileSync } = await import('node:fs');
    const source = readFileSync(new URL('./suite-api.ts', import.meta.url), 'utf8');
    expect(source).not.toMatch(/@tauri-apps|from ['"]node:|require\(/u);
  });
});

// A release signature as latest.json carries it: base64 of the minisign .sig text. The id bytes are stored little endian.
function fixtureSignature(keyId: string, algorithm = 'D'): string {
  const raw = Buffer.concat([Buffer.from(`E${algorithm}`, 'latin1'), Buffer.from(keyId, 'hex').reverse(), Buffer.alloc(64)]);
  return signatureText(raw.toString('base64'));
}

function signatureText(secondLine: string): string {
  const text = `untrusted comment: signature from tauri secret key\n${secondLine}\ntrusted comment: timestamp:1\n${Buffer.alloc(64).toString('base64')}\n`;
  return Buffer.from(text).toString('base64');
}

const SIGNING_KEY_ID = suite.RIGHT_SUITE_UPDATER_SIGNING_KEY_ID;
// Asymmetric on purpose: read in the wrong byte order it becomes EFCDAB8967452301.
const OTHER_KEY_ID = '0123456789ABCDEF';
const UNLISTED_KEY_ID = 'FEDCBA9876543210';
const PLACEHOLDER_KEY_TEXT = 'placeholder public key text';
// 2027-01-01T00:00:00Z: the first second after the last accepted day, as the contract's date turns into notAfterUnix.
const END_OF_OLD_KEY = 1_798_761_600;

function fixtureKeys(): UpdaterKey[] {
  return [
    { kid: SIGNING_KEY_ID, publicKey: PLACEHOLDER_KEY_TEXT, notAfter: '2026-12-31', notAfterUnix: END_OF_OLD_KEY },
    { kid: OTHER_KEY_ID, publicKey: PLACEHOLDER_KEY_TEXT, notAfter: null, notAfterUnix: null },
  ];
}

describe('suite-api updater keys', () => {
  it('lists the contract keys and signing key id', () => {
    expect(suite.updaterSigningKeyId()).toBe(contractJson.updaterSigningKeyId);
    const listed = contractJson.updaterPublicKeys.map((entry) => ({ ...entry, notAfter: (entry as { notAfter?: string }).notAfter ?? null }));
    expect(suite.updaterKeys().map(({ kid, publicKey, notAfter }) => ({ kid, publicKey, notAfter }))).toEqual(listed);
    // The end second exists exactly when the contract gives an end date.
    expect(suite.updaterKeys().map((key) => key.notAfterUnix === null)).toEqual(listed.map((entry) => entry.notAfter === null));
    // A rotation lists the new key before the signing key id moves; the id must always name a listed key.
    expect(suite.updaterKeys().map((key) => key.kid)).toContain(suite.updaterSigningKeyId());
  });

  it('returns copies the caller cannot widen the contract with', () => {
    const first = suite.updaterKeys() as UpdaterKey[];
    first[0].kid = 'FFFFFFFFFFFFFFFF';
    first[0].notAfterUnix = 1;
    first.push({ kid: OTHER_KEY_ID, publicKey: PLACEHOLDER_KEY_TEXT, notAfter: null, notAfterUnix: null });
    const second = suite.updaterKeys();
    expect(second).toHaveLength(RIGHT_SUITE_API_CONTRACT.updaterPublicKeys.length);
    expect(second[0].kid).toBe(SIGNING_KEY_ID);
    expect(second[0].notAfterUnix).toBe(suite.RIGHT_SUITE_UPDATER_PUBLIC_KEYS[0].notAfterUnix);
    expect(suite.selectUpdaterKey(fixtureSignature(SIGNING_KEY_ID), 0)).toMatchObject({ ok: true });
    expect(suite.selectUpdaterKey(fixtureSignature(OTHER_KEY_ID), 0)).toEqual({ ok: false, reason: 'unknown-key' });
  });

  it('reads the key id a signature names, in either algorithm', () => {
    expect(suite.updaterKeyIdOfSignature(fixtureSignature(SIGNING_KEY_ID, 'd'))).toBe(SIGNING_KEY_ID);
    expect(suite.updaterKeyIdOfSignature(fixtureSignature(OTHER_KEY_ID, 'D'))).toBe(OTHER_KEY_ID);
    expect(suite.updaterKeyIdOfSignature(fixtureSignature(OTHER_KEY_ID, 'd'))).toBe(OTHER_KEY_ID);
    // A .sig file read from disk ends in a newline.
    expect(suite.updaterKeyIdOfSignature(` ${fixtureSignature(OTHER_KEY_ID)}\n`)).toBe(OTHER_KEY_ID);
    // CRLF line ends, as a .sig file written on Windows carries them.
    const crlf = Buffer.from(Buffer.from(fixtureSignature(OTHER_KEY_ID), 'base64').toString().replaceAll('\n', '\r\n')).toString('base64');
    expect(suite.updaterKeyIdOfSignature(crlf)).toBe(OTHER_KEY_ID);
  });

  it('reads anything that is not a minisign signature as null and never throws', () => {
    const good = fixtureSignature(OTHER_KEY_ID);
    const secondLine = (raw: Buffer, encode = (line: string) => line) => signatureText(encode(raw.toString('base64')));
    const header = (algorithm: string) => Buffer.concat([Buffer.from(`E${algorithm}`, 'latin1'), Buffer.from(OTHER_KEY_ID, 'hex').reverse()]);
    const bad: unknown[] = [
      '',
      '   ',
      'not base64!',
      good.slice(0, good.length / 8 * 4),
      `${good.slice(0, 8)}\n${good.slice(8)}`,
      // Padding dropped from the id line: atob reads it, the native host does not.
      secondLine(Buffer.concat([header('D'), Buffer.alloc(64)]), (line) => line.replace(/=+$/u, '')),
      Buffer.from('untrusted comment: only one line\n').toString('base64'),
      Buffer.from('untrusted comment: x\nnot base64!\n').toString('base64'),
      secondLine(Buffer.concat([header('D'), Buffer.alloc(63)])),
      secondLine(Buffer.concat([header('D'), Buffer.alloc(65)])),
      secondLine(Buffer.concat([header('X'), Buffer.alloc(64)])),
      secondLine(Buffer.concat([Buffer.from('Xd', 'latin1'), Buffer.from(OTHER_KEY_ID, 'hex').reverse(), Buffer.alloc(64)])),
      // Not valid UTF-8 once decoded, even though the id line after it is intact.
      Buffer.from([0xff, 0xfe, 0xfd]).toString('base64'),
      Buffer.concat([Buffer.from([0xff, 0x0a]), Buffer.from(`${Buffer.concat([header('D'), Buffer.alloc(64)]).toString('base64')}\n`)]).toString('base64'),
      undefined,
      null,
      42,
      {},
    ];
    for (const value of bad) {
      expect(suite.updaterKeyIdOfSignature(value as string), String(value)).toBeNull();
    }
  });

  it('refuses base64 whose last digit carries bits the native host refuses', () => {
    const line = Buffer.concat([Buffer.from('ED', 'latin1'), Buffer.from(OTHER_KEY_ID, 'hex').reverse(), Buffer.alloc(64)]).toString('base64');
    expect(line.endsWith('A=')).toBe(true);
    // 74 bytes leave two unused bits in the last digit; 'B' sets one. atob reads the same bytes, the canonical text differs.
    const sloppy = `${line.slice(0, -2)}B=`;
    expect(atob(sloppy)).toBe(atob(line));
    expect(suite.updaterKeyIdOfSignature(signatureText(sloppy))).toBeNull();
    expect(suite.updaterKeyIdOfSignature(signatureText(line))).toBe(OTHER_KEY_ID);
  });

  it('selects the key a signature names, by its end date', () => {
    const keys = fixtureKeys();
    const old = fixtureSignature(SIGNING_KEY_ID);
    const rotated = fixtureSignature(OTHER_KEY_ID);
    // Before the old key's end date both releases are accepted, each by its own key.
    expect(suite.selectUpdaterKey(old, END_OF_OLD_KEY - 1, keys)).toEqual({ ok: true, key: keys[0] });
    expect(suite.selectUpdaterKey(rotated, END_OF_OLD_KEY - 1, keys)).toEqual({ ok: true, key: keys[1] });
    // From the end second on the old key is refused; the key without an end date is not.
    expect(suite.selectUpdaterKey(old, END_OF_OLD_KEY, keys)).toEqual({ ok: false, reason: 'key-expired' });
    expect(suite.selectUpdaterKey(old, 5_000_000_000, keys)).toEqual({ ok: false, reason: 'key-expired' });
    expect(suite.selectUpdaterKey(rotated, 5_000_000_000, keys)).toEqual({ ok: true, key: keys[1] });
  });

  it('never lets an unreadable clock extend a key that has an end date', () => {
    const keys = fixtureKeys();
    expect(suite.selectUpdaterKey(fixtureSignature(SIGNING_KEY_ID), undefined, keys)).toEqual({ ok: false, reason: 'key-expired' });
    expect(suite.selectUpdaterKey(fixtureSignature(SIGNING_KEY_ID), Number.NaN, keys)).toEqual({ ok: false, reason: 'key-expired' });
    expect(suite.selectUpdaterKey(fixtureSignature(OTHER_KEY_ID), undefined, keys)).toEqual({ ok: true, key: keys[1] });
  });

  it('refuses a key nothing lists and text that is not a signature', () => {
    const keys = fixtureKeys();
    expect(suite.selectUpdaterKey(fixtureSignature(UNLISTED_KEY_ID), 0, keys)).toEqual({ ok: false, reason: 'unknown-key' });
    expect(suite.selectUpdaterKey(fixtureSignature(SIGNING_KEY_ID), 0, [])).toEqual({ ok: false, reason: 'unknown-key' });
    expect(suite.selectUpdaterKey('garbage', 0, keys)).toEqual({ ok: false, reason: 'malformed' });
    expect(suite.selectUpdaterKey('', undefined, keys)).toEqual({ ok: false, reason: 'malformed' });
  });

  it('accepts a release signed by the contract signing key and refuses a stranger over the contract list', () => {
    const signed = suite.selectUpdaterKey(fixtureSignature(suite.updaterSigningKeyId()), 0);
    expect(signed).toEqual({ ok: true, key: suite.updaterKeys().find((key) => key.kid === suite.updaterSigningKeyId()) });
    expect(suite.selectUpdaterKey(fixtureSignature(UNLISTED_KEY_ID), 0)).toEqual({ ok: false, reason: 'unknown-key' });
  });
});

describe('updater signature key ids against the shared vectors', () => {
  // The same file rightkit-license (Rust) and @rightkit/release (publish tooling) are held to, so the three readers of a
  // signature's key id cannot drift apart.
  it('reads exactly the id each vector names, and refuses what the vectors say to refuse', () => {
    expect(signatureVectors.schema).toBe('updater-signature-key-id.v1');
    expect(signatureVectors.cases.length).toBeGreaterThanOrEqual(15);
    for (const vector of signatureVectors.cases) {
      expect(suite.updaterKeyIdOfSignature(vector.signature), vector.name).toBe(vector.keyId);
    }
  });
});
