import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';

// Static conformance only: pin native guards, extract their rules, then project
// every shared draft. No native compilation, key access, or authentication.
const read = path => readFileSync(new URL(path, import.meta.url), 'utf8');
const require = (ok, message) => assert.ok(ok, message);
const matches = (text, pattern) => new RegExp(pattern, 'i').test(text);
const count = (text, pattern) => [...text.matchAll(new RegExp(pattern, 'g'))].length;
function section(source, start, end) {
  const from = source.indexOf(start);
  const to = from < 0 ? -1 : source.indexOf(end, from + start.length);
  require(from >= 0 && to > from, 'missing source section: ' + start);
  return source.slice(from, to);
}
const quoted = text => [...text.matchAll(/"([^"]+)"/g)].map(match => match[1]);
const expectedKeys = ['repositoryIdentity', 'fingerprint', 'policyDigest', 'minimumScannerVersion', 'minimumCatalogVersion', 'occurrences', 'reason', 'owner', 'issued', 'expires', 'requestId', 'trackingRef'];
const expectedPatterns = {
  fingerprint: '^[a-z0-9][a-z0-9._-]{1,127}$', policyDigest: '^[a-f0-9]{64}$',
  floor: '^[0-9]+(?:\\.[0-9]+)*$', requestId: '^[A-Za-z0-9][A-Za-z0-9._:-]{2,127}$',
};
function nativeRules(platform, body, payloadKeys, fields) {
  const rules = { payloadKeys };
  require(payloadKeys.length === expectedKeys.length && expectedKeys.every(key => payloadKeys.includes(key)),
    platform + ': native payload keys must match v2 exactly');
  for (const [key, field] of Object.entries(fields)) {
    const escaped = field.replace(/[.*+?^{}()|[\]\\$]/g, '\\$&');
    const pattern = platform === 'macOS' ? escaped + '\\.range\\(of: "([^"]+)"'
      : 'RequirePattern\\(payload, ' + escaped + ', "([^"]+)"';
    const extracted = new RegExp(pattern).exec(body);
    require(extracted, platform + ': missing native pattern for ' + field);
    rules[key] = extracted[1].replaceAll('\\\\', '\\');
    assert.equal(rules[key], expectedPatterns[key], platform + ': native token pattern for ' + key + ' must match v2');
  }
  return rules;
}
function macRules() {
  // Overrides are test-only inputs for scratch-source mutation checks.
  const source = process.env.RIGHTKIT_CONFORMANCE_SWIFT_SOURCE === undefined
    ? read('../macos/ownership-approval.swift') : readFileSync(process.env.RIGHTKIT_CONFORMANCE_SWIFT_SOURCE, 'utf8');
  const entitlements = read('../macos/ownership-approval.entitlements');
  const buildScript = read('../macos/build.sh');
  const impostor = read('../macos/keychain-storage-impostor.swift');
  const infoPlist = read('../macos/Info.plist');
  const enrollRoot = section(source, "private func enrollRoot()", "private func enroll()");
  const enroll = section(source, "private func enroll()", "private func signLoadedDraft()");
  const signUi = section(source, "private func signLoadedDraft()", "private func showError(");
  const signEvent = section(source, "private func signKeyLog(", "private func exportKey(");
  const signedHelper = section(source, "private func signedKeyLog(", "private func requireEnrollmentPresence(");
  const presenceHelper = section(source, "private func requireEnrollmentPresence(", "private func jsQuote(");
  const keychainQuery = section(source, "private func keychainQuery(", "private func keychainPreflight(");
  const keychainPreflight = section(source, "private func keychainPreflight(", "private func keychainStorageSelfTest(");
  const storageProbe = section(source, "private func keychainStorageSelfTest(", "private func publicKeyExport(");
  const persistPublicKey = section(source, "private func persistPublicKey(", "private func loadExistingPublicKey(");
  require((count(enrollRoot, "authContext\\(reason")) === 1, "root mode must create one LAContext");
  require((count(enrollRoot, "SecureEnclave\\.P256\\.Signing\\.PrivateKey")) === 1, "root mode must create one Secure Enclave key");
  require((count(enrollRoot, "signedKeyLog\\(payload, with: key\\)")) === 1, "root mode must perform one genesis signature");
  require((count(enrollRoot, "requireEnrollmentPresence\\(")) === 0, "root mode must not add a second proof signature");
  require((count(enroll, "authContext\\(reason")) === 1, "enrollment must create one LAContext");
  require((count(enroll, "SecureEnclave\\.P256\\.Signing\\.PrivateKey")) === 1, "enrollment must create one Secure Enclave key");
  require((count(enroll, "requireEnrollmentPresence\\(privateKey\\)")) === 1, "enrollment must force one presence signature before storage/export");
  require((count(signUi, "signKeyLog\\(loaded, context: authContext\\(reason")) === 1, "key-log approval must pass one LAContext into signing");
  require((count(signUi, "let context = authContext\\(reason")) === 1, "legacy approval must create one LAContext");
  require((count(signEvent, "SecureEnclave\\.P256\\.Signing\\.PrivateKey\\(dataRepresentation: data, authenticationContext: context\\)")) === 1, "Secure Enclave restore must reuse operation LAContext");
  require((count(signEvent, "signedKeyLog\\(payload, with: key\\)")) === 1, "signing path must produce one event signature");
  require((count(signedHelper, "key\\.signature\\(for:")) === 1, "event helper must perform exactly one private-key operation");
  require((count(presenceHelper, "key\\.signature\\(for:")) === 1, "enrollment presence helper must perform exactly one private-key operation");
  require(!matches(source, "evaluatePolicy\\s*\\("), "do not add separate LA policy authentication");
  require(matches(source, "SecStaticCodeCheckValidity[\\s\\S]*kSecCSStrictValidate") && matches(source, "certificate leaf\\[subject\\.OU\\]") && matches(source, "1\\.2\\.840\\.113635\\.100\\.6\\.1\\.13") && matches(source, "6KLGD3LLKF"), "require strict valid Developer ID signature, app ID & expected team");
  require(matches(source, "Bundle\\.main\\.bundleIdentifier == ownershipBundleIdentifier"), "require stable current bundle ID");
  require(matches(source, "SecTrustedApplicationCreateFromPath\\(nil"), "trusted-app ACL must bind current signed helper");
  require(matches(source, "SecAccessCreate[\\s\\S]*\\[trusted\\] as CFArray"), "build legacy Keychain ACL from explicit trusted helper");
  require(matches(source, "SecKeychainCopyDefault[\\s\\S]*SecKeychainGetStatus"), "resolve & require unlocked default user Keychain");
  require(matches(source, "kSecMatchSearchList as String: \\[try defaultUserKeychain\\(\\)\\]"), "private key lookup must search only explicit default user Keychain");
  require(matches(source, "kSecUseKeychain as String: try defaultUserKeychain\\(\\)"), "Keychain adds must target explicit default user Keychain");
  require(!matches(source, "kSecUseDataProtectionKeychain|kSecAttrAccessGroup|ownershipKeychainGroup"), "do not use data-protection keychain or access groups");
  require(matches(source, "kSecAttrAccess as String: try signedHelperAccess\\(\\)"), "storage must use signed-helper legacy ACL");
  require(!matches(source, "kSecAttrAccessControl as String: try accessControl\\(privateKey: false\\)"), "do not use SecAccessControl on storage item");
  require(matches(source, "flags\\.insert\\(\\.privateKeyUsage\\)"), "Secure Enclave key must retain privateKeyUsage");
  require(matches(source, "SecAccessControlCreateWithFlags[\\s\\S]*flags"), "Secure Enclave key must retain userPresence access control");
  require(!matches(keychainQuery, "kSecUseAuthenticationContext"), "legacy storage lookup must not bind LAContext or suppress SE presence UI");
  require(matches(keychainPreflight, "requireSignedHelperIdentity\\(\\)[\\s\\S]*defaultUserKeychain\\(\\)"), "preflight must validate identity & Keychain before UI");
  require(matches(source, "SecKeychainSetUserInteractionAllowed\\(false\\)[\\s\\S]*SecKeychainSetUserInteractionAllowed\\(previous\\.boolValue\\)"), "scope Keychain interaction suppression & restore previous state");
  require(matches(storageProbe, "keychainPreflight\\(printPass: false\\)") && matches(storageProbe, "SecItemAdd") && matches(storageProbe, "SecItemCopyMatching") && matches(storageProbe, "runStorageProbe\\(impostorPath") && matches(storageProbe, "SecItemDelete") && matches(storageProbe, "errSecItemNotFound"), "storage probe must create/read/deny/delete & verify absence of only a sentinel");
  require(matches(storageProbe, "signedHelperAccess\\(\\)") && matches(storageProbe, "defaultUserKeychain\\(\\)"), "storage probe must reuse signed ACL & default Keychain");
  require(matches(storageProbe, "successorPath") && matches(storageProbe, "READ_ALLOWED"), "storage probe must support signed-helper update continuity");
  require(matches(storageProbe, "errSecInteractionNotAllowed") && matches(storageProbe, "errSecAuthFailed"), "storage probe must accept only known silent ACL denial statuses");
  require(matches(source, "private func keychainStorageReadProbe") && matches(source, "UUID sentinel coordinates") && matches(source, "SecKeychainOpen") && matches(source, "READ_ALLOWED"), "updated-helper read mode must accept UUID sentinel only & never print data");
  require(matches(source, "finished\\.wait\\(timeout: \\.now\\(\\) \\+ 10\\)"), "child probes must have a bounded timeout");
  require(matches(impostor, "SecKeychainSetUserInteractionAllowed\\(false\\)") && matches(impostor, "SecKeychainSetUserInteractionAllowed\\(previous\\.boolValue\\)"), "impostor probe must scope silent mode");
  require(matches(impostor, "SecItemCopyMatching"), "impostor probe must perform read-only sentinel lookup");
  require(matches(impostor, "SecKeychainGetStatus") && matches(impostor, "kSecUnlockStateStatus") && matches(impostor, "KEYCHAIN_UNLOCKED") && matches(impostor, "SecItemCopyMatching"), "impostor must prove explicit keychain is open & unlocked before sentinel query");
  require(matches(entitlements, "<dict\\s*/>"), "entitlements plist must be empty");
  require(!matches(entitlements, "keychain-access-groups|application-identifier|application-groups|team-identifier"), "do not require restricted entitlements");
  require(matches(infoPlist, "<key>CFBundleIdentifier</key>\\s*<string>com\\.orthiclabs\\.rightkit\\.ownership-approval</string>"), "app bundle ID must remain stable");
  require(matches(buildScript, "\\[\\[ \\$# -ne 2 \\]\\]"), "builder must accept exactly Node & app path");
  require(matches(buildScript, "swiftc -parse-as-library -O"), "native builder must compile on Mac host");
  require(matches(buildScript, "keychain-storage-impostor\\.swift"), "builder must include unrelated executable ACL probe");
  require(matches(buildScript, "signer=\"\\$release_tools/sign-macos\\.mjs\"[\\s\\S]*\"\\$bundled_node\" \"\\$signer\""), "native builder must use shared macOS signer");
  require(matches(buildScript, "--keychain-preflight[\\s\\S]*--keychain-storage-self-test[\\s\\S]*--crypto-self-test[\\s\\S]*--key-log-self-test"), "builder must require signed preflight, ACL probe & software checks");
  require(!matches(buildScript, "provisionprofile|security cms"), "builder must not require or embed provisioning profile");
  require(matches(buildScript, "Keychain preflight: PASS"), "builder must require affirmative preflight output");
  const draftBody = section(source, 'private func validateDraft(', 'private func accessControl(');
  const payloadKeys = quoted(section(source, 'private let payloadKeys', 'private enum ApprovalError'));
  for (const guard of [
    "schema.doubleValue == 2",
    "CFGetTypeID(schema) != CFBooleanGetTypeID()",
    "outer[\"signature\"] is NSNull",
    "Set(payload.keys) == Set(payloadKeys)",
    "requireString(payload, \"reason\", max: 4000)",
    "requireString(payload, \"owner\", max: 256)",
    "requireString(payload, \"repositoryIdentity\", max: 512)",
    "issuedDate <= Date(), date > issuedDate, date.timeIntervalSince(issuedDate) <= 90 * 86400",
    "ISO8601DateFormatter().string(from: issuedDate).hasPrefix(issued)",
    "ISO8601DateFormatter().string(from: date).hasPrefix(expiry)",
    "(1...256).contains(occurrences.count)",
    "Set(occurrence.keys) == Set([\"path\", \"kind\", \"hash\"])",
    "path.split",
    "path.contains",
    "[\"source\", \"dependency\"].contains(kind)",
    "hash.range(of: \"^[a-f0-9]{64}$\"",
    "pairs.insert([path, hash]).inserted",
    "fingerprint.range(of: \"^[a-f0-9]{64}$\", options: .regularExpression) == nil",
    "path.range(of: \"^[A-Za-z]:\", options: .regularExpression) == nil",
    "URLComponents(string: url)",
    "components.scheme == \"https\"",
    "components.user == nil, components.password == nil",
    "CFGetTypeID(verified) == CFBooleanGetTypeID(), verified.boolValue"
  ]) {
    require(draftBody.includes(guard), 'macOS: missing native v2 refusal guard: ' + guard);
  }
  require(count(draftBody, '== \\w+\\.startIndex\\.\\.<\\w+\\.endIndex') === 7, 'macOS: native regex matching must consume whole token');
  require(matches(signUi, '"schema": 2'), 'macOS: exception signing must emit schema 2');
  return nativeRules('macOS', draftBody, payloadKeys, { fingerprint: 'fingerprint', policyDigest: 'policyDigest', floor: 'value', requestId: 'request' });
}
function windowsRules() {
  const source = process.env.RIGHTKIT_CONFORMANCE_CS_SOURCE === undefined
    ? read('../windows/OwnershipApproval.cs') : readFileSync(process.env.RIGHTKIT_CONFORMANCE_CS_SOURCE, 'utf8');
  const project = read('../windows/OwnershipApproval.csproj');
  const manifest = read('../windows/app.manifest');
  require(matches(source, "private static int Main\\(string\\[\\] args\\)"), "entry point must parse native command modes");
  require(matches(source, "\"--enroll\"") && matches(source, "\"--public-key\"") && matches(source, "\"--key-operation\"") && matches(source, "\"--enroll-root\""), "public export, enrollment, key-operation, and root command modes are required");
  require(matches(source, "Path\\.IsPathFullyQualified\\(output\\)") && matches(source, "Path\\.IsPathFullyQualified\\(input\\)"), "commanded paths must be absolute");
  require(matches(source, "private async System\\.Threading\\.Tasks\\.Task StageKeyOperationAsync") && matches(source, "Sign reviewed key-log event"), "key-operation must stage typed event for explicit main-window signing action");
  require(matches(source, "private async System\\.Threading\\.Tasks\\.Task ExportPublicKeyToPathAsync"), "public key discovery command must export an existing key without enrollment");
  require(matches(source, "private async System\\.Threading\\.Tasks\\.Task EnrollRootAsync"), "root command must create genesis event in native helper");
  require(matches(source, "ValidateKeyLogPayload") && matches(source, "rightkit\\.ownership-key-log\\.v1"), "key-log payload schema/domain must be validated");
  require(matches(source, "signature\"] = null") && matches(source, "KeyCredentialCreationOption\\.FailIfExists"), "root event must be unsigned and credential creation must refuse replacement");
  require(matches(source, "signature:null") || matches(source, "draftObj\\[\"signature\"\\] is not null"), "key-operation must reject pre-signed drafts");
  require(matches(source, "AtomicWriteExclusive") && matches(source, "File\\.Move\\(temp, path\\)"), "command outputs must be atomically created without overwrite");
  require(matches(source, "private static string CanonicalJson\\(JsonNode\\? node\\)") && matches(source, "if \\(node is null\\) return \"null\";"), "canonical JSON must encode JSON null values");
  require(!matches(source, "Confirm ownership key-log event|Continue to the native key signature"), "key-log command must use main-window review/sign action without modal confirmation");
  require(matches(source, "Environment\\.ExitCode = 0") && matches(source, "Environment\\.ExitCode = 1"), "commanded enrollment must report success/failure via exit code");
  require(matches(source, "KeyCredentialManager\\.RequestCreateAsync") && matches(source, "KeyCredentialCreationOption\\.FailIfExists"), "new key-log credentials must use Windows Hello and refuse replacement");
  require(matches(source, "CryptographicPublicKeyBlobType\\.X509SubjectPublicKeyInfo") && matches(source, "KeyCredentialManager\\.OpenAsync"), "public key export must read Windows Hello SPKI without signing");
  require(matches(source, "credential\\.RequestSignAsync\\(writer\\.DetachBuffer\\(\\)\\)") && matches(source, "writer\\.WriteBytes\\(Encoding\\.UTF8\\.GetBytes\\(canonical\\)\\)"), "key-log signing must pass canonical payload bytes to one Windows Hello signature request");
  require(matches(source, "RSASignaturePadding\\.Pkcs1") && matches(source, "RSASignaturePadding\\.Pss"), "RSA signatures must be self-checked with accepted SHA-256 paddings");
  require(!matches(source, "ECDsaCng|CngKey|OpenApprovalKey|CreateApprovalKey|VerifyCurrentUserAsync|ForceHighProtection"), "legacy approval key & separate authentication paths must not be selectable");
  require(matches(source, "rsa\\.KeySize != 2048") && matches(source, "SHA256\\.HashData\\(rsa\\.ExportSubjectPublicKeyInfo\\(\\)\\)"), "Hello key must be RSA-2048 with SPKI fingerprint");
  require(matches(source, "PayloadDigest\\(current\\[\"payload\"\\]!\\)"), "review must show canonical payload digest");
  require(matches(source, "currentCanonical.*CanonicalJson\\(_draft\\[\"payload\"\\]!\\)") || matches(source, "CanonicalJson\\(_draft\\[\"payload\"\\]!\\)"), "signing must compare current payload to reviewed payload");
  const helloEnroll = section(source, "private async System.Threading.Tasks.Task EnrollKeyCredentialAsync", "private async System.Threading.Tasks.Task ExportPublicKeyToPathAsync");
  const helloRoot = section(source, "private async System.Threading.Tasks.Task EnrollRootAsync", "private async System.Threading.Tasks.Task EnrollKeyCredentialAsync");
  const helloRead = section(source, "private async System.Threading.Tasks.Task ExportPublicKeyToPathAsync", "private static KeyCredential RequireCredential");
  const helloSign = section(source, "private async System.Threading.Tasks.Task SignKeyLogCommandAsync", "private static void ValidateKeyLogPayload");
  const helloStage = section(source, "private async System.Threading.Tasks.Task StageKeyOperationAsync", "private void StageKeyLog");
  require(matches(helloStage, "KeyCredentialManager\\.OpenAsync\\(KeyLogCredentialName\\)") && matches(helloStage, "RequireCredential\\(opened,") && matches(helloStage, "PublicKeyFingerprint\\(PublicKeyPem\\(credential\\)\\)"), "staging must bind signer to same Hello RSA key used by signing");
  require((count(helloEnroll, "RequestCreateAsync\\(") === 1) && !matches(helloEnroll, "VerifyCurrentUserAsync|CreateApprovalKey|OpenApprovalKey|ForceHighProtection"), "enrollment must use one Windows Hello key-creation call only");
  require((count(helloRoot, "RequestCreateAsync\\(") === 1) && !matches(helloRoot, "VerifyCurrentUserAsync|CreateApprovalKey|OpenApprovalKey|SignHash"), "root command must create once and emit unsigned genesis without extra auth/key prompts");
  require(matches(helloRead, "KeyCredentialManager\\.OpenAsync") && !matches(helloRead, "RequestSignAsync|RequestCreateAsync|VerifyCurrentUserAsync"), "public key lookup must read existing Hello credential without authentication");
  require((count(helloSign, "RequestSignAsync\\(") === 1) && !matches(helloSign, "VerifyCurrentUserAsync|OpenApprovalKey|SignHash|ForceHighProtection"), "key operation must use one native Hello signing API path");
  require(!matches(source, "Environment\\.GetCommandLineArgs|CommandLine\\.Arguments|Console\\.ReadLine|GetEnvironmentVariable|\\b--sign\\b|--rotate"), "no arbitrary signing, rotation, or environment key path may exist");
  require(matches(project, "net10\\.0-windows10\\.0\\.22000\\.0") && matches(project, "<UseWindowsForms>true</UseWindowsForms>"), "project must target the supported interactive Windows API set");
  require(matches(project, "<PublishSingleFile>true</PublishSingleFile>") && matches(project, "<SelfContained>true</SelfContained>"), "package must be a self-contained single-file native-host build");
  require(matches(manifest, "requestedExecutionLevel level=\"asInvoker\""), "helper must not request elevation");
  const signBody = section(source, "private async System.Threading.Tasks.Task SignAsync()", "private void UpdateSignAvailability()");
  const confirmAt = signBody.indexOf("MessageBox.Show(this,");
  const signAt = signBody.indexOf(".RequestSignAsync(");
  require(confirmAt >= 0 && signAt > confirmAt && (count(signBody, "RequestSignAsync\\(") === 1), "signing order must be review, explicit confirmation, one Windows Hello signature");
  require(matches(signBody, "RequireCredential\\(hello,") && matches(signBody, "PublicKeyFingerprint\\(pem\\)"), "exception signing must refuse absent Hello key & use its RSA fingerprint");
  const draftBody = section(source, 'private static void ValidateDraft(', 'private static void RequirePattern(');
  const keys = /var payloadKeys = new\[\] \{([^}]+)\}/.exec(draftBody);
  require(keys, 'Windows: missing native payloadKeys declaration');
  const payloadKeys = quoted(keys[1]);
  for (const guard of [
    "root[\"schema\"]?.GetValue<int>() != 2",
    "root[\"signature\"] is not null",
    "SetEquals(payloadKeys)",
    "RequireText(payload, \"reason\", 4000)",
    "RequireText(payload, \"owner\", 256)",
    "RequireText(payload, \"repositoryIdentity\", 512)",
    "issued > DateTime.UtcNow.Date || expiry <= issued || (expiry - issued).TotalDays > 90",
    "DateTime.TryParseExact(payload[\"issued\"]",
    "DateTime.TryParseExact(payload[\"expires\"]",
    "occurrences.Count is < 1 or > 256",
    "SetEquals(new[] { \"path\", \"kind\", \"hash\" })",
    "path.Split",
    "path.Contains",
    "is not (\"source\" or \"dependency\")",
    "RequirePattern(occurrence, \"hash\", \"^[a-f0-9]{64}$\", 64)",
    "!pairs.Add((path, occurrence[\"hash\"]!.GetValue<string>()))",
    "Regex.IsMatch(payload[\"fingerprint\"]!.GetValue<string>(), \"^[a-f0-9]{64}$\"",
    "\"^[A-Za-z]:|",
    "\"^https://[^/?#]*@\"",
    "!Uri.TryCreate(url, UriKind.Absolute",
    "uri.Scheme != Uri.UriSchemeHttps",
    "!string.IsNullOrEmpty(uri.UserInfo)",
    "tracking[\"verified\"]?.GetValue<bool>() != true"
  ]) {
    require(draftBody.includes(guard), 'Windows: missing native v2 refusal guard: ' + guard);
  }
  require(source.includes('!match.Success || match.Length != text.Length'), 'Windows: native regex matching must consume whole token');
  return nativeRules('Windows', draftBody, payloadKeys, { fingerprint: '"fingerprint"', policyDigest: '"policyDigest"', floor: 'floor', requestId: '"requestId"' });
}
function exactKeys(value, keys) {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    && Object.keys(value).length === keys.length && keys.every(key => Object.hasOwn(value, key));
}
function textField(value, key, limit = 4096) {
  return typeof value[key] === 'string' && value[key].trim().length > 0 && value[key].length <= limit;
}
function fullMatch(value, pattern) {
  if (typeof value !== 'string') return false;
  const match = new RegExp(pattern).exec(value);
  return match !== null && match.index === 0 && match[0].length === value.length;
}
function dateValue(value) {
  if (!fullMatch(value, '^[0-9]{4}-[0-9]{2}-[0-9]{2}$') || value.startsWith('0000-')) return NaN;
  const time = Date.parse(value + 'T00:00:00Z');
  return Number.isFinite(time) && new Date(time).toISOString().slice(0, 10) === value ? time : NaN;
}
// Same source-projected refusal rules formerly duplicated in both PS scripts.
// Return a specific refusal so unexpected valid-vector failures explain why.
function draftRefusal(record, rules) {
  if (!exactKeys(record, ['schema', 'payload', 'signature']) || record.schema !== 2 || record.signature !== null) return 'expected unsigned schema-2 envelope';
  const p = record.payload;
  if (!exactKeys(p, rules.payloadKeys)) return 'payload keys must match v2 exactly';
  for (const [key, limit] of [['repositoryIdentity', 512], ['reason', 4000], ['owner', 256]]) {
    if (!textField(p, key, limit)) return 'missing or invalid ' + key;
  }
  if (!fullMatch(p.fingerprint, rules.fingerprint) || fullMatch(p.fingerprint, rules.policyDigest)) return 'invalid fingerprint';
  if (!fullMatch(p.policyDigest, rules.policyDigest)) return 'invalid policyDigest';
  if (!fullMatch(p.requestId, rules.requestId)) return 'invalid requestId';
  for (const key of ['minimumScannerVersion', 'minimumCatalogVersion']) {
    if (!textField(p, key, 2147483647) || !fullMatch(p[key], rules.floor)) return 'invalid ' + key;
  }
  const issued = dateValue(p.issued), expires = dateValue(p.expires);
  if (!Number.isFinite(issued) || !Number.isFinite(expires)) return 'invalid issued or expires date';
  const today = dateValue(new Date().toISOString().slice(0, 10));
  if (issued > today || expires <= issued || expires - issued > 90 * 86400000) return 'invalid issued/expiry window';
  if (!Array.isArray(p.occurrences) || p.occurrences.length < 1 || p.occurrences.length > 256) return 'occurrences must contain 1 to 256 objects';
  const pairs = new Set();
  for (const o of p.occurrences) {
    if (!exactKeys(o, ['path', 'kind', 'hash']) || typeof o.path !== 'string' || o.path.length === 0
      || !['source', 'dependency'].includes(o.kind) || !fullMatch(o.hash, rules.policyDigest)) return 'invalid occurrence shape';
    if (o.path.startsWith('/') || o.path.includes('\\') || /^[A-Za-z]:|[*?\[\]{}]/.test(o.path)
      || o.path.split('/').some(part => ['', '.', '..'].includes(part))) return 'invalid repository-relative occurrence path';
    const pair = JSON.stringify([o.path, o.hash]);
    if (pairs.has(pair)) return 'duplicate occurrence path/hash pair';
    pairs.add(pair);
  }
  const tracking = p.trackingRef;
  if (!exactKeys(tracking, ['id', 'url', 'verified']) || typeof tracking.id !== 'string' || tracking.id.length === 0
    || tracking.verified !== true || typeof tracking.url !== 'string') return 'invalid trackingRef shape';
  try {
    const url = new URL(tracking.url);
    if (url.protocol !== 'https:' || !url.hostname || url.username || url.password || /^https:\/\/[^/?#]*@/i.test(tracking.url)) return 'trackingRef must be HTTPS without userinfo';
  } catch { return 'invalid trackingRef URL'; }
  return null;
}
async function projectVectors(t, platform, rules) {
  const vectors = JSON.parse(read('./v2-vectors.json'));
  require(vectors.schema === 2 && Array.isArray(vectors.valid) && Array.isArray(vectors.invalid)
    && vectors.valid.length > 0 && vectors.invalid.length > 0, 'shared vectors must contain valid & invalid v2 records');
  for (const [group, accepted] of [['valid', true], ['invalid', false]]) {
    for (const vector of vectors[group]) {
      require(typeof vector?.name === 'string' && vector.name.length > 0, platform + ': missing vector id in ' + group);
      await t.test(platform + ': ' + group + ' v2 vector ' + vector.name, () => {
        const refusal = draftRefusal(vector.record, rules);
        if (accepted) assert.equal(refusal, null, platform + ': valid v2 vector refused: ' + vector.name + ' (' + refusal + ')');
        else assert.notEqual(refusal, null, platform + ': invalid v2 vector accepted: ' + vector.name);
      });
    }
  }
  t.diagnostic(platform + ': ' + vectors.valid.length + ' valid accepted; ' + vectors.invalid.length + ' invalid refused');
}
test('macOS native source conformance', async t => { await projectVectors(t, 'macOS', macRules()); });
test('Windows native source conformance', async t => { await projectVectors(t, 'Windows', windowsRules()); });
