// Regenerates shell.css (the 0.5.x-compatible, unlayered, self-contained stylesheet) from the two
// source files. `node scripts/build-shell-css.mjs` writes it; `--check` fails when it is stale.
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const read = (name) => readFileSync(join(root, name), "utf8");
const banner = "/* GENERATED from shell.structure.css + shell.visual.css by scripts/build-shell-css.mjs. Edit those files, then run the script. */\n";

export const combined = () => `${banner}${read("shell.structure.css").trimEnd()}\n\n${read("shell.visual.css").trimEnd()}\n`;

if (process.argv[1] === fileURLToPath(import.meta.url)) {
  const out = join(root, "shell.css");
  if (process.argv.includes("--check")) {
    if (readFileSync(out, "utf8") !== combined()) {
      console.error("shell.css is stale: run node scripts/build-shell-css.mjs");
      process.exit(1);
    }
  } else {
    writeFileSync(out, combined());
  }
}
