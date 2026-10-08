import path from "node:path";

// Closed grammar shared by shim & agent. Never accept executable names, an
// environment, keychains, detached signatures, or file-producing display flags.
export function parseCodesignArgv(argv, cwd) {
  const deny = () => { const error = new Error("Unsupported codesign argv"); error.code = "MAC_SIGNING_AGENT_ARGV_DENIED"; throw error; };
  if (!Array.isArray(argv) || !argv.length || argv.length > 8192
    || argv.some((arg) => typeof arg !== "string" || !arg || arg.includes("\0") || arg.length > 32768)
    || typeof cwd !== "string" || !path.isAbsolute(cwd)) deny();
  const targets = [], inputs = [], options = [];
  let mode, verbose = false, operands = false;
  const operation = (next) => { if (mode) deny(); mode = next; };
  const value = (inline, i) => {
    const result = inline ?? argv[i + 1];
    if (!result || result.startsWith("--")) deny();
    return result;
  };
  for (let i = 0; i < argv.length; i++) {
    const arg = argv[i];
    if (operands || !arg.startsWith("-")) { targets.push(path.isAbsolute(arg) ? arg : `${cwd}/${arg}`); continue; }
    if (arg === "--") { operands = true; continue; }
    if (/^-[dv]+$/.test(arg)) {
      if (arg.includes("d")) { if ((arg.match(/d/g) ?? []).length !== 1) deny(); operation("display"); }
      if (arg.includes("v")) verbose = true;
      continue;
    }
    const equal = arg.indexOf("=");
    const flag = equal < 0 ? arg : arg.slice(0, equal);
    const inline = equal < 0 ? undefined : arg.slice(equal + 1);
    if (flag === "--display" && inline === undefined) { operation("display"); continue; }
    if (flag === "--verify" && inline === undefined) { operation("verify"); continue; }
    if (flag === "--verbose") { if (inline !== undefined && !/^\d{1,2}$/.test(inline)) deny(); verbose = true; continue; }
    if (["-s", "--sign"].includes(flag) || /^-s.+/.test(arg)) {
      const attached = flag === "-s" || flag === "--sign" ? inline : arg.slice(2);
      const identity = value(attached, i);
      if (identity.length > 512 || (identity.startsWith("-") && identity !== "-")) deny();
      operation("sign"); if (attached === undefined) i++;
      continue;
    }
    if (["--force", "--deep", "--strict"].includes(flag) && inline === undefined) { options.push(flag); continue; }
    if (flag === "--timestamp" && (inline === undefined || inline === "none")) { options.push(flag); continue; }
    if (["--options", "--identifier", "--preserve-metadata", "--entitlements", "--requirements"].includes(flag) || arg === "-r-") {
      const parameter = arg === "-r-" ? "-" : value(inline, i);
      if (arg !== "-r-" && inline === undefined) i++;
      if (flag === "--options" && !/^(runtime|hard|kill|host|expires|restrict|library)(,(runtime|hard|kill|host|expires|restrict|library))*$/.test(parameter)) deny();
      if (flag === "--preserve-metadata" && !/^(identifier|entitlements|requirements|flags|runtime)(,(identifier|entitlements|requirements|flags|runtime))*$/.test(parameter)) deny();
      if (flag === "--identifier" && (parameter.startsWith("-") || parameter.length > 512)) deny();
      options.push(arg === "-r-" ? "--requirements" : flag);
      if (flag === "--entitlements") inputs.push({ value: parameter, path: path.isAbsolute(parameter) ? parameter : `${cwd}/${parameter}` });
      if ((flag === "--requirements" || arg === "-r-") && parameter !== "-") deny();
      continue;
    }
    deny();
  }
  mode ??= verbose ? "verify" : undefined;
  if (!mode || !targets.length || targets.length > 4096) deny();
  const permitted = mode === "sign"
    ? ["--force", "--options", "--timestamp", "--entitlements", "--deep", "--preserve-metadata", "--identifier"]
    : mode === "verify" ? ["--strict", "--deep"] : ["--entitlements", "--requirements"];
  if (options.some((flag) => !permitted.includes(flag))) deny();
  if (mode === "display" && inputs.some((input) => ![":-", "-"].includes(input.value))) deny();
  if (mode === "sign" && inputs.some((input) => [":-", "-"].includes(input.value))) deny();
  return { mode, targets, entitlements: mode === "sign" ? inputs.map((input) => input.path) : [] };
}
