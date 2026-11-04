// Cargo argv width is explicit so broker admission, cache keys and signed receipts see the same request.
const COMPILE = new Set(["build", "check", "clippy", "doc", "rustdoc", "test", "bench"]);

export function parseCargoWidth(value) {
  if (typeof value !== "string" || !/^[1-9][0-9]*$/.test(value)) throw new Error("--cargo-jobs needs a positive decimal integer");
  const width = Number(value);
  if (!Number.isSafeInteger(width) || width > 0xffffffff) throw new Error("--cargo-jobs is out of range");
  return width;
}

export function cargoWidthArgs(argv, width) {
  if (!Array.isArray(argv) || !argv.every((x) => typeof x === "string") || !COMPILE.has(argv[0])) throw new Error("--cargo-jobs cannot control this Cargo command");
  if (!Number.isSafeInteger(width) || width < 1 || width > 0xffffffff) throw new Error("invalid Cargo width");
  const end = argv.indexOf("--");
  if (argv.slice(1, end < 0 ? undefined : end).some((arg) => arg === "--jobs" || arg.startsWith("--jobs=") || arg === "-j" || arg.startsWith("-j"))) throw new Error("--cargo-jobs conflicts with an existing Cargo jobs flag");
  return [argv[0], "--jobs", String(width), ...argv.slice(1)];
}

export function validateCargoWidthPool(width, status) {
  const size = status?.type === "LIST" && status?.broker?.phase === "READY" ? status.broker.tokens?.size : null;
  if (!Number.isSafeInteger(size) || size < 1) throw new Error("broker does not report a ready compile-token pool");
  if (width > size) throw new Error(`--cargo-jobs ${width} exceeds broker token pool ${size}`);
  return size;
}

/** Width is supported only on the shared Cargo unit-list door, never on command or app routes. */
export function cargoWidthRouteProblem(args) {
  if (!Array.isArray(args) || !args[0]?.startsWith("--") || args.includes("--")) return "--cargo-jobs needs a controlled unit-list compile";
  const at = args.indexOf("--to");
  if (at < 0 || args[at + 1] !== "compile" || !(args.includes("--units-file") || args.includes("--changed")) || ["--route", "--unit", "--all", "--all-units-exception", "--no-broker"].some((flag) => args.includes(flag))) return "--cargo-jobs needs a controlled unit-list compile";
  return null;
}

export function cargoWidthUnitsProblem(selected) {
  if (!Array.isArray(selected) || selected.some((unit) => typeof unit?.selector !== "string" || !/^cargo:rightkit-[a-z0-9-]+$/.test(unit.selector))) return "--cargo-jobs supports Cargo-only unit lists; nested npm, native and custom adapters are uncontrolled";
  return null;
}

export function cargoWidthScanProblem(scan, cargoCommand, width) {
  const prefix = cargoCommand.slice(1);
  if (scan?.tool !== cargoCommand[0] || !Array.isArray(scan.args) || prefix.some((word, i) => scan.args[i] !== word)) return 'uncontrolled Class B command';
  const args = scan.args.slice(prefix.length);
  if (!COMPILE.has(args[0])) return 'uncontrolled Cargo subcommand';
  const end = args.indexOf('--');
  const own = args.slice(1, end < 0 ? undefined : end);
  const flags = own.flatMap((word, i) => word === '--jobs' || word.startsWith('--jobs=') || word === '-j' || word.startsWith('-j') ? [i] : []);
  if (flags.length !== 1 || own[flags[0]] !== '--jobs' || own[flags[0] + 1] !== String(width)) return 'uncontrolled Cargo width';
  return null;
}
