// Test-only scanner for deep imports of @rightkit/release in a checked-out source tree.
//
// An "import" is a complete quoted specifier `@rightkit/release/<subpath>` in a code file (a JavaScript or TypeScript
// import, a dynamic import, `import.meta.resolve`, `createRequire().resolve`, or the same string inside a Rust, shell,
// PowerShell or workflow file that hands it to Node). Comment-only lines and prose files (Markdown, patches, JSON that
// is not a package.json) are never imports. A "path access" is a file under node_modules/@rightkit/release/ reached by
// a path string or path.join / .join(...) segments: the exports map does not govern it, but it is how some code still
// loads a module, so the scan reports it separately.
//
// Not shipped: lives under test-support/, outside package.json "files".
import { lstatSync, readdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

export const SCAN_EXTENSIONS = new Set([
  ".mjs", ".cjs", ".js", ".jsx", ".mts", ".cts", ".ts", ".tsx", ".sh", ".ps1", ".rs", ".yml", ".yaml", ".py",
]);
export const SKIP_DIRECTORIES = new Set([
  "node_modules", ".git", "target", "dist", "build", ".build", "DerivedData", ".audit", ".cache", ".pnpm-store", ".vite", ".next", ".turbo",
]);
const MAX_BYTES = 2 * 1024 * 1024;
const SPECIFIER = /(["'`])@rightkit\/release\/([A-Za-z0-9_][A-Za-z0-9_./-]*)\1/g;
const PATH_STRING = /node_modules\/@rightkit\/release\/([A-Za-z0-9_][A-Za-z0-9_./-]*)/g;
const JOIN_ARGUMENTS = /["']node_modules["']\s*,\s*["']@rightkit["']\s*,\s*["']release["']((?:\s*,\s*["'][^"'\r\n]+["'])*)/g;
const RUST_JOINS = /\.join\(\s*"@rightkit"\s*\)\s*\.join\(\s*"release"\s*\)((?:\s*\.join\(\s*"[^"\r\n]+"\s*\))*)/g;

/** How a specifier on `text` (a line of a file with extension `extension`) is used: for the inventory's Form column. */
export function formOf(text, extension = ".mjs") {
  if (/import\.meta\.resolve\(/.test(text)) return "import.meta.resolve";
  if (/\brequire(?:\([^)]*\))?\.resolve\(|createRequire\(/.test(text)) return "require.resolve";
  if (/\bimport\s*\(/.test(text)) return extension === ".rs" ? "dynamic import in a node snippet" : "dynamic import";
  if (/^\s*import\b|\bfrom\s+["'`]/.test(text)) return [".sh", ".yml", ".yaml", ".ps1"].includes(extension) ? "static import in a node snippet" : "static import";
  if ([".rs", ".sh", ".yml", ".yaml", ".ps1"].includes(extension)) return "module name handed to node";
  return "string literal";
}

export const isCommentLine = (line) => /^\s*(?:\/\/|\/\*|\*|#|<!--)/.test(line);

/** Subpaths (as written after the package name) and the 1-based line of each import found in `text`. */
export function importsInText(text) {
  const found = [];
  const lines = String(text).split(/\r?\n/);
  lines.forEach((line, index) => {
    if (isCommentLine(line)) return;
    for (const match of line.matchAll(SPECIFIER)) found.push({ line: index + 1, subpath: match[2], text: line });
  });
  return found;
}

/** Path accesses (node_modules/@rightkit/release/...) in `text`, with the subpath they reach. */
export function pathAccessesInText(text) {
  const source = String(text);
  const lineOf = (offset) => source.slice(0, offset).split(/\r?\n/).length;
  const found = [];
  const add = (offset, subpath) => {
    const line = source.split(/\r?\n/)[lineOf(offset) - 1] ?? "";
    if (!subpath || isCommentLine(line)) return;
    found.push({ line: lineOf(offset), subpath });
  };
  for (const match of source.matchAll(PATH_STRING)) add(match.index, match[1]);
  for (const match of source.matchAll(JOIN_ARGUMENTS)) add(match.index, [...match[1].matchAll(/["']([^"'\r\n]+)["']/g)].map((m) => m[1]).join("/"));
  for (const match of source.matchAll(RUST_JOINS)) add(match.index, [...match[1].matchAll(/"([^"\r\n]+)"/g)].map((m) => m[1]).join("/"));
  return found;
}

function* walk(root, directory = root) {
  let entries;
  try { entries = readdirSync(directory, { withFileTypes: true }); } catch { return; }
  for (const entry of entries.sort((a, b) => a.name.localeCompare(b.name))) {
    const full = path.join(directory, entry.name);
    if (entry.isSymbolicLink()) continue;
    if (entry.isDirectory()) {
      if (!SKIP_DIRECTORIES.has(entry.name)) yield* walk(root, full);
    } else if (entry.isFile()) {
      const code = SCAN_EXTENSIONS.has(path.extname(entry.name).toLowerCase());
      if (code || entry.name === "package.json") yield full;
    }
  }
}

/**
 * Scan one source tree. `file` in the result is relative to `root` with forward slashes.
 * Returns { imports: [{ file, line, subpath }], pathAccesses: [{ file, line, subpath }] }, sorted by file then line.
 */
export function scanTree(root) {
  const imports = [];
  const pathAccesses = [];
  for (const full of walk(root)) {
    let stat;
    try { stat = lstatSync(full); } catch { continue; }
    if (stat.size > MAX_BYTES) continue;
    let text;
    try { text = readFileSync(full, "utf8"); } catch { continue; }
    if (!text.includes("@rightkit")) continue;
    const file = path.relative(root, full).split(path.sep).join("/");
    const extension = path.extname(full).toLowerCase();
    const add = ({ text: lineText, ...found }) => imports.push({ file, ...found, form: formOf(lineText, extension) });
    if (path.basename(full) === "package.json") {
      // Only a script string can import; dependency names never carry a subpath.
      for (const found of importsInText(text.replaceAll("\\\"", "\""))) add(found);
      continue;
    }
    for (const found of importsInText(text)) add(found);
    for (const found of pathAccessesInText(text)) pathAccesses.push({ file, ...found });
  }
  return { imports, pathAccesses };
}

/** The `exports` key a subpath needs: `./<subpath>`. */
export const exportKey = (subpath) => `./${subpath}`;

/** Imports whose subpath is not an explicit key of `exportsMap`. */
export function importsOutsideMap(imports, exportsMap) {
  const keys = new Set(Object.keys(exportsMap ?? {}));
  return imports.filter((entry) => !keys.has(exportKey(entry.subpath)));
}

// node test-support/release-exports-scan.mjs <label>=<dir> ...  prints the inventory rows of docs/RELEASE-EXPORTS.md for each tree.
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  for (const spec of process.argv.slice(2)) {
    const split = spec.indexOf("=");
    const [label, root] = split > 0 ? [spec.slice(0, split), spec.slice(split + 1)] : [path.basename(spec), spec];
    const { imports, pathAccesses } = scanTree(root);
    for (const entry of imports) process.stdout.write(`| \`${entry.subpath}\` | ${label} | \`${entry.file}:${entry.line}\` | ${entry.form} |\n`);
    for (const entry of pathAccesses) process.stdout.write(`path access | ${label} | ${entry.file}:${entry.line} | ${entry.subpath}\n`);
  }
}
