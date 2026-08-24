// Wire format & validation firewall for dual-host Rust routing
// (docs/RIGHTKIT-DUAL-HOST-BUILD-ROUTING.md). The SSH payload is base64url
// JSON with a strict schema; no user string ever becomes shell syntax because
// every transmitted field is validated here before use.

import { createHash, randomBytes } from 'node:crypto';

export const REMOTE_PROTOCOL_VERSION = 1;
export const REMOTE_OPERATIONS = Object.freeze(['SUBMIT', 'STATUS', 'CANCEL']);
export const REMOTE_VERBS = Object.freeze(['check', 'clippy', 'test', 'build']);
export const REMOTE_STATES = Object.freeze(['PREPARING', 'QUEUED', 'RUNNING', 'TERMINAL', 'LOST', 'FAILED', 'CANCELLED', 'DEADLINE']);
export const TERMINAL_REMOTE_STATES = Object.freeze(['TERMINAL', 'LOST', 'FAILED', 'CANCELLED', 'DEADLINE']);
export const DEFAULT_REMOTE_DEADLINE_MS = 4 * 60 * 60 * 1000;
export const MIN_REMOTE_DEADLINE_MS = 60_000;
export const MAX_REMOTE_DEADLINE_MS = 24 * 60 * 60 * 1000;
const MAX_ARGV_ENTRIES = 64;
const MAX_ARGV_LENGTH = 4_096;
const HEX_256 = /^[a-f0-9]{64}$/;
const REQUEST_ID = /^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$/;
const REPOSITORY_KEY = /^[a-z0-9][a-z0-9._-]{0,63}$/;

export class RemoteProtocolError extends Error {
  constructor(message, { code = 'REMOTE_PROTOCOL_VIOLATION' } = {}) {
    super(message);
    this.name = 'RemoteProtocolError';
    this.code = code;
  }
}

/** Source-mutating or broker-authority-stealing tokens are denied even though
 * the local broker policy would classify some of them: the remote replica must
 * stay byte-stable for receipt verification, so validation is read-only verbs
 * plus an explicit denylist, enforced worker-side before broker submission. */
const DENIED_FLAGS_EXACT = new Set([
  '--fix', '-C', '-Z', '--config', '--target-dir', '--manifest-path', '--offline', '--locked',
]);
const DENIED_FLAG_PREFIXES = ['--fix=', '--config=', '--target-dir=', '--manifest-path=', '-Z'];
const DENIED_SUBCOMMANDS = new Set([
  'fix', 'fmt', 'update', 'add', 'remove', 'vendor', 'generate-lockfile', 'clean', 'run',
  'install', 'publish', 'package', 'login', 'logout', 'owner', 'yank', 'uninstall', 'init', 'new',
]);

/** Validate remote Cargo argv: registered read-only verbs only, no
 * source-mutating flags, no redirection of tool/target/config authority. */
export function validateRemoteCargoArgv(argv) {
  if (!Array.isArray(argv) || argv.length < 1 || argv.length > MAX_ARGV_ENTRIES) throw new RemoteProtocolError(`remote cargo argv must contain 1-${MAX_ARGV_ENTRIES} entries`);
  for (const value of argv) {
    if (typeof value !== 'string' || value.length > MAX_ARGV_LENGTH) throw new RemoteProtocolError('remote cargo argv entries must be bounded strings');
  }
  if (!REMOTE_VERBS.includes(argv[0])) throw new RemoteProtocolError(`unregistered remote cargo verb: ${argv[0]}`, { code: 'REMOTE_COMMAND_DENIED' });
  let afterSeparator = false;
  for (const value of argv.slice(1)) {
    if (value === '--') { afterSeparator = true; continue; }
    if (afterSeparator) continue; // test-binary arguments are passed through as opaque argv strings
    if (!value.startsWith('-')) continue; // package selectors & test filters
    if (DENIED_FLAGS_EXACT.has(value)) throw new RemoteProtocolError(`denied remote cargo flag: ${value}`, { code: 'REMOTE_COMMAND_DENIED' });
    if (DENIED_FLAG_PREFIXES.some((prefix) => value.startsWith(prefix))) throw new RemoteProtocolError(`denied remote cargo flag: ${value}`, { code: 'REMOTE_COMMAND_DENIED' });
  }
  return Object.freeze([...argv]);
}

function boundedString(value, label, pattern = null) {
  if (typeof value !== 'string' || !value) throw new RemoteProtocolError(`${label} must be a non-empty string`);
  if (pattern && !pattern.test(value)) throw new RemoteProtocolError(`${label} is invalid`);
  return value;
}

function deadline(value) {
  const ms = value ?? DEFAULT_REMOTE_DEADLINE_MS;
  if (!Number.isSafeInteger(ms) || ms < MIN_REMOTE_DEADLINE_MS || ms > MAX_REMOTE_DEADLINE_MS) throw new RemoteProtocolError(`deadlineMs must be ${MIN_REMOTE_DEADLINE_MS}-${MAX_REMOTE_DEADLINE_MS}`);
  return ms;
}

/** Decode & fully validate one base64url-encoded operation payload.
 * SUBMIT carries the complete snapshot description; STATUS/CANCEL carry only
 * the durable request identity. */
export function decodePayload(encoded) {
  if (typeof encoded !== 'string' || !encoded || encoded.length > 16_384) throw new RemoteProtocolError('payload must be a bounded base64url string');
  if (!/^[A-Za-z0-9_-]+$/.test(encoded)) throw new RemoteProtocolError('payload must be base64url without padding');
  let parsed;
  try { parsed = JSON.parse(Buffer.from(encoded, 'base64url').toString('utf8')); } catch { throw new RemoteProtocolError('payload is not valid JSON'); }
  if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) throw new RemoteProtocolError('payload must be a JSON object');
  if (parsed.v !== REMOTE_PROTOCOL_VERSION) throw new RemoteProtocolError(`payload version must be ${REMOTE_PROTOCOL_VERSION}`);
  if (!REMOTE_OPERATIONS.includes(parsed.op)) throw new RemoteProtocolError('payload op must be SUBMIT, STATUS, or CANCEL');
  const allowed = parsed.op === 'SUBMIT'
    ? ['v', 'op', 'requestId', 'repositoryKey', 'digest', 'bytes', 'argv', 'deadlineMs', 'createdAt']
    : ['v', 'op', 'requestId'];
  for (const key of Object.keys(parsed)) if (!allowed.includes(key)) throw new RemoteProtocolError(`unknown payload field for ${parsed.op}: ${key}`);
  const requestId = boundedString(parsed.requestId, 'payload.requestId', REQUEST_ID);
  if (parsed.op !== 'SUBMIT') return Object.freeze({ v: REMOTE_PROTOCOL_VERSION, op: parsed.op, requestId });
  const repositoryKey = boundedString(parsed.repositoryKey, 'payload.repositoryKey', REPOSITORY_KEY);
  const digest = boundedString(parsed.digest, 'payload.digest', HEX_256).toLowerCase();
  if (!Number.isSafeInteger(parsed.bytes) || parsed.bytes < 1 || parsed.bytes > 8 * 1024 ** 3) throw new RemoteProtocolError('payload.bytes must be a positive safe integer within 8 GiB');
  const argv = validateRemoteCargoArgv(parsed.argv);
  return Object.freeze({ v: REMOTE_PROTOCOL_VERSION, op: parsed.op, requestId, repositoryKey, digest, bytes: parsed.bytes, argv, deadlineMs: deadline(parsed.deadlineMs), createdAt: boundedString(parsed.createdAt, 'payload.createdAt') });
}

/** Encode one operation payload for transport over the ssh bridge. The exact
 * transmitted bytes are validated before emission. */
export function encodePayload(payload) {
  const jsonText = JSON.stringify(payload);
  const validated = decodePayload(Buffer.from(jsonText, 'utf8').toString('base64url'));
  return Buffer.from(JSON.stringify(validated), 'utf8').toString('base64url');
}

export function generateRemoteRequestId(now = Date.now) {
  return `rr-${now()}-${randomBytes(8).toString('hex')}`;
}

export function sha256Hex(buffer) { return createHash('sha256').update(buffer).digest('hex'); }

export function isTerminalRemoteState(state) { return TERMINAL_REMOTE_STATES.includes(state); }
