/** Derive native feature labels from the exact cargo test commands recorded by the gate. */
export function featureLabels(registry, unitName, commands) {
  if (registry === "npm") return ["default"];
  const labels = [];
  for (const command of commands) {
    const args = command.split(/\s+/);
    const at = args.indexOf("-p");
    if (args[at + 1] !== unitName) throw new Error(`${unitName}: malformed gate test command`);
    const stop = args.indexOf("--no-fail-fast", at + 2);
    if (stop < 0) throw new Error(`${unitName}: gate test command has no feature boundary`);
    labels.push(args.slice(at + 2, stop).join(" ") || "default");
  }
  if (!labels.length) throw new Error(`${unitName}: no cargo feature-bearing test commands`);
  return [...new Set(labels)];
}
