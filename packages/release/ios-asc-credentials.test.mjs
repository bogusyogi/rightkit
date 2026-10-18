import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import test from "node:test";
import {
  ASC_ENV_NAMES, DEFAULT_ASC_SERVICES, IosCredentialsError, checkAscCredentials, parseAscPrivateKey, readAscCredentials,
  refuseIosAscEnv, resolveAscCredentialSource, storeAscCredentials,
} from "./ios-asc-credentials.mjs";

const KEY_ID = "ABC123DEFG";
const ISSUER = "69a6de80-0b1c-47e3-e053-5b8c7c11a4d1";
const { privateKey: EC_PEM } = generateKeyPairSync("ec", { namedCurve: "prime256v1", privateKeyEncoding: { type: "pkcs8", format: "pem" }, publicKeyEncoding: { type: "spki", format: "pem" } });
const EC_DER_B64 = parseAscPrivateKey(EC_PEM).export({ type: "pkcs8", format: "der" }).toString("base64");
const { privateKey: RSA_PEM } = generateKeyPairSync("rsa", { modulusLength: 2048, privateKeyEncoding: { type: "pkcs8", format: "pem" }, publicKeyEncoding: { type: "spki", format: "pem" } });
const coded = (code, fn) => assert.throws(fn, (error) => error.code === code, code);
const MANAGED = { RIGHTKIT_BUILD_BROKER_SOCKET: "/tmp/broker.sock" };

// A fake login Keychain: `security find-generic-password` reads it, `security -i` writes it from stdin.
function fakeKeychain(initial = {}) {
  const items = new Map(Object.entries(initial));
  const calls = [];
  const run = (cmd, args, options = {}) => {
    calls.push({ cmd, args: [...args], input: options.input });
    if (cmd !== "security") return { status: 127 };
    if (args[0] === "find-generic-password") {
      assert.deepEqual(args.slice(0, 1).concat(args[1], args[3], args[5]), ["find-generic-password", "-s", "-a", "-w"]);
      const value = items.get(`${args[2]}|${args[4]}`);
      return value === undefined ? { status: 44, stdout: "" } : { status: 0, stdout: `${value}\n` };
    }
    if (args[0] === "-i") {
      for (const line of String(options.input).split("\n")) {
        const match = /^add-generic-password -U -a "([^"]+)" -s "([^"]+)" -l "[^"]+" -w "([^"]+)"$/.exec(line);
        if (match) items.set(`${match[2]}|${match[1]}`, match[3]);
      }
      return { status: 0, stdout: "" };
    }
    return { status: 1 };
  };
  return { items, calls, run };
}
const seeded = () => fakeKeychain({
  [`${DEFAULT_ASC_SERVICES.keyService}|tester`]: EC_DER_B64,
  [`${DEFAULT_ASC_SERVICES.keyIdService}|tester`]: KEY_ID,
  [`${DEFAULT_ASC_SERVICES.issuerService}|tester`]: ISSUER,
});
const env = { USER: "tester" };

test("service names default and can be configured; only Keychain service names are accepted", () => {
  assert.deepEqual(resolveAscCredentialSource(undefined, { env }), { ...DEFAULT_ASC_SERVICES, account: "tester" });
  const custom = resolveAscCredentialSource({ keyService: "a.key", keyIdService: "a.id", issuerService: "a.iss", account: "me" }, { env });
  assert.equal(custom.keyService, "a.key");
  assert.equal(custom.account, "me");
  coded("IOS_ASC_CONFIG_INVALID", () => resolveAscCredentialSource({ keyService: "same", keyIdService: "same" }, { env }));
  coded("IOS_ASC_CONFIG_INVALID", () => resolveAscCredentialSource({ keyService: "bad service" }, { env }));
  coded("IOS_ASC_CONFIG_INVALID", () => resolveAscCredentialSource({ colour: "red" }, { env }));
});

test("a key path, key id, issuer, inline key or env name in the credentials block is refused", () => {
  for (const key of ["keyPath", "keyFile", "file", "path", "p8", "pem", "key", "keyId", "issuer", "issuerId", "env", "password", "secret", "token"]) {
    coded("IOS_ASC_CREDENTIAL_SOURCE_REFUSED", () => resolveAscCredentialSource({ [key]: "x" }, { env }));
  }
});

test("key material in ANY environment variable is refused on every host; ASC variable names are refused on managed hosts", () => {
  assert.doesNotThrow(() => refuseIosAscEnv({ PATH: "/usr/bin", APPLE_API_KEY_PATH: "/Users/me/AuthKey.p8" }));
  coded("IOS_ASC_ENV_SECRET_REFUSED", () => refuseIosAscEnv({ SOMETHING: EC_PEM }));
  for (const name of ASC_ENV_NAMES) coded("MANAGED_HOST_ENV_SECRET_REFUSED", () => refuseIosAscEnv({ ...MANAGED, [name]: "value" }));
  assert.doesNotThrow(() => refuseIosAscEnv({ ...MANAGED, APPLE_API_KEY: "" }));
});

test("the key, key id and issuer come from the Keychain with the documented security command", () => {
  const keychain = seeded();
  const found = readAscCredentials({ env, platform: "darwin", run: keychain.run });
  assert.equal(found.keyId, KEY_ID);
  assert.equal(found.issuer, ISSUER);
  assert.equal(found.privateKey.asymmetricKeyType, "ec");
  assert.deepEqual(keychain.calls.map((call) => call.args), [
    ["find-generic-password", "-s", DEFAULT_ASC_SERVICES.keyService, "-a", "tester", "-w"],
    ["find-generic-password", "-s", DEFAULT_ASC_SERVICES.keyIdService, "-a", "tester", "-w"],
    ["find-generic-password", "-s", DEFAULT_ASC_SERVICES.issuerService, "-a", "tester", "-w"],
  ]);
  // custom service names and account reach the command
  const custom = fakeKeychain({ "k|me": EC_DER_B64, "i|me": KEY_ID, "s|me": ISSUER });
  readAscCredentials({ credentials: { keyService: "k", keyIdService: "i", issuerService: "s", account: "me" }, env, platform: "darwin", run: custom.run });
  assert.deepEqual(custom.calls[0].args, ["find-generic-password", "-s", "k", "-a", "me", "-w"]);
});

test("a PEM stored by hand is accepted; a missing item names the service, never a value", () => {
  const pem = fakeKeychain({ [`${DEFAULT_ASC_SERVICES.keyService}|tester`]: EC_PEM.replace(/\n/g, "\n"), [`${DEFAULT_ASC_SERVICES.keyIdService}|tester`]: KEY_ID, [`${DEFAULT_ASC_SERVICES.issuerService}|tester`]: ISSUER });
  assert.equal(readAscCredentials({ env, platform: "darwin", run: pem.run }).keyId, KEY_ID);
  const missing = fakeKeychain({ [`${DEFAULT_ASC_SERVICES.keyIdService}|tester`]: KEY_ID });
  assert.throws(() => readAscCredentials({ env, platform: "darwin", run: missing.run }), (error) => error.code === "IOS_ASC_CREDENTIAL_MISSING"
    && error.message.includes(DEFAULT_ASC_SERVICES.keyService) && error.message.includes("ios-credentials store") && !error.message.includes(KEY_ID));
});

test("credentials are read on macOS only, and a bad key, key id or issuer fails without echoing the value", () => {
  coded("IOS_ASC_HOST_UNSUPPORTED", () => readAscCredentials({ env, platform: "win32", run: () => assert.fail("no command off macOS") }));
  const rsa = fakeKeychain({ [`${DEFAULT_ASC_SERVICES.keyService}|tester`]: RSA_PEM, [`${DEFAULT_ASC_SERVICES.keyIdService}|tester`]: KEY_ID, [`${DEFAULT_ASC_SERVICES.issuerService}|tester`]: ISSUER });
  assert.throws(() => readAscCredentials({ env, platform: "darwin", run: rsa.run }), (error) => error.code === "IOS_ASC_KEY_INVALID" && !error.message.includes("PRIVATE"));
  const junk = fakeKeychain({ [`${DEFAULT_ASC_SERVICES.keyService}|tester`]: "bm90IGEga2V5", [`${DEFAULT_ASC_SERVICES.keyIdService}|tester`]: KEY_ID, [`${DEFAULT_ASC_SERVICES.issuerService}|tester`]: ISSUER });
  coded("IOS_ASC_KEY_INVALID", () => readAscCredentials({ env, platform: "darwin", run: junk.run }));
  const badId = fakeKeychain({ [`${DEFAULT_ASC_SERVICES.keyService}|tester`]: EC_DER_B64, [`${DEFAULT_ASC_SERVICES.keyIdService}|tester`]: "short", [`${DEFAULT_ASC_SERVICES.issuerService}|tester`]: ISSUER });
  coded("IOS_ASC_KEY_ID_INVALID", () => readAscCredentials({ env, platform: "darwin", run: badId.run }));
});

test("a managed host with an ASC variable set refuses before any Keychain read", () => {
  const keychain = seeded();
  coded("MANAGED_HOST_ENV_SECRET_REFUSED", () => readAscCredentials({ env: { ...env, ...MANAGED, APPLE_API_KEY_PATH: "/x/AuthKey.p8" }, platform: "darwin", run: keychain.run }));
  assert.equal(keychain.calls.length, 0);
});

test("store sends the key to security -i on stdin, never as an argument, and verifies the read-back", () => {
  const keychain = fakeKeychain();
  const stored = storeAscCredentials({ keyText: EC_PEM, keyId: KEY_ID, issuer: ISSUER, env, platform: "darwin", run: keychain.run });
  assert.equal(stored.account, "tester");
  const write = keychain.calls.find((call) => call.args[0] === "-i");
  assert.deepEqual(write.args, ["-i"]);
  assert.ok(write.input.includes(`-w "${EC_DER_B64}"`));
  assert.ok(write.input.endsWith("exit\n"));
  for (const call of keychain.calls) for (const arg of call.args) assert.ok(!arg.includes(EC_DER_B64) && !arg.includes("PRIVATE KEY"), "the key is never an argument");
  assert.equal(readAscCredentials({ env, platform: "darwin", run: keychain.run }).keyId, KEY_ID);
});

test("store fails when the Keychain does not hold what was written, and refuses a non-P-256 key or non-macOS", () => {
  const dropped = { ...fakeKeychain(), run: () => ({ status: 0, stdout: "" }) };
  coded("IOS_ASC_STORE_FAILED", () => storeAscCredentials({ keyText: EC_PEM, keyId: KEY_ID, issuer: ISSUER, env, platform: "darwin", run: dropped.run }));
  coded("IOS_ASC_KEY_INVALID", () => storeAscCredentials({ keyText: RSA_PEM, keyId: KEY_ID, issuer: ISSUER, env, platform: "darwin", run: fakeKeychain().run }));
  coded("IOS_ASC_KEY_ID_INVALID", () => storeAscCredentials({ keyText: EC_PEM, keyId: "lower12345", issuer: ISSUER, env, platform: "darwin", run: fakeKeychain().run }));
  coded("IOS_ASC_ISSUER_INVALID", () => storeAscCredentials({ keyText: EC_PEM, keyId: KEY_ID, issuer: "not-a-uuid", env, platform: "darwin", run: fakeKeychain().run }));
  coded("IOS_ASC_HOST_UNSUPPORTED", () => storeAscCredentials({ keyText: EC_PEM, keyId: KEY_ID, issuer: ISSUER, env, platform: "linux", run: () => assert.fail("no command") }));
});

test("check reports identifiers and service names but never the key", () => {
  const found = checkAscCredentials({ env, platform: "darwin", run: seeded().run });
  assert.equal(found.keyId, KEY_ID);
  assert.ok(!JSON.stringify(found).includes(EC_DER_B64));
  assert.ok(!("privateKey" in found));
});
