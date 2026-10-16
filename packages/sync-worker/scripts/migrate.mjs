// Apply D1 migrations to every shard binding declared in wrangler.toml (DB_0, DB_1, ...).
// Usage: node scripts/migrate.mjs --local | --remote
import { execFileSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const mode = process.argv[2];
if (mode !== '--local' && mode !== '--remote') {
  console.error('usage: node scripts/migrate.mjs --local | --remote');
  process.exit(2);
}

const toml = readFileSync(fileURLToPath(new URL('../wrangler.toml', import.meta.url)), 'utf8');
const bindings = [...toml.matchAll(/^binding\s*=\s*"(DB_\d+)"/gm)].map((m) => m[1]);
const shardCount = Number(/^SHARD_COUNT\s*=\s*"(\d+)"/m.exec(toml)?.[1] ?? '1');

if (bindings.length !== shardCount || bindings.some((b, i) => b !== `DB_${i}`)) {
  console.error(`wrangler.toml declares [${bindings.join(', ')}] but SHARD_COUNT is ${shardCount}; they must be DB_0..DB_${shardCount - 1}`);
  process.exit(1);
}

for (const binding of bindings) {
  console.log(`migrating ${binding} (${mode.slice(2)})`);
  execFileSync('npx', ['wrangler', 'd1', 'migrations', 'apply', binding, mode], { stdio: 'inherit', shell: process.platform === 'win32' });
}
