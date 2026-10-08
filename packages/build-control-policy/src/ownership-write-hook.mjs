import { readFileSync } from 'node:fs';
import { extname, isAbsolute, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { sourceNonCode } from './ownership-source.mjs';

// Packaged snapshot keeps installed adapters self-contained. Focused test
// asserts this snapshot matches release catalog before any catalog changes ship.
const catalogPath = fileURLToPath(new URL('./ownership-fingerprints.json', import.meta.url));
const catalog = JSON.parse(readFileSync(catalogPath, 'utf8'));
const fingerprints = catalog.capabilities.flatMap((capability) => capability.fingerprints
  .filter((entry) => entry.type === 'source' && typeof entry.pattern === 'string')
  .map((entry) => ({ ...entry, package: capability.package, regex: new RegExp(entry.pattern, entry.flags ?? '') }))).sort((a, b) => Number(b.tier === 'block') - Number(a.tier === 'block'));
const sourceExtensions = new Set(['.rs', '.ts', '.tsx', '.js', '.mjs', '.cjs', '.jsx', '.swift', '.m', '.mm', '.c', '.cc', '.cpp', '.h', '.py', '.go', '.kt', '.java', '.cs']);

function normalizedPath(value, cwd = process.cwd()) {
  if (typeof value !== 'string') return null;
  const slash = value.replace(/\\/g, '/').replace(/^file:\/\//i, '');
  const absolute = resolve(cwd, slash);
  return absolute.toLowerCase().replace(/\\/g, '/');
}

function isExceptionPath(value, cwd) {
  const normalized = normalizedPath(value, cwd);
  return Boolean(normalized && normalized.split('/').some((part, index, parts) =>
    part === 'ownership-exceptions.json' && parts[index - 1] === '.rightkit'));
}

function isTrustPath(value, cwd) {
  const normalized = normalizedPath(value, cwd);
  return Boolean(normalized && normalized.includes('/orthic labs/rightkit/ownership/trust.json'));
}

function isProtectedPath(value, cwd) { return isExceptionPath(value, cwd) || isTrustPath(value, cwd); }

function writeTargetException(command, cwd) {
  if (typeof command !== 'string') return false;
  // This hook is advisory feedback. Any command naming the protected record is
  // denied; broker/build admission remains authoritative for opaque shell IO.
  if (!/(?:ownership-exceptions\.json|(?:Orthic Labs[\\/]RightKit|RightKit[\\/]ownership)[\\/]trust\.json)/i.test(command)) return false;
  const hasWriteIntent = /(?:>|\btee\b|\b(?:mv|cp|copy|move|del|rm|remove-item|set-content|add-content|out-file|apply_patch|sed\s+-i|perl\s+-i|createWriteStream)\b|\bfs(?:\.promises)?\.(?:writeFile|appendFile|rename|rm|unlink|open)|writeFileSync|writeFile\s*\()/i.test(command);
  return hasWriteIntent && (isProtectedPath(command, cwd) || /\.rightkit[\\/]ownership-exceptions\.json/i.test(command) || /(?:Orthic Labs[\\/]RightKit|RightKit[\\/]ownership)[\\/]trust\.json/i.test(command));
}

function applyEdit(oldText, oldString, newString) {
  if (typeof oldString !== 'string' || typeof newString !== 'string' || oldString.length === 0) return null;
  const first = oldText.indexOf(oldString);
  if (first < 0 || oldText.indexOf(oldString, first + oldString.length) >= 0) return null;
  return oldText.slice(0, first) + newString + oldText.slice(first + oldString.length);
}

function reconstructClaude(input, current) {
  const tool = input?.tool_name ?? input?.toolName;
  const value = input?.tool_input ?? input?.toolInput ?? {};
  if (tool === 'Write' && typeof value.content === 'string') return value.content;
  if (tool === 'Edit') return applyEdit(current, value.old_string ?? value.oldString, value.new_string ?? value.newString);
  if (tool === 'MultiEdit' && Array.isArray(value.edits)) {
    let result = current;
    for (const edit of value.edits) {
      result = applyEdit(result, edit.old_string ?? edit.oldString, edit.new_string ?? edit.newString);
      if (result === null) return null;
    }
    return result;
  }
  return null;
}

function patchFiles(patch) {
  if (typeof patch !== 'string' || !patch.includes('*** Begin Patch')) return null;
  const files = []; let active = null; let hunk = false;
  for (const line of patch.replace(/\r/g, '').split('\n')) {
    const header = /^\*\*\* (Update|Add|Delete) File: (.+)$/.exec(line);
    if (header) {
      if (active) files.push(active);
    active = { kind: header[1], path: header[2], hunks: [], added: [] }; hunk = false;
      continue;
    }
    const move = /^\*\*\* Move to: (.+)$/.exec(line);
    if (move && active) { active.path = `${active.path}\n${move[1]}`; continue; }
    if (!active || /^\*\*\* (?:Begin|End) Patch$/.test(line) || /^\*\*\* End of File$/.test(line)) continue;
    if (line.startsWith('@@')) {
      hunk = true;
      active.hunks.push({ anchor: line.slice(2).replace(/^@+\s*/, '').trim(), lines: [] });
      continue;
    }
    if (active.kind === 'Add') {
      if (line.startsWith('+')) active.added.push(line.slice(1));
      continue;
    }
    if (hunk && /^[ +\\-]/.test(line)) active.hunks.at(-1)?.lines.push(line);
  }
  if (active) files.push(active);
  return files.length ? files : null;
}

function reconstructPatch(current, file) {
  if (file.kind !== 'Update') {
    if (file.kind === 'Delete') return '';
    return file.added.join('\n');
  }
  const output = current.replace(/\r/g, '').split('\n'); let cursor = 0;
  for (const hunk of file.hunks) {
    if (hunk.anchor) {
      const anchorIndex = output.findIndex((line, index) => index >= cursor && line.includes(hunk.anchor));
      if (anchorIndex < 0) return null;
      cursor = anchorIndex + 1;
    } else if (hunk.lines[0]?.[0] !== '+') {
      const firstOldLine = hunk.lines.find((line) => line[0] === ' ' || line[0] === '-')?.slice(1);
      if (firstOldLine === undefined) return null;
      const firstIndex = output.findIndex((line, index) => index >= cursor && line === firstOldLine);
      if (firstIndex < 0) return null;
      cursor = firstIndex;
    } else return null;
    for (const line of hunk.lines) {
      const marker = line[0]; const text = line.slice(1);
      if (marker === ' ') { if (output[cursor] !== text) return null; cursor += 1; }
      else if (marker === '-') { if (output[cursor] !== text) return null; output.splice(cursor, 1); }
      else if (marker === '+') { output.splice(cursor, 0, text); cursor += 1; }
      else if (marker !== '\\') return null;
    }
  }
  return file.hunks.length ? output.join('\n') : null;
}

function stripComments(text, extension) {
  const hashComments = ['.py', '.ps1', '.rb', '.sh'].includes(extension);
  let result = ''; let quote = ''; let escaped = false; let lineComment = false; let blockComment = false;
  for (let i = 0; i < text.length; i += 1) {
    const ch = text[i]; const next = text[i + 1];
    if (lineComment) { if (ch === '\n') { lineComment = false; result += '\n'; } else result += ' '; continue; }
    if (blockComment) { if (ch === '*' && next === '/') { result += '  '; i += 1; blockComment = false; } else result += ch === '\n' ? '\n' : ' '; continue; }
    if (quote) { result += ch; if (escaped) escaped = false; else if (ch === '\\') escaped = true; else if (ch === quote) quote = ''; continue; }
    if (ch === '"' || ch === "'" || ch === '`') { quote = ch; result += ch; continue; }
    if (ch === '/' && next === '/') { result += '  '; i += 1; lineComment = true; continue; }
    if (ch === '/' && next === '*') { result += '  '; i += 1; blockComment = true; continue; }
    if (hashComments && ch === '#') { result += ' '; lineComment = true; continue; }
    result += ch;
  }
  return result;
}

function scannedText(filePath, text, cwd) {
  const extension = extname(resolve(cwd, filePath)).toLowerCase();
  return sourceExtensions.has(extension) ? stripComments(text, extension) : '';
}

function hasFingerprintIncrease(beforePath, before, after, cwd) {
  const oldText = scannedText(beforePath, before, cwd);
  const newText = scannedText(beforePath, after, cwd);
  return fingerprints.find((entry) => occurrenceCount(newText, entry, beforePath) > occurrenceCount(oldText, entry, beforePath));
}

function fallbackPatchDecision(patch, cwd) {
  if (typeof patch !== 'string') return null;
  const paths = [...patch.matchAll(/^\*\*\* (?:Update|Add|Delete) File: (.+)$/gm)].map((match) => match[1]);
  if (paths.some((path) => isProtectedPath(path, cwd))) return denial('Agent changes to ownership exception or protected trust records are blocked.');
  const added = patch.split(/\r?\n/).filter((line) => line.startsWith('+') && !line.startsWith('+++')).map((line) => line.slice(1)).join('\n');
  if (paths.some((path) => sourceExtensions.has(extname(resolve(cwd, path)).toLowerCase()))
    && paths.some((file) => fingerprints.some((entry) => occurrenceCount(added, entry, file)))) {
    const match = fingerprints.find((entry) => paths.some((file) => occurrenceCount(added, entry, file)));
    return fingerprintDecision(match, 'Could not safely reconstruct patch containing a RightKit source fingerprint.');
  }
  return null;
}

function occurrenceCount(text, entry, filePath) {
  const extension = extname(filePath).toLowerCase();
  if (entry.extensions && !entry.extensions.includes(extension)) return 0;
  const { regex } = entry;
  const flags = regex.flags.includes('g') ? regex.flags : `${regex.flags}g`;
  const matches = [...text.matchAll(new RegExp(regex.source, flags))];
  if (!entry.codeOnly) return matches.length;
  const nonCode = sourceNonCode(text, extension === '.rs');
  return matches.filter((match) => !nonCode[match.index]).length;
}

function denial(reason) {
  return { hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'deny', permissionDecisionReason: `[HOOK:rightkit-ownership:block]\nWhy: ${reason}\nRequired: use the published RightKit capability; exception records require separate owner approval.` } };
}

export function fingerprintDecision(entry, reason) {
  if (entry.tier === 'block') return denial(reason);
  return { decision: 'allow', hookSpecificOutput: { hookEventName: 'PreToolUse', permissionDecision: 'allow', additionalContext: `[HOOK:rightkit-ownership:advise] ${reason} Usage context & replacement qualification require review.` } };
}

export function ownershipWriteDecision(input, { readFile = readFileSync, cwd = input?.cwd ?? process.cwd() } = {}) {
  const tool = input?.tool_name ?? input?.toolName ?? '';
  const data = input?.tool_input ?? input?.toolInput ?? {};
  const command = tool === 'exec_command' ? data.cmd ?? input?.cmd : data.command ?? data.cmd ?? data.code ?? data.input ?? input?.command ?? input?.cmd ?? input?.code;
  if (writeTargetException(command, cwd)) return denial('Agent changes to ownership exception or protected trust records are blocked.');
  const path = data.file_path ?? data.filePath ?? data.path ?? data.file;
  const patch = data.patch ?? data.input ?? input?.patch ?? (tool === 'apply_patch' ? data.code : undefined);
  if (tool === 'apply_patch') {
    const files = patchFiles(patch);
    if (!files) return fallbackPatchDecision(patch, cwd) ?? { decision: 'allow' };
    for (const file of files) {
      const [oldPath, newPath] = file.path.split('\n');
      if (isProtectedPath(oldPath, cwd) || (newPath && isProtectedPath(newPath, cwd))) return denial('Agent changes to ownership exception or protected trust records are blocked.');
      const absolute = resolve(cwd, oldPath); let current = '';
      try { current = readFile(absolute, 'utf8'); } catch (error) { if (error?.code !== 'ENOENT') return { decision: 'allow' }; }
      const post = reconstructPatch(current, file);
      if (post === null) {
        const addedText = file.kind === 'Add' ? file.added.join('\n') : file.hunks.flatMap((hunk) => hunk.lines.filter((line) => line.startsWith('+')).map((line) => line.slice(1))).join('\n');
        const likelySource = sourceExtensions.has(extname(resolve(cwd, oldPath)).toLowerCase());
        const addedMatch = likelySource && fingerprints.find((entry) => occurrenceCount(addedText, entry, oldPath));
        if (addedMatch) return fingerprintDecision(addedMatch, 'Could not reconstruct patch adding a RightKit source fingerprint.');
        continue;
      }
      const match = hasFingerprintIncrease(oldPath, current, post, cwd);
      if (match) {
        return fingerprintDecision(match, `Post-edit content adds RightKit source fingerprint ${match.id} (${match.package}).`);
      }
    }
    return { decision: 'allow' };
  }
  if (!['Write', 'Edit', 'MultiEdit'].includes(tool) || typeof path !== 'string') return { decision: 'allow' };
  if (isProtectedPath(path, cwd)) return denial('Agent changes to ownership exception or protected trust records are blocked.');
  const absolute = isAbsolute(path) ? path : resolve(cwd, path);
  let current = '';
  try { current = readFile(absolute, 'utf8'); } catch (error) { if (error?.code !== 'ENOENT') return { decision: 'allow' }; }
  const post = reconstructClaude(input, current);
  if (post === null) return { decision: 'allow' };
  const match = hasFingerprintIncrease(path, current, post, cwd);
  return match ? fingerprintDecision(match, `Post-edit content adds RightKit source fingerprint ${match.id} (${match.package}).`) : { decision: 'allow' };
}

export function handleOwnershipWriteHook(input) { return ownershipWriteDecision(input); }
