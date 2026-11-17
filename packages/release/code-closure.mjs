// The files a piece of gate code is made of: the entry files and every file they import by a relative path, followed to the end.
// Two users: the build-together path's remembered-call key (gate/compile.mjs) and the contract-test receipt (gate-acceptance.mjs), which
// lists the runner's files so an installer can compare each with the copy inside a sealed gate.
import fs from "node:fs";
import path from "node:path";

const IMPORT = /(?:\bfrom\s*|\bimport\s*\(?\s*)["'](\.{1,2}\/[^"']+)["']/g;

/**
 * [{ name, text }] sorted by name; `name` is the file's path relative to `base`, with forward slashes.
 * An import that cannot be read throws: a hash or a receipt is never made over a list that skipped a file.
 */
export function codeClosure(entries, base, { readFile = (f) => fs.readFileSync(f, "utf8") } = {}) {
  const seen = new Map();
  const todo = [...entries];
  while (todo.length) {
    const file = todo.pop();
    if (seen.has(file)) continue;
    const text = readFile(file);
    seen.set(file, text);
    for (const m of text.matchAll(IMPORT)) todo.push(path.resolve(path.dirname(file), m[1]));
  }
  return [...seen].map(([file, text]) => ({ name: path.relative(base, file).split(path.sep).join("/"), text })).sort((a, b) => (a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}
