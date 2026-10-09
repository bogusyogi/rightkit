// Denies shell commands that intrude on the user's live desktop (raise windows,
// capture the screen, synthesize input). Agents must use RightKit hidden QA
// (`rightkit-qa` hidden mode) or `right-ctl` / `rightkit-control` instead.
import { basename } from 'node:path';

const SANCTIONED = new Set(['rightkit-qa', 'right-ctl', 'rightkit-control']);
const WRAPPERS = new Set(['sudo', 'doas', 'env', 'command', 'exec', 'nohup', 'time', 'nice', 'caffeinate', 'builtin', 'arch', 'xcrun', 'stdbuf', 'timeout']);
const WRAPPER_ARG_FLAGS = new Set(['-u', '-g', '-h', '-p', '-C', '-n', '-s', '-k']);
const SHELLS = new Set(['bash', 'sh', 'zsh', 'dash', 'ksh', 'fish']);
const ALWAYS = new Set(['cliclick', 'xdotool', 'ydotool']);
const MAC = new Set(['screencapture', 'osascript', 'open']);
const WINDOWS_TEXT = /SetForegroundWindow|SendKeys|CopyFromScreen/i;

export const DESKTOP_INTRUSION_REASON = 'Blocked: this would intrude on Adrian\'s live desktop (raise a window, capture his screen, or synthesize input). Use RightKit hidden mode instead: `rightkit-qa` (hidden mode) for app QA, or `right-ctl` / `rightkit-control` to drive installed apps in the background.';

// Quote-aware split into segments (on ; & | newline, ( ) and `) of word tokens.
// $( ... ) and backtick bodies become their own segments.
function segments(command) {
  const out = []; let words = []; let word = null; let quote = null;
  const endWord = () => { if (word !== null) { words.push(word); word = null; } };
  const endSeg = () => { endWord(); if (words.length) out.push(words); words = []; };
  for (let i = 0; i < command.length; i += 1) {
    const c = command[i];
    if (quote) {
      if (c === quote) quote = null;
      else if (c === '\\' && quote === '"' && i + 1 < command.length) { word += command[i + 1]; i += 1; }
      else word += c;
      continue;
    }
    if (c === '"' || c === "'") { quote = c; word ??= ''; continue; }
    if (c === '\\' && i + 1 < command.length) { word = (word ?? '') + command[i + 1]; i += 1; continue; }
    if (c === ' ' || c === '\t') { endWord(); continue; }
    if (';&|\n()`{}'.includes(c)) { endSeg(); continue; }
    if (c === '$' && command[i + 1] === '(') { endSeg(); i += 1; continue; }
    word = (word ?? '') + c;
  }
  endSeg();
  return out;
}

// Heredoc bodies are data (often scripts), not commands: drop them.
function stripHeredocBodies(command) {
  const lines = command.split('\n'); const kept = []; let end = null; let strip = false;
  for (const line of lines) {
    if (end !== null) { if ((strip ? line.trim() : line) === end) end = null; continue; }
    kept.push(line);
    const m = /<<(-?)\s*(?:'([^']+)'|"([^"]+)"|\\?([A-Za-z_][A-Za-z0-9_]*))/.exec(line);
    if (m && !line.includes('<<<')) { end = m[2] ?? m[3] ?? m[4]; strip = m[1] === '-'; }
  }
  return kept.join('\n');
}

function programName(token, platform) {
  let name = basename(token.replace(/\\/g, '/'));
  if (platform === 'win32') name = name.replace(/\.(exe|cmd|bat|com)$/i, '');
  return name;
}

function classifySegment(words, platform, depth) {
  let i = 0;
  while (i < words.length) {
    const w = words[i];
    if (/^[A-Za-z_][A-Za-z0-9_]*=/.test(w)) { i += 1; continue; }
    const name = programName(w, platform);
    if (WRAPPERS.has(name)) {
      i += 1;
      while (i < words.length && (words[i].startsWith('-') || /^[A-Za-z_][A-Za-z0-9_]*=/.test(words[i]))) {
        i += WRAPPER_ARG_FLAGS.has(words[i]) && name === 'sudo' ? 2 : 1;
      }
      continue;
    }
    break;
  }
  if (i >= words.length) return false;
  const name = programName(words[i], platform);
  const args = words.slice(i + 1);
  if (SANCTIONED.has(name)) return false;
  if (ALWAYS.has(name)) return true;
  if (platform === 'win32') {
    if (name === 'nircmd' || name === 'nircmdc') return true;
    if (['powershell', 'pwsh'].includes(name) && args.some((a) => WINDOWS_TEXT.test(a))) return true;
  }
  if (platform === 'darwin') {
    if (MAC.has(name)) return true;
    if (name === 'notifyutil' && args.some((a) => a === '--post' || /^-[A-Za-z]*p[A-Za-z]*$/.test(a))) return true;
  }
  if (depth < 4) {
    if (SHELLS.has(name)) {
      const c = args.findIndex((a) => /^-[A-Za-z]*c[A-Za-z]*$/.test(a));
      if (c >= 0 && args[c + 1] !== undefined) return desktopIntrusionViolation(args[c + 1], { platform, depth: depth + 1 });
    }
    if (name === 'cmd') { const c = args.findIndex((a) => /^\/[ck]$/i.test(a)); if (c >= 0) return classifySegment(args.slice(c + 1), platform, depth + 1); }
    if (name === 'eval') return desktopIntrusionViolation(args.join(' '), { platform, depth: depth + 1 });
    if (name === 'xargs') return classifySegment(args.filter((a) => !a.startsWith('-')), platform, depth + 1);
  }
  return false;
}

export function desktopIntrusionViolation(command, { platform = process.platform, depth = 0 } = {}) {
  if (typeof command !== 'string' || !command) return null;
  for (const words of segments(stripHeredocBodies(command))) if (classifySegment(words, platform, depth)) return words.join(' ').slice(0, 120);
  return null;
}
