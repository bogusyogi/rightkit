// right-release ios-credentials store | check  (bogusyogi/rightkit#349, settled decision D4)
//
//   cat AuthKey_ABC123DEFG.p8 | right-release ios-credentials store --key-id ABC123DEFG --issuer <issuer-uuid>
//                               [--service <key service>] [--key-id-service <name>] [--issuer-service <name>] [--account <name>]
//   right-release ios-credentials check [same service flags]
//
// `store` reads the .p8 from stdin and writes the key, the key id and the issuer id into the macOS login Keychain once. The key
// is never written to disk by this command and never appears in an argument or a log line (it goes to `security -i` on stdin).
// Pipe it from a secure source and delete the downloaded .p8 afterwards. A key typed at a terminal is refused: it would echo.
// `check` reads the three items back and prints the key id, the issuer id and the service names, never the key.
import { realpathSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "./background-process.mjs";
import { checkAscCredentials, IosCredentialsError, iosCredentialsStoreHint, storeAscCredentials } from "./ios-asc-credentials.mjs";

const FLAGS = new Map([["--service", "keyService"], ["--key-id-service", "keyIdService"], ["--issuer-service", "issuerService"], ["--account", "account"]]);

export function parseIosCredentialsArgs(args) {
  const [command, ...rest] = args;
  if (command !== "store" && command !== "check") throw new IosCredentialsError("IOS_CREDENTIALS_USAGE", "expected: right-release ios-credentials store|check");
  const parsed = { command, credentials: {}, keyId: undefined, issuer: undefined };
  for (let index = 0; index < rest.length; index += 1) {
    const arg = rest[index];
    const value = () => {
      const next = rest[++index];
      if (!next || next.startsWith("--")) throw new IosCredentialsError("IOS_CREDENTIALS_USAGE", `missing value for ${arg}`);
      return next;
    };
    if (FLAGS.has(arg)) parsed.credentials[FLAGS.get(arg)] = value();
    else if (arg === "--key-id" && command === "store") parsed.keyId = value();
    else if (arg === "--issuer" && command === "store") parsed.issuer = value();
    else throw new IosCredentialsError("IOS_CREDENTIALS_USAGE", `unknown option: ${arg}`);
  }
  return parsed;
}

const readStdin = async (stream) => {
  const chunks = [];
  for await (const chunk of stream) chunks.push(Buffer.from(chunk));
  return Buffer.concat(chunks).toString("utf8");
};

export async function main(args = process.argv.slice(2), { stdin = process.stdin, env = process.env, platform = process.platform, run = spawnSync, log = console.log } = {}) {
  if (args.includes("--help") || args.includes("-h") || args.length === 0) {
    log("right-release ios-credentials store --key-id <KEYID> --issuer <issuer-uuid> [--service <name>] [--key-id-service <name>] [--issuer-service <name>] [--account <name>]   (the .p8 comes from stdin)\nright-release ios-credentials check [--service <name>] [--key-id-service <name>] [--issuer-service <name>] [--account <name>]\nDefault Keychain services: rightkit-asc-api-key, rightkit-asc-key-id, rightkit-asc-issuer-id. macOS only; nothing is written to disk.");
    return 0;
  }
  const parsed = parseIosCredentialsArgs(args);
  const credentials = Object.keys(parsed.credentials).length ? parsed.credentials : undefined;
  if (parsed.command === "check") {
    const found = checkAscCredentials({ credentials, env, platform, run });
    log(`ios-credentials: key id ${found.keyId}, issuer ${found.issuer}, services ${found.services.join(", ")} (account ${found.account}); the key itself is present and parses`);
    return 0;
  }
  if (!parsed.keyId || !parsed.issuer) throw new IosCredentialsError("IOS_CREDENTIALS_USAGE", `store needs --key-id and --issuer; ${iosCredentialsStoreHint()}`);
  if (stdin.isTTY) throw new IosCredentialsError("IOS_CREDENTIALS_USAGE", `the key must be piped into stdin (a key typed into a terminal would echo); ${iosCredentialsStoreHint()}`);
  const keyText = await readStdin(stdin);
  const stored = storeAscCredentials({ keyText, keyId: parsed.keyId, issuer: parsed.issuer, credentials, env, platform, run });
  log(`ios-credentials: stored key id ${stored.keyId} and issuer ${stored.issuer} in the login Keychain (services ${stored.services.join(", ")}, account ${stored.account}); delete the .p8 file you piped in`);
  return 0;
}

const realOrSelf = (file) => { try { return realpathSync(file); } catch { return path.resolve(file); } };
if (process.argv[1] && realOrSelf(process.argv[1]) === realOrSelf(fileURLToPath(import.meta.url))) {
  main().then((code) => { process.exitCode = code; }).catch((error) => {
    console.error(`right-release ios-credentials: ${error.code ? `[${error.code}] ` : ""}${error.message}`);
    process.exitCode = 1;
  });
}
