// The door's mirror of the broker's `rightkit-pnpm` adapter grammar (rustright-policy adapter.rs `scan`, M12 of the gate-vs-broker audit).
//
// The broker admits a pnpm job only when its FIRST argument is one of the adapter's registered `subcommands` (commands and the leading global
// flags the adapter registers), and every flag it meets is a registered flag. The door refuses before the broker would: a pnpm call whose first
// argument is not registered (a leading `--if-present`, `--prefix`, `--config`, ...) or whose global options (those before the command token)
// are not registered flags never reaches the client. Arguments after the command are not judged here: they are the broker's (the door neither
// adds nor removes a refusal there).
//
// PNPM_ADAPTER_GRAMMAR is the constant mirror of the registration on the managed hosts (build-control config.json, adapters["rightkit-pnpm"]).
// The door reads the LIVE spec when it can (liveAdapterGrammar) and falls back to this constant; a test compares the two on a host that has the
// registration (pnpm-adapter-grammar.test.mjs), so the constant cannot drift from what the broker enforces without a failing test.
import { readFileSync } from "node:fs";
import { resolveManagedRoots } from "./managed-roots.mjs";

export const PNPM_ADAPTER = "rightkit-pnpm";

/** subcommands: the registered first arguments, in registration order. flags: registered flag name to value kind. */
export const PNPM_ADAPTER_GRAMMAR = Object.freeze({
  subcommands: Object.freeze(["install", "run", "exec", "pack", "--dir", "--filter", "--workspace-concurrency=1", "--version", "-r", "--recursive", "-F", "-C", "--silent", "--reporter", "--workspace-concurrency"]),
  flags: Object.freeze({
    "--frozen-lockfile": "none", "--ignore-scripts": "none", "--offline": "none", "--if-present": "none", "--dir": "relpath", "--filter": "any", "--workspace-concurrency": "int",
    "--store-dir": "any", "-r": "none", "--recursive": "none", "-F": "any", "-C": "relpath", "--silent": "none", "--reporter": "any", "--pack-destination": "scratchPackDir",
  }),
  opaqueAfterTarget: Object.freeze(["exec"]),
  snapshotSubcommands: Object.freeze(["exec", "run", "pack"]),
});

/** The grammar a registered adapter spec declares (the fields the door uses), or null when `spec` is not a usable registration. */
export function grammarFromSpec(spec) {
  if (!spec || typeof spec !== "object" || !Array.isArray(spec.subcommands) || !spec.subcommands.every((s) => typeof s === "string" && s !== "")) return null;
  if (!Array.isArray(spec.flags) || !spec.flags.every((f) => f && typeof f.name === "string" && typeof f.value === "string")) return null;
  const list = (v) => Object.freeze(Array.isArray(v) ? v.filter((s) => typeof s === "string") : []);
  return Object.freeze({
    subcommands: Object.freeze([...spec.subcommands]),
    flags: Object.freeze(Object.fromEntries(spec.flags.map((f) => [f.name, f.value]))),
    opaqueAfterTarget: list(spec.opaqueAfterTarget),
    snapshotSubcommands: list(spec.snapshotSubcommands),
  });
}

/** The live registration on this host (the managed control root's config.json, adapters["rightkit-pnpm"]), or null when it cannot be read. */
export function liveAdapterGrammar({ env = process.env, platform = process.platform, readFile = (file) => readFileSync(file, "utf8") } = {}) {
  try {
    const { configPath } = resolveManagedRoots({ env, platform });
    return grammarFromSpec(JSON.parse(readFile(configPath))?.adapters?.[PNPM_ADAPTER]);
  } catch { return null; }
}

const flagName = (word) => (word.startsWith("--") && word.includes("=") ? word.slice(0, word.indexOf("=")) : word);

/**
 * Why the broker's `rightkit-pnpm` adapter would refuse the grammar of these pnpm arguments, or null. Judged: the first argument is registered
 * (adapter.rs scan: "subcommand ... is not registered"; "a subcommand is required"), and every global option before the command token is a
 * registered flag with the value its kind needs. `grammar` is the live spec's, else PNPM_ADAPTER_GRAMMAR.
 */
export function pnpmGrammarRefusal(words, grammar = PNPM_ADAPTER_GRAMMAR) {
  const args = words.map(String);
  if (args.length === 0) return "a subcommand is required";
  const first = args[0];
  if (!grammar.subcommands.includes(first)) return `${JSON.stringify(first)} is not a registered first argument of ${PNPM_ADAPTER} (registered: ${grammar.subcommands.join(", ")}); a leading flag such as --if-present, --prefix or --config is refused`;
  if (!first.startsWith("-")) return null; // a command first: no global option precedes it
  // The broker reads a registered flag in the first position as that flag (value included); any other registered first argument is consumed as the subcommand token.
  let i = Object.hasOwn(grammar.flags, flagName(first)) ? 0 : 1;
  while (i < args.length) {
    const word = args[i];
    if (word === "--" || !word.startsWith("-") || word === "-") break; // the command token (or the separator)
    const name = flagName(word);
    if (!Object.hasOwn(grammar.flags, name)) return `global option ${name} is not a registered flag of ${PNPM_ADAPTER}; --prefix and --config as global options are refused`;
    const inline = word.startsWith("--") && word.includes("=");
    const kind = grammar.flags[name];
    if (kind === "none") { if (inline) return `global option ${name} takes no value`; i += 1; continue; }
    if (!inline) {
      if (i + 1 >= args.length) return `global option ${name} needs a value`;
      i += 1;
    }
    i += 1;
  }
  return null;
}
