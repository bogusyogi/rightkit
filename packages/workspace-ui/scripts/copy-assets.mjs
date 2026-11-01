// Copies non-TypeScript assets (CSS, JSON, images, raw text) from src/ to dist/ after tsc.
import { cpSync, readdirSync, statSync, mkdirSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const src = join(root, "src");
const dist = join(root, "dist");
const code = /\.(?:[cm]?[jt]sx?)$/;

function walk(dir) {
  for (const name of readdirSync(dir)) {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) walk(path);
    else if (!code.test(name) || name.endsWith(".d.ts")) {
      const out = join(dist, relative(src, path));
      mkdirSync(dirname(out), { recursive: true });
      cpSync(path, out);
    }
  }
}
walk(src);
