import { readFileSync, writeFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const sourceRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "../../release");
const outputRoot = path.dirname(fileURLToPath(import.meta.url));
const FILES = ["rightkit-ownership.json", "rightkit-versions.json"];

export async function syncOwnershipCatalog({ source = sourceRoot, output = outputRoot } = {}) {
  if (source === sourceRoot && output === outputRoot) {
    const { writeOwnershipCatalog } = await import("../../release/ownership-catalog.mjs");
    const { syncConsumerCopies } = await import("../../release/rightkit-sync.mjs");
    const root = path.resolve(source, "../..");
    writeOwnershipCatalog(root);
    return syncConsumerCopies(root);
  }
  for (const name of FILES) {
    const content = readFileSync(path.join(source, name), "utf8");
    JSON.parse(content);
    writeFileSync(path.join(output, name === "rightkit-ownership.json" ? "ownership-catalog.json" : name), content);
  }
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) await syncOwnershipCatalog();
