// GB-67: SwiftPM's build state belongs to the broker. For a snapshot-bound `swift build` through the registered swift adapter RustRight appends ONE
// `--scratch-path <outputDir>/swiftpm/<sha256 of the package's snapshot-relative path>` (the same directory for `build` and `build --show-bin-path` of one
// package, on every run and platform) and never creates a second `.build`. A caller-supplied `--scratch-path`, `-scratch-path` or `--build-path` (also `=`-spelled) is
// refused at admission with RK_ADAPTER_ARGV_DENIED and a message naming the canonical route. The gate therefore sends neither, refuses them before anything is sent
// with the same explanation, and strips nothing silently: the product location is read from `build --show-bin-path`.
const SCRATCH_OPTIONS = ["--scratch-path", "-scratch-path", "--build-path", "-build-path"];
export const SWIFT_SCRATCH_REFUSED = "RK_SWIFT_SCRATCH_OWNED";

/** The caller-supplied scratch or build-path option in a swift argv (before any `--`), or null. */
export function swiftScratchOption(words) {
  const end = words.indexOf("--");
  for (const word of end < 0 ? words : words.slice(0, end)) {
    const w = String(word);
    const name = w.includes("=") ? w.slice(0, w.indexOf("=")) : w;
    if (SCRATCH_OPTIONS.includes(name)) return w;
  }
  return null;
}

/** `{ code, message }` when a swift argv names a scratch or build path the broker owns; null otherwise. */
export function swiftScratchRefusal(words) {
  const option = swiftScratchOption(words);
  if (option === null) return null;
  return { code: SWIFT_SCRATCH_REFUSED, message: `\`swift ${words.join(" ")}\` passes ${option.split("=")[0]}, but the broker owns SwiftPM's build state (it appends the one managed --scratch-path and never creates a second .build); send \`build\` and \`build --show-bin-path\` without it and read the product location from --show-bin-path; nothing is stripped or rewritten, so nothing was sent` };
}
