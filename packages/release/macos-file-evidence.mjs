// Content evidence for a macOS signing entry: a plain file hashes its bytes; a directory (.app bundle)
// hashes a deterministic tree digest. Shared by sign-macos.mjs (writes the receipt) and release.mjs (verifies it).
import { createHash } from "node:crypto";
import { lstatSync, readdirSync, readFileSync, readlinkSync, statSync } from "node:fs";
import path from "node:path";

export function fileEvidence(file) {
  if (statSync(file).isDirectory()) {
    const digest = createHash("sha256");
    let sizeBytes = 0;
    const visit = (directory) => {
      for (const name of readdirSync(directory).sort()) {
        const entry = path.join(directory, name);
        const info = lstatSync(entry);
        const type = info.isSymbolicLink() ? "link" : info.isDirectory() ? "directory" : "file";
        const bytes = type === "link" ? Buffer.from(readlinkSync(entry)) : type === "file" ? readFileSync(entry) : Buffer.alloc(0);
        digest.update(`${path.relative(file, entry).split(path.sep).join("/")}\0${type}\0${bytes.length}\0`).update(bytes);
        sizeBytes += bytes.length;
        if (type === "directory") visit(entry);
      }
    };
    visit(file);
    return { sha256: digest.digest("hex"), sizeBytes, kind: "bundle-tree-v1" };
  }
  const bytes = readFileSync(file);
  return { sha256: createHash("sha256").update(bytes).digest("hex"), sizeBytes: statSync(file).size };
}
