// Archive safety for dual-host Rust routing (docs/RIGHTKIT-DUAL-HOST-BUILD-ROUTING.md
// §3, §9): a `git archive --format=tar` snapshot is enumerated and validated
// member-by-member before anything is written to disk. Absolute, drive-qualified,
// parent-traversal, alternate-stream, reserved-Windows, symlink, hardlink,
// junction/reparse, device, FIFO, and other non-regular/non-directory entries are
// rejected, as are names equal under Windows case-folding or trailing-dot/space
// normalization and Git submodule entries.

const USTAR_HEADER = 512;
const REGULAR = new Set(['0', '\0', '']);
const ACCEPTED_TYPES = new Set([...REGULAR, '5', 'x', 'g']); // regular file, directory, pax extended/global header
const MAX_MEMBERS = 200_000;
const MAX_TOTAL_BYTES = 8 * 1024 ** 3;
const MAX_PATH_BYTES = 4_096;
const RESERVED_WINDOWS = /^(CON|PRN|AUX|NUL|COM[1-9]|LPT[1-9])(\..*)?$/i;

export class UnsafeArchiveError extends Error {
  constructor(message, { code = 'REMOTE_UNSAFE_ARCHIVE_MEMBER' } = {}) {
    super(message);
    this.name = 'UnsafeArchiveError';
    this.code = code;
  }
}

function paxRecords(block) {
  const text = block.toString('utf8');
  const records = new Map();
  let offset = 0;
  while (offset < text.length) {
    const space = text.indexOf(' ', offset);
    if (space < 0) throw new UnsafeArchiveError('malformed pax extended header record');
    const length = Number(text.slice(offset, space));
    if (!Number.isInteger(length) || length <= 0) throw new UnsafeArchiveError('malformed pax extended header length');
    const record = text.slice(offset + space + 1, offset + length - 1); // strip trailing newline
    const equals = record.indexOf('=');
    if (equals < 0) throw new UnsafeArchiveError('malformed pax extended header record');
    records.set(record.slice(0, equals), record.slice(equals + 1));
    offset += length;
  }
  return records;
}

function tarSize(header) {
  const raw = header.subarray(124, 136).toString('ascii').replace(/\0/g, '').trim();
  if (!raw) return 0;
  if (!/^[0-7]+$/.test(raw)) throw new UnsafeArchiveError('malformed tar member size');
  const size = Number.parseInt(raw, 8);
  if (!Number.isSafeInteger(size)) throw new UnsafeArchiveError('tar member size exceeds safe integer range');
  return size;
}

function dataBlocks(size, offset, archiveLength) {
  const blocks = Math.ceil(size / USTAR_HEADER) * USTAR_HEADER;
  if (!Number.isSafeInteger(blocks) || blocks > archiveLength - offset) throw new UnsafeArchiveError('truncated tar member payload');
  return blocks;
}

/** Enumerate one tar archive into [{ path, type, size }] entries. Only ustar
 * plus pax `path` overrides are supported; every other entry type fails closed. */
export function listArchiveMembers(buffer) {
  if (!Buffer.isBuffer(buffer) || buffer.length % USTAR_HEADER !== 0) throw new UnsafeArchiveError('archive must be whole 512-byte tar blocks');
  const members = [];
  let pendingPathOverride = null;
  let offset = 0;
  while (offset < buffer.length) {
    if (buffer.length - offset < USTAR_HEADER) throw new UnsafeArchiveError('truncated tar member header');
    const header = buffer.subarray(offset, offset + USTAR_HEADER);
    if (header.every((byte) => byte === 0)) break; // two zero blocks terminate; one is enough to stop here
    const nameField = header.subarray(0, 100).toString('utf8').replace(/\0.*$/, '');
    const prefixField = header.subarray(345, 500).toString('utf8').replace(/\0.*$/, '');
    const typeflag = header.toString('ascii', 156, 157);
    const size = tarSize(header);
    offset += USTAR_HEADER;
    if (typeflag === 'x' || typeflag === 'g') {
      const block = buffer.subarray(offset, offset + dataBlocks(size, offset, buffer.length));
      const records = paxRecords(block.subarray(0, size));
      if (records.has('linkpath')) throw new UnsafeArchiveError('pax linkpath entries are not accepted');
      // A global PAX header applies metadata to following members; Git emits
      // one for archive provenance. Only per-member headers may override path.
      if (typeflag === 'x' && records.has('path')) pendingPathOverride = records.get('path');
      offset += block.length;
      continue;
    }
    if (!ACCEPTED_TYPES.has(typeflag)) throw new UnsafeArchiveError(`unsupported archive member type '${typeflag}'`, { code: 'REMOTE_UNSUPPORTED_MEMBER_TYPE' });
    const path = `${prefixField ? `${prefixField}/` : ''}${nameField}`;
    members.push({ path: pendingPathOverride ?? path, type: typeflag === '5' ? 'directory' : 'file', size });
    pendingPathOverride = null;
    offset += dataBlocks(size, offset, buffer.length);
    if (members.length > MAX_MEMBERS) throw new UnsafeArchiveError(`archive exceeds ${MAX_MEMBERS} members`);
  }
  return members;
}

function validateMemberPath(path, seenLower) {
  if (typeof path !== 'string' || !path.length || path.length > MAX_PATH_BYTES) throw new UnsafeArchiveError(`invalid archive member path length`);
  if (path.includes('\\')) throw new UnsafeArchiveError(`backslash in archive member path: ${path}`);
  if (path.startsWith('/') || /^[A-Za-z]:/.test(path)) throw new UnsafeArchiveError(`absolute or drive-qualified archive member path: ${path}`);
  if (path.includes(':')) throw new UnsafeArchiveError(`colon in archive member path (alternate stream risk): ${path}`);
  const segments = path.split('/');
  if (segments.some((segment) => segment.length === 0 && segment !== segments[segments.length - 1]) || segments.includes('..') || segments.includes('.')) {
    throw new UnsafeArchiveError(`parent-traversal or empty segment in archive member path: ${path}`);
  }
  for (const segment of segments) {
    if (/[\s.]$/.test(segment)) throw new UnsafeArchiveError(`trailing dot/space in archive member path component: ${path}`);
    if (RESERVED_WINDOWS.test(segment)) throw new UnsafeArchiveError(`reserved Windows name in archive member path: ${path}`);
    if (Buffer.byteLength(segment, 'utf8') > 255) throw new UnsafeArchiveError(`over-long archive member path component: ${path}`);
  }
  // Git submodule entries never appear in a tree snapshot; a checked-in
  // .gitmodules without submodule workspaces would be inert but is still a
  // loud signal the coordinator should have caught. Reject it defensively.
  if (segments[segments.length - 1] === '.gitmodules') throw new UnsafeArchiveError('Git submodule configuration inside archive', { code: 'REMOTE_ARCHIVE_SUBMODULE' });
  const lower = path.toLowerCase();
  if (seenLower.has(lower)) throw new UnsafeArchiveError(`Windows case-folding collision in archive: ${path} vs ${seenLower.get(lower)}`, { code: 'REMOTE_ARCHIVE_CASE_COLLISION' });
  seenLower.set(lower, path);
}

/** Validate an enumerated member list against every documented rejection rule.
 * Returns the total payload byte count on success. */
export function validateArchiveMembers(members) {
  if (!Array.isArray(members) || members.length === 0) throw new UnsafeArchiveError('archive contains no members');
  const seenLower = new Map();
  let totalBytes = 0;
  for (const member of members) {
    validateMemberPath(member.path, seenLower);
    if (member.type === 'file') {
      totalBytes += member.size;
      if (totalBytes > MAX_TOTAL_BYTES) throw new UnsafeArchiveError(`archive expands beyond ${MAX_TOTAL_BYTES} bytes`);
    }
  }
  return totalBytes;
}

/** Convenience: enumerate then validate. */
export function inspectArchive(buffer) {
  const members = listArchiveMembers(buffer);
  const totalBytes = validateArchiveMembers(members);
  return Object.freeze({ members: Object.freeze(members), totalBytes });
}

/** Extract a validated archive into an existing empty destination directory.
 * A single pass walks headers with the same rules as listArchiveMembers,
 * re-validates every final path, and writes only regular files & directories. */
export async function extractArchive(buffer, destination) {
  const { join, mkdir, writeFile } = await import('node:fs/promises');
  const { join: pathJoin, resolve } = await import('node:path');
  const root = resolve(destination);
  const seenLower = new Map();
  await mkdir(root, { recursive: true });
  if (!Buffer.isBuffer(buffer) || buffer.length % USTAR_HEADER !== 0) throw new UnsafeArchiveError('archive must be whole 512-byte tar blocks');
  let pendingPathOverride = null;
  let offset = 0;
  let memberCount = 0;
  let archiveMemberCount = 0;
  while (offset < buffer.length) {
    if (buffer.length - offset < USTAR_HEADER) throw new UnsafeArchiveError('truncated tar member header');
    const header = buffer.subarray(offset, offset + USTAR_HEADER);
    if (header.every((byte) => byte === 0)) break;
    const nameField = header.subarray(0, 100).toString('utf8').replace(/\0.*$/, '');
    const prefixField = header.subarray(345, 500).toString('utf8').replace(/\0.*$/, '');
    const typeflag = header.toString('ascii', 156, 157);
    const size = tarSize(header);
    offset += USTAR_HEADER;
    if (typeflag === 'x' || typeflag === 'g') {
      const blockLength = dataBlocks(size, offset, buffer.length);
      const records = paxRecords(buffer.subarray(offset, offset + size));
      if (records.has('linkpath')) throw new UnsafeArchiveError('pax linkpath entries are not accepted');
      if (typeflag === 'x' && records.has('path')) pendingPathOverride = records.get('path');
      offset += blockLength;
      continue;
    }
    if (!ACCEPTED_TYPES.has(typeflag)) throw new UnsafeArchiveError(`unsupported archive member type '${typeflag}'`, { code: 'REMOTE_UNSUPPORTED_MEMBER_TYPE' });
    const path = pendingPathOverride ?? `${prefixField ? `${prefixField}/` : ''}${nameField}`;
    pendingPathOverride = null;
    validateMemberPath(path, seenLower);
    const target = pathJoin(root, ...path.split('/'));
    if (!resolve(target).startsWith(root)) throw new UnsafeArchiveError(`extraction escaped destination: ${path}`);
    if (typeflag === '5') {
      await mkdir(target, { recursive: true });
    } else {
      const payload = buffer.subarray(offset, offset + size);
      await mkdir(dirname(target), { recursive: true });
      await writeFile(target, payload, { flag: 'wx' });
    }
    // Return count is the number of materialized files. Directory entries are
    // validated & created too, but do not represent extracted payload bytes.
    archiveMemberCount += 1;
    if (typeflag !== '5') memberCount += 1;
    if (archiveMemberCount > MAX_MEMBERS) throw new UnsafeArchiveError(`archive exceeds ${MAX_MEMBERS} members`);
    offset += dataBlocks(size, offset, buffer.length);
  }
  return memberCount;
}

function dirname(path) { const index = path.lastIndexOf('/'); const native = path.lastIndexOf('\\'); return index < 0 && native < 0 ? path : path.slice(0, Math.max(index, native)); }
