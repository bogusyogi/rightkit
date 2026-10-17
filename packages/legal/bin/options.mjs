import path from "node:path";

export function options(args, flags, switches = []) {
  const values = {};
  for (let i = 0; i < args.length; i++) {
    const arg = args[i];
    if (switches.includes(arg)) values[arg] = true;
    else if (flags.includes(arg)) {
      const value = args[++i];
      if (!value || value.startsWith("--")) throw new Error(`missing value for ${arg}`);
      values[arg] = value;
    } else throw new Error(`unknown argument: ${arg}`);
  }
  values.root = path.resolve(values["--root"] ?? process.cwd());
  return values;
}
