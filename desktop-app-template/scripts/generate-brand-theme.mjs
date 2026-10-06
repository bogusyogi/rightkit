// Generates src/brand.css (Tailwind v4 @theme tokens) from assets/brand/brand.json.
// The brand file is the only source of brand colours; edit it, then `pnpm brand:sync`.
// Real apps replace the placeholder palette with the approved card from `/brand <code>`.
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const brand = JSON.parse(await fs.readFile(path.join(root, "assets/brand/brand.json"), "utf8"));
const colors = Object.entries(brand.colors);
for (const [name, value] of colors) {
  if (!/^brand(?:-[a-z]+)*$/.test(name) || !/^#[0-9a-f]{6}$/i.test(value)) {
    throw new Error(`Invalid brand token: ${name}`);
  }
}
const css =
  "/* Generated from assets/brand/brand.json; edit that file, then pnpm brand:sync. */\n@theme {\n" +
  colors.map(([n, v]) => `  --color-${n}: ${v};`).join("\n") +
  "\n}\n";
const target = path.join(root, "src/brand.css");
const existing = await fs.readFile(target, "utf8").catch((e) => {
  if (e.code !== "ENOENT") throw e;
  return "";
});
if (existing !== css) await fs.writeFile(target, css);
console.log(JSON.stringify({ event: "brand_theme_synced", brand: brand.code, tokens: colors.length }));
