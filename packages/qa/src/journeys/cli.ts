#!/usr/bin/env node
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { census, censusDiff, formatCensus } from './census.js';
import { formatGate, runGate } from './gate.js';
import { systemGit } from './git.js';
import { checkRoot, type Manifest } from './manifest.js';
import { validateReceiptFile, type Receipt } from './receipt.js';
import { JourneysError, runJourneys } from './run.js';
import type { Problem } from './util.js';

const USAGE = `rightkit-qa-journeys <command>
  check [--root DIR]                       validate journeys.toml and journeys-map.toml
  census [--root DIR] [--base REF] [--json]  count tagged/untagged tests and journeys (report only)
  run [--root DIR] [--id J-001]... [--platform P] [--tier T] [--run-root DIR]
  validate-receipt FILE [--root DIR] [--manifest PATH] [--results PATH] [--tree SHA] [--require-clean] [--check-files]
  gate [--root DIR] [--base REF] [--receipts DIR] [--tree SHA]   advisory; always exits 0`;

interface Args {
  positional: string[];
  flags: Map<string, string[]>;
}

const BOOLEAN = new Set(['json', 'require-clean', 'check-files']);

function parseArgs(argv: string[]): Args {
  const out: Args = { positional: [], flags: new Map() };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i]!;
    if (a.startsWith('--')) {
      const name = a.slice(2);
      const value = BOOLEAN.has(name) ? 'true' : argv[++i];
      if (value === undefined) throw new JourneysError(`--${name} needs a value`);
      out.flags.set(name, [...(out.flags.get(name) ?? []), value]);
    } else out.positional.push(a);
  }
  return out;
}

const one = (a: Args, k: string): string | undefined => a.flags.get(k)?.[0];

function printProblems(problems: Problem[]): void {
  for (const p of problems) console.log(`${p.code}${p.id ? ` [${p.id}]` : ''}: ${p.detail}`);
}

/** Only receipts that validate offline reach the gate; the rest are ignored, never trusted. */
function loadReceipts(dir: string, manifestPath: string, manifest: Manifest | null): Receipt[] {
  const found: Receipt[] = [];
  const stack = [dir];
  while (stack.length > 0) {
    const d = stack.pop()!;
    let entries: fs.Dirent[] = [];
    try {
      entries = fs.readdirSync(d, { withFileTypes: true });
    } catch {
      continue;
    }
    for (const e of entries) {
      const full = path.join(d, e.name);
      if (e.isDirectory()) stack.push(full);
      else if (e.name === 'journeys-receipt.json') {
        try {
          if (validateReceiptFile(full, { manifestPath, manifest }).ok) found.push(JSON.parse(fs.readFileSync(full, 'utf8')) as Receipt);
        } catch {
          /* unreadable receipt: ignored */
        }
      }
    }
  }
  return found;
}

export function main(argv: string[]): number {
  const [cmd, ...rest] = argv;
  if (!cmd || cmd === '--help' || cmd === '-h') {
    console.log(USAGE);
    return cmd ? 0 : 2;
  }
  let args: Args;
  try {
    args = parseArgs(rest);
  } catch (e) {
    console.error((e as Error).message);
    return 2;
  }
  const root = path.resolve(one(args, 'root') ?? process.cwd());
  try {
    switch (cmd) {
      case 'check': {
        const r = checkRoot(root);
        printProblems(r.problems);
        console.log(r.problems.length === 0 ? `ok: ${r.manifest?.journeys.length ?? 0} journeys` : `${r.problems.length} problem(s)`);
        return r.problems.length === 0 ? 0 : 1;
      }
      case 'census': {
        const report = census(root);
        const base = one(args, 'base');
        const git = systemGit(root);
        const prefix = git.run(['rev-parse', '--show-prefix'])?.trim() ?? '';
        const diff = base ? censusDiff(root, git, base, prefix) : null;
        console.log(args.flags.has('json') ? JSON.stringify({ report, diff }, null, 2) : formatCensus(report, diff));
        return 0;
      }
      case 'run': {
        const result = runJourneys({
          root,
          ...(args.flags.has('id') ? { ids: args.flags.get('id')! } : {}),
          ...(one(args, 'platform') ? { platform: one(args, 'platform')! } : {}),
          ...(one(args, 'tier') ? { tier: one(args, 'tier')! } : {}),
          ...(one(args, 'run-root') ? { runRoot: one(args, 'run-root')! } : {}),
        });
        printProblems(result.receipt.problems);
        console.log(`verdict: ${result.receipt.verdict}\nreceipt: ${result.receiptPath}`);
        return result.ok ? 0 : 1;
      }
      case 'validate-receipt': {
        const file = args.positional[0];
        if (!file) {
          console.error('validate-receipt needs a receipt file');
          return 2;
        }
        const manifestPath = one(args, 'manifest') ?? path.join(root, 'journeys.toml');
        const loaded = checkRoot(path.dirname(path.resolve(manifestPath)));
        const v = validateReceiptFile(file, {
          manifestPath,
          manifest: loaded.manifest,
          ...(one(args, 'results') ? { resultsPath: one(args, 'results')! } : {}),
          ...(one(args, 'tree') ? { tree: one(args, 'tree')! } : {}),
          ...(args.flags.has('require-clean') ? { requireClean: true } : {}),
          ...(args.flags.has('check-files') ? { checkFiles: true } : {}),
        });
        printProblems(v.problems);
        console.log(`verdict: ${v.verdict}${v.ok ? '' : ' (not accepted)'}`);
        return v.ok ? 0 : 1;
      }
      case 'gate': {
        // Advisory phase: every path below prints findings and exits 0.
        const loaded = checkRoot(root);
        if (!loaded.manifest) {
          printProblems(loaded.problems);
          console.log('gate (advisory): no readable journeys.toml; nothing to check');
          return 0;
        }
        const git = systemGit(root);
        const base = one(args, 'base') ?? 'HEAD';
        const dir = one(args, 'receipts');
        const receipts = dir ? loadReceipts(dir, path.join(root, 'journeys.toml'), loaded.manifest) : [];
        const report = runGate({
          root,
          git,
          base,
          prefix: git.run(['rev-parse', '--show-prefix'])?.trim() ?? '',
          manifest: loaded.manifest,
          map: loaded.map,
          receipts,
          ...(one(args, 'tree') ? { claimedTree: one(args, 'tree')! } : {}),
        });
        console.log(formatGate(report));
        return 0;
      }
      default:
        console.error(`unknown command ${cmd}\n${USAGE}`);
        return 2;
    }
  } catch (e) {
    console.error((e as Error).message);
    if (e instanceof JourneysError) printProblems(e.problems);
    return cmd === 'gate' ? 0 : 1;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === path.resolve(fileURLToPath(import.meta.url))) {
  process.exitCode = main(process.argv.slice(2));
}
