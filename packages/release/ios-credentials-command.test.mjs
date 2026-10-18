import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import { Readable } from "node:stream";
import test from "node:test";
import { main, parseIosCredentialsArgs } from "./ios-credentials-command.mjs";

const KEY_ID = "ABC123DEFG";
const ISSUER = "69a6de80-0b1c-47e3-e053-5b8c7c11a4d1";
const { privateKey: PEM } = generateKeyPairSync("ec", { namedCurve: "prime256v1", privateKeyEncoding: { type: "pkcs8", format: "pem" }, publicKeyEncoding: { type: "spki", format: "pem" } });
const pipe = (text, isTTY = false) => Object.assign(Readable.from([Buffer.from(text)]), { isTTY });

function keychain() {
  const items = new Map();
  const calls = [];
  const run = (cmd, args, options = {}) => {
    calls.push({ cmd, args: [...args], input: options.input });
    if (args[0] === "-i") {
      for (const line of String(options.input).split("\n")) {
        const match = /^add-generic-password -U -a "([^"]+)" -s "([^"]+)" -l "[^"]+" -w "([^"]+)"$/.exec(line);
        if (match) items.set(`${match[2]}|${match[1]}`, match[3]);
      }
      return { status: 0, stdout: "" };
    }
    const value = items.get(`${args[2]}|${args[4]}`);
    return value === undefined ? { status: 44, stdout: "" } : { status: 0, stdout: `${value}\n` };
  };
  return { items, calls, run };
}

test("arguments: store and check, service flags, and a clear usage error", () => {
  assert.deepEqual(parseIosCredentialsArgs(["store", "--key-id", KEY_ID, "--issuer", ISSUER, "--service", "my.key", "--account", "me"]), { command: "store", credentials: { keyService: "my.key", account: "me" }, keyId: KEY_ID, issuer: ISSUER });
  assert.equal(parseIosCredentialsArgs(["check"]).command, "check");
  assert.throws(() => parseIosCredentialsArgs(["wipe"]), { code: "IOS_CREDENTIALS_USAGE" });
  assert.throws(() => parseIosCredentialsArgs(["check", "--key-id", KEY_ID]), { code: "IOS_CREDENTIALS_USAGE" });
  assert.throws(() => parseIosCredentialsArgs(["store", "--service"]), { code: "IOS_CREDENTIALS_USAGE" });
  assert.throws(() => parseIosCredentialsArgs(["store", "--key-path", "/x.p8"]), { code: "IOS_CREDENTIALS_USAGE" });
});

test("store reads the key from stdin, writes the three items and prints no key", async () => {
  const kc = keychain();
  const logs = [];
  const code = await main(["store", "--key-id", KEY_ID, "--issuer", ISSUER], { stdin: pipe(PEM), env: { USER: "tester" }, platform: "darwin", run: kc.run, log: (line) => logs.push(line) });
  assert.equal(code, 0);
  assert.equal(kc.items.size, 3);
  assert.ok(!logs.join("\n").match(/PRIVATE KEY/));
  assert.ok(!logs.join("\n").includes([...kc.items.values()][0]));
  const checked = [];
  assert.equal(await main(["check"], { env: { USER: "tester" }, platform: "darwin", run: kc.run, log: (line) => checked.push(line) }), 0);
  assert.match(checked[0], new RegExp(`key id ${KEY_ID}`));
});

test("store refuses a key typed at a terminal, a missing id or issuer, and a non-macOS host", async () => {
  const env = { USER: "tester" };
  await assert.rejects(main(["store", "--key-id", KEY_ID, "--issuer", ISSUER], { stdin: pipe(PEM, true), env, platform: "darwin", run: () => assert.fail("nothing is stored"), log: () => {} }), { code: "IOS_CREDENTIALS_USAGE" });
  await assert.rejects(main(["store", "--issuer", ISSUER], { stdin: pipe(PEM), env, platform: "darwin", run: () => assert.fail("nothing is stored"), log: () => {} }), { code: "IOS_CREDENTIALS_USAGE" });
  await assert.rejects(main(["store", "--key-id", KEY_ID, "--issuer", ISSUER], { stdin: pipe(PEM), env, platform: "win32", run: () => assert.fail("nothing is stored"), log: () => {} }), { code: "IOS_ASC_HOST_UNSUPPORTED" });
  await assert.rejects(main(["store", "--key-id", KEY_ID, "--issuer", ISSUER], { stdin: pipe("not a key"), env, platform: "darwin", run: () => assert.fail("nothing is stored"), log: () => {} }), { code: "IOS_ASC_KEY_INVALID" });
});

test("help prints the default service names", async () => {
  const logs = [];
  assert.equal(await main(["--help"], { log: (line) => logs.push(line) }), 0);
  assert.match(logs[0], /rightkit-asc-api-key/);
});
