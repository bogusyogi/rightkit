// The RustRight production vectors (GB-01..GB-84, BROKER-CONTRACT section 14), pinned to their source identity.
// `loadVector(file)` reads one copied vector and refuses it when its bytes are not the sha256 recorded in vectors/vectors-source.json
// (so a hand edit or a stale copy fails loudly), and when its `schema` is not the pinned one.
import crypto from "node:crypto";
import fs from "node:fs";

const dir = new URL("./vectors/", import.meta.url);
export const vectorsSource = () => JSON.parse(fs.readFileSync(new URL("vectors-source.json", dir), "utf8"));

export function loadVector(file) {
  const pin = vectorsSource().files[file];
  if (!pin) throw new Error(`vector ${file} is not pinned in vectors-source.json`);
  const bytes = fs.readFileSync(new URL(file, dir));
  const digest = crypto.createHash("sha256").update(bytes).digest("hex");
  if (digest !== pin.sha256) throw new Error(`vector ${file} has sha256 ${digest}, not the pinned ${pin.sha256}: recopy it from the pinned RustRight commit`);
  const value = JSON.parse(bytes);
  if ((value.schema ?? null) !== pin.schema) throw new Error(`vector ${file} has schema ${value.schema}, not ${pin.schema}`);
  return value;
}
