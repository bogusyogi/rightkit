#!/usr/bin/env node
// Complete-bundle qualification before any download (rightkit#781, #827).
//
// A candidate model qualifies for the compact experiments only when its COMPLETE bundle fits the byte
// cap (700,000,000, target below 500,000,000): every file the runtime needs, not the headline weights.
// This tool reads a bundle manifest and answers that from sizes alone, so nothing oversized is downloaded
// to find out. Package bytes are not peak RAM; resident memory is measured separately on the target machine.
//
//   node bundle-qualify.mjs --manifest <bundle.json> [--kind tts|router] [--cap N] [--target N]
//   node bundle-qualify.mjs --hf <owner/repo> --revision <40-hex sha> [--kind ...] [--runtime <json>]
//
// --hf reads only the repository tree metadata (file names and sizes) from the Hugging Face API; it never
// fetches a model file. The manifest shape:
//   { "model": "...", "revision": "<40 hex>", "license": "<spdx or text>", "source": "<url>",
//     "files": [{ "path": "...", "bytes": N, "role": "weights|tokenizer|vocoder|speaker-assets|reader|port|engine|stats|runtime|config|other" }],
//     "runtimeDeps": [{ "name": "...", "bytes": N, "role": "runtime" }] }
// Exit: 0 qualifies, 1 does not qualify, 2 usage or unreadable input.

import { readFile } from 'node:fs/promises';

export const DEFAULT_CAP = 700_000_000;
export const DEFAULT_TARGET = 500_000_000;

// Roles a complete bundle must name. A bundle that lists no file for a required role is incomplete,
// whatever its total: an incomplete bundle cannot have its size trusted.
export const REQUIRED_ROLES = Object.freeze({
  tts: ['weights', 'tokenizer', 'vocoder', 'speaker-assets', 'runtime'],
  router: ['reader', 'port', 'engine', 'stats', 'runtime'],
});

const REVISION = /^[0-9a-f]{40}$/;

function roleOf(file) {
  if (typeof file.role === 'string' && file.role) return file.role;
  const p = file.path.toLowerCase();
  if (/\.(safetensors|gguf|onnx|bin|pt|pth|npz|mlmodel|mlpackage)$/.test(p) && !/vocoder|codec|audio.?tokenizer|speaker|voice/.test(p)) return 'weights';
  if (/tokenizer|vocab|merges|\.model$|spiece/.test(p)) return 'tokenizer';
  if (/vocoder|codec|audio.?tokenizer|bigvgan|hifigan/.test(p)) return 'vocoder';
  if (/speaker|voice|prompt.*\.(wav|npy|pt)$|\.wav$/.test(p)) return 'speaker-assets';
  if (/config|generation|preprocessor|\.json$/.test(p)) return 'config';
  return 'other';
}

/** Pure qualification of an in-memory manifest. */
export function qualify(manifest, { kind = 'tts', cap = DEFAULT_CAP, target = DEFAULT_TARGET } = {}) {
  const problems = [];
  const files = Array.isArray(manifest.files) ? manifest.files : [];
  const deps = Array.isArray(manifest.runtimeDeps) ? manifest.runtimeDeps : [];
  const items = [];
  for (const f of files) {
    if (typeof f?.path !== 'string' || !Number.isSafeInteger(f.bytes) || f.bytes < 0) {
      problems.push(`file entry without a path and a byte count: ${JSON.stringify(f)}`);
      continue;
    }
    items.push({ name: f.path, bytes: f.bytes, role: roleOf(f), from: 'file' });
  }
  for (const d of deps) {
    if (typeof d?.name !== 'string' || !Number.isSafeInteger(d.bytes) || d.bytes < 0) {
      problems.push(`runtime dependency without a name and a byte count: ${JSON.stringify(d)}`);
      continue;
    }
    items.push({ name: d.name, bytes: d.bytes, role: d.role ?? 'runtime', from: 'runtimeDep' });
  }
  if (items.length === 0) problems.push('the manifest lists no file');
  if (!REVISION.test(manifest.revision ?? '')) problems.push('revision is not a pinned 40-hex commit');
  if (typeof manifest.license !== 'string' || manifest.license.trim() === '') problems.push('license is not recorded');
  if (typeof manifest.source !== 'string' || manifest.source.trim() === '') problems.push('source (provenance) is not recorded');
  const required = REQUIRED_ROLES[kind];
  if (!required) throw new Error(`unknown kind ${kind}`);
  const present = new Set(items.map((i) => i.role));
  const missingRoles = required.filter((r) => !present.has(r));
  for (const r of missingRoles) problems.push(`no file for required role \`${r}\`: the bundle is incomplete`);
  const totalBytes = items.reduce((n, i) => n + i.bytes, 0);
  const byRole = {};
  for (const i of items) byRole[i.role] = (byRole[i.role] ?? 0) + i.bytes;
  const largest = [...items].sort((a, b) => b.bytes - a.bytes).slice(0, 5).map(({ name, bytes, role }) => ({ name, bytes, role }));
  const withinCap = totalBytes <= cap;
  const complete = missingRoles.length === 0;
  const verdict = problems.length === 0 && withinCap ? (totalBytes <= target ? 'qualifies' : 'qualifies-above-target') : 'does-not-qualify';
  if (!withinCap) problems.push(`complete bundle is ${totalBytes} bytes, over the ${cap} byte cap by ${totalBytes - cap}`);
  return { model: manifest.model ?? null, revision: manifest.revision ?? null, kind, cap, target, totalBytes, withinCap, complete, byRole, largest, missingRoles, problems, verdict };
}

async function hfManifest(repo, revision, runtime) {
  if (!/^[\w.-]+\/[\w.-]+$/.test(repo)) throw new Error('--hf expects owner/repo');
  if (!REVISION.test(revision ?? '')) throw new Error('--revision must be a pinned 40-hex commit');
  const base = `https://huggingface.co/api/models/${repo}`;
  const info = await (await fetch(`${base}/revision/${revision}`)).json();
  const tree = [];
  const walk = async (path) => {
    const url = `${base}/tree/${revision}${path ? `/${path}` : ''}?recursive=true`;
    const res = await fetch(url);
    if (!res.ok) throw new Error(`tree listing failed: HTTP ${res.status}`);
    tree.push(...(await res.json()));
  };
  await walk('');
  const files = tree.filter((e) => e.type === 'file').map((e) => ({ path: e.path, bytes: e.lfs?.size ?? e.size }));
  const license = info?.cardData?.license ?? (info?.tags ?? []).find((t) => t.startsWith('license:'))?.slice(8) ?? '';
  return { model: repo, revision, license, source: `https://huggingface.co/${repo}/tree/${revision}`, files, runtimeDeps: runtime };
}

function parseArgs(argv) {
  const out = {};
  for (let i = 0; i < argv.length; i += 2) {
    const k = argv[i];
    if (!k?.startsWith('--') || argv[i + 1] === undefined) throw new Error(`bad argument ${k}`);
    out[k.slice(2)] = argv[i + 1];
  }
  return out;
}

async function main() {
  let args;
  try { args = parseArgs(process.argv.slice(2)); } catch (e) { process.stderr.write(`${e.message}\n`); return 2; }
  const kind = args.kind ?? 'tts';
  const cap = args.cap ? Number(args.cap) : DEFAULT_CAP;
  const target = args.target ? Number(args.target) : DEFAULT_TARGET;
  if (!Number.isSafeInteger(cap) || !Number.isSafeInteger(target) || !REQUIRED_ROLES[kind]) { process.stderr.write('bad --kind, --cap or --target\n'); return 2; }
  try {
    let manifest;
    if (args.manifest) manifest = JSON.parse(await readFile(args.manifest, 'utf8'));
    else if (args.hf) {
      const runtime = args.runtime ? JSON.parse(await readFile(args.runtime, 'utf8')) : [];
      manifest = await hfManifest(args.hf, args.revision, runtime);
    } else { process.stderr.write('give --manifest <file> or --hf <owner/repo> --revision <sha>\n'); return 2; }
    const result = qualify(manifest, { kind, cap, target });
    process.stdout.write(`${JSON.stringify(result, null, 2)}\n`);
    return result.verdict === 'does-not-qualify' ? 1 : 0;
  } catch (e) {
    process.stderr.write(`${e.message}\n`);
    return 2;
  }
}

if (import.meta.url === `file://${process.argv[1]}`) process.exitCode = await main();
