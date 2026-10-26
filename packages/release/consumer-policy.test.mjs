// Consumer semantic policy (Class A row 13). The negatives are SellRight's own historical failures, from verbatim excerpts of the
// commits named in each test (Orthic-Labs/sellright); each repair and each legitimate exception is the commit that followed.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { test } from "node:test";
import { main, RULES } from "./app-check.mjs";
import { blankFragment, CONSUMER_POLICY_FILE, CONSUMER_POLICY_RULES, irreversible, matchGlob, moduleSpecifiers, resolveSpecifier, routeKey } from "./consumer-policy.mjs";

const IDS = ["retired-token", "route-deny-policy", "layer-reach", "spec-route-parity"];

function tree(files) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), "rk-consumer-policy-"));
  for (const [rel, text] of Object.entries(files)) {
    fs.mkdirSync(path.dirname(path.join(root, rel)), { recursive: true });
    fs.writeFileSync(path.join(root, rel), typeof text === "string" ? text : JSON.stringify(text, null, 2));
  }
  return { root, files: Object.keys(files), done: () => fs.rmSync(root, { recursive: true, force: true }) };
}
const run = (id, files) => {
  const t = tree(files);
  try { return CONSUMER_POLICY_RULES.find((r) => r.id === id).run({ root: t.root, files: t.files }); } finally { t.done(); }
};
const problems = (found) => found.map((f) => f.problem);

// ---- registry ------------------------------------------------------------------------------------------------------------------

test("the four CA-13 rules run after Legion's semantic contracts, before source-closure and the ownership rules, and an app without a declaration has nothing to check", () => {
  assert.deepEqual(CONSUMER_POLICY_RULES.map((r) => `${r.id}:${r.gap}`), IDS.map((id) => `${id}:CA-13`));
  assert.ok(Object.isFrozen(CONSUMER_POLICY_RULES));
  assert.deepEqual(RULES.slice(-7, -3).map((r) => r.id), IDS);
  for (const id of IDS) assert.deepEqual(run(id, { "src/a.ts": "export const vendure = 1;\n" }), []);
});

test("a declaration that is not JSON, not schema 1 or has a malformed section is a finding, never a throw or a pass", () => {
  for (const id of IDS) {
    assert.match(problems(run(id, { [CONSUMER_POLICY_FILE]: "{ nope" }))[0], /is not JSON/);
    assert.match(problems(run(id, { [CONSUMER_POLICY_FILE]: { schema: 2 } }))[0], /this check reads schema 1/);
  }
  assert.match(problems(run("retired-token", { [CONSUMER_POLICY_FILE]: { schema: 1, retiredTokens: {} } }))[0], /retiredTokens is not an array/);
  assert.match(problems(run("retired-token", { [CONSUMER_POLICY_FILE]: { schema: 1, retiredTokens: [{ id: "x", tokens: [], include: ["**"] }] } }))[0], /tokens must be a non-empty list/);
  assert.match(problems(run("retired-token", { [CONSUMER_POLICY_FILE]: { schema: 1, retiredTokens: [{ id: "x", tokens: ["v"], include: ["**"], allowFiles: [{ file: "a.ts" }] }] } }))[0], /needs a file and a reason/);
  assert.match(problems(run("route-deny-policy", { [CONSUMER_POLICY_FILE]: { schema: 1, routeDenyPolicies: [{ id: "x", prefix: "/v1/", routes: "missing.json", policy: "p.json" }] } }))[0], /is not a file of this commit/);
  assert.match(problems(run("layer-reach", { [CONSUMER_POLICY_FILE]: { schema: 1, layerBoundaries: [{ id: "x", root: "../up", from: { include: ["**"] }, forbid: { include: ["**"] } }] } }))[0], /root must be a relative directory/);
  assert.match(problems(run("spec-route-parity", { [CONSUMER_POLICY_FILE]: { schema: 1, specRouteParity: [{ id: "x", spec: "s.json", routes: "r.json" }], }, "r.json": { routes: ["GET /a"] } }))[0], /the spec "s.json" is not a file/);
});

// ---- retired-token: scripts/assert-no-vendure.sh and its compatibility scanner, as data ------------------------------------------

const SCOPE = ["packages/api/src/**", "packages/storefront-client/**", "packages/storefront/src/**"];
const vendurePolicy = ({ allowFiles = [], allowFragments = [] } = {}) => ({
  schema: 1,
  retiredTokens: [
    { id: "vendure-coupling", tokens: ["graphql", "gql`", "@vendure", "__typename"], include: SCOPE, exclude: ["packages/api/src/import/**", "**/node_modules/**"], extensions: [".ts", ".tsx", ".js", ".mjs"] },
    { id: "vendure-word", tokens: ["vendure"], caseInsensitive: true, include: SCOPE, exclude: ["packages/api/src/import/**", "**/node_modules/**"], extensions: [".ts", ".tsx", ".js", ".mjs"], allowFiles, allowFragments },
  ],
});
// packages/storefront/src/sellright/types/catalog.ts:221 at 8e31c0e (red) and c3f83a3 (the repair)
const CATALOG_RED = "export function normalizeManifestProductDetail(raw: RawManifestProductDetail): CatalogProduct {\n  // Featured first, then the gallery. The featured asset is normally ALSO a gallery row (Vendure's convention, kept\n  // 1:1 by the import), so the list is deduped on the resolved path: without this every product with a gallery showed\n";
const CATALOG_GREEN = CATALOG_RED.replace("(Vendure's convention, kept", "(the source catalog's convention, kept");

test("retired-token: SellRight 8e31c0e (a Vendure comment in the storefront) is red; c3f83a3's rewording is green", () => {
  const red = run("retired-token", { [CONSUMER_POLICY_FILE]: vendurePolicy(), "packages/storefront/src/sellright/types/catalog.ts": CATALOG_RED });
  assert.deepEqual(problems(red), ['packages/storefront/src/sellright/types/catalog.ts:2 contains the retired token "vendure" (.rightkit/consumer-policy.json retiredTokens vendure-word)']);
  assert.equal(red[0].subject, "vendure-word:vendure");
  assert.deepEqual(run("retired-token", { [CONSUMER_POLICY_FILE]: vendurePolicy(), "packages/storefront/src/sellright/types/catalog.ts": CATALOG_GREEN }), []);
});

// packages/api/src/sdk/authorization-matrix.cart-cases.testkit.ts:41 at 70c071a; 520907d adds it to VENDURE_WORD_ALLOWLIST
const TESTKIT = "    await rows('UPDATE product_variant SET price=2500,sale_price=NULL,is_pre_order=false,pre_order_price=NULL,fulfillment_type=$3,enabled=true,deleted_at=NULL,metafields=$2::jsonb WHERE id=$1', [product.variantId, JSON.stringify({ vendureId: legacyId }), 'physical']);\n";
test("retired-token: a whole-file exception (520907d, approved 2026-10-20) is honoured; 70c071a without it is red", () => {
  const file = "packages/api/src/sdk/authorization-matrix.cart-cases.testkit.ts";
  assert.equal(run("retired-token", { [CONSUMER_POLICY_FILE]: vendurePolicy(), [file]: TESTKIT }).length, 1);
  assert.deepEqual(run("retired-token", { [CONSUMER_POLICY_FILE]: vendurePolicy({ allowFiles: [{ file, reason: "legacy-cart metafield key routes/cart.ts reads" }] }), [file]: TESTKIT }), []);
});

// packages/api/src/auth/cookies.ts:154-161 at 7ea10ea (red, PR 247, CI 38872962333 "legacy compatibility lint") and 1b6c0d3, whose
// scripts/assert-no-gql-tags.mjs COMPATIBILITY entry excepts exactly these three spellings
const COOKIES = "/** Vendure carriers are untrusted transport, consumed only by migration bootstrap. */\nexport function legacyCustomerToken(c: { req: { header: (k: string) => string | undefined } }): string | null {\n  const headers = parseCookies(c.req.header('cookie'));\n  const bearer = c.req.header('x-vendure-session-token');\n  if (bearer && bearer.length <= 2048 && /^[A-Za-z0-9_-]+$/.test(bearer)) return bearer;\n  const readable = headers.authToken;\n  if (readable && readable.length <= 2048 && /^[A-Za-z0-9_-]+$/.test(readable)) return readable;\n  const wrapped = headers.__vendure_session;\n";
const COOKIE_FRAGMENTS = [
  { file: "packages/api/src/auth/cookies.ts", text: "__vendure_session", kind: "identifier", reason: "legacy session carrier" },
  { file: "packages/api/src/auth/cookies.ts", text: "x-vendure-session-token", kind: "string", reason: "legacy bearer header" },
  { file: "packages/api/src/auth/cookies.ts", text: "/** Vendure carriers are untrusted transport, consumed only by migration bootstrap. */", reason: "migration note" },
];
test("retired-token: exact-fragment exceptions (1b6c0d3) clear PR 247's legacy carriers; without them (7ea10ea) the first mention is red", () => {
  const file = "packages/api/src/auth/cookies.ts";
  assert.deepEqual(problems(run("retired-token", { [CONSUMER_POLICY_FILE]: vendurePolicy(), [file]: COOKIES })), [`${file}:1 contains the retired token "vendure" (.rightkit/consumer-policy.json retiredTokens vendure-word)`]);
  assert.deepEqual(run("retired-token", { [CONSUMER_POLICY_FILE]: vendurePolicy({ allowFragments: COOKIE_FRAGMENTS }), [file]: COOKIES }), []);
  // an exception is exact: a new mention elsewhere in the same file still fails, on its own line
  const grown = `${COOKIES}  // TODO drop the vendure fallback\n`;
  assert.deepEqual(problems(run("retired-token", { [CONSUMER_POLICY_FILE]: vendurePolicy({ allowFragments: COOKIE_FRAGMENTS }), [file]: grown })), [`${file}:9 contains the retired token "vendure" (.rightkit/consumer-policy.json retiredTokens vendure-word)`]);
});

test("retired-token: fragment kinds, case, scope and the hard coupling tokens", () => {
  assert.equal(blankFragment("a vendureMethod b", { text: "vendure", kind: "identifier" }), "a vendureMethod b", "an identifier is not part of a longer one");
  assert.equal(blankFragment("x('vendure:x') // vendure:x", { text: "vendure:x", kind: "string" }), "x('         ') // vendure:x", "a string is the whole quoted body");
  assert.equal(blankFragment("ven/*k*/dure", { text: "/*k*/" }), "ven     dure", "blanking never joins two halves into a token");
  const files = (body) => ({ [CONSUMER_POLICY_FILE]: vendurePolicy(), "packages/api/src/import/source.ts": "import { gql } from '@vendure/core';\n", "packages/api/src/x.md": "Vendure", "packages/storefront-client/src/q.ts": body });
  assert.deepEqual(run("retired-token", files("export const ok = 1;\n")), [], "the importer and non-code files are out of scope");
  assert.deepEqual(problems(run("retired-token", files("const q = gql`{ product }`;\n"))), ['packages/storefront-client/src/q.ts:1 contains the retired token "gql`" (.rightkit/consumer-policy.json retiredTokens vendure-coupling)']);
  assert.deepEqual(run("retired-token", files("const GraphQLish = 1;\n")), [], "the coupling tokens are case-sensitive, as the guard's grep -E was");
  assert.equal(run("retired-token", files("// VENDURE\n")).length, 1, "the bare word is not");
});

// ---- route-deny-policy: packages/api/scripts/assert-agent-boundary.mjs over the committed route table -----------------------------

// IRREVERSIBLE_PATH at 13ddad3 (packages/api/src/auth/agent-policy.ts:69), as data: `[/-](a|b|...)(\/|$)` is segments, the rest fragments
const SEGMENTS_13DDAD3 = ["passkeys", "cancel", "purge", "refund", "revoke", "settle", "releases", "licenses", "staff", "agents", "2fa", "step-up", "override-release", "approve", "retry", "reconcile", "verify", "secret", "token", "mode", "google", "adjust", "reverse", "password", "webhooks"];
const FRAGMENTS = ["/payments/settings", "/settings/security", "/email/settings", "/system/", "/payment-reconciliation/", "/sheerid/config", "/marketing/config", "verification/clear"];
const ROUTES = { routes: ["DELETE /v1/admin/products/{id}", "GET /v1/admin/settings/apple", "PATCH /v1/admin/settings/apple", "PATCH /v1/admin/settings/google", "POST /v1/admin/reviews/{id}/approve", "POST /v1/admin/orders/{code}/cancel", "PATCH /v1/admin/products/{id}", "GET /v1/shop/products"], undocumented: [] };
const denyFiles = ({ denied, exceptions = ["POST /v1/admin/reviews/:id/approve"], segments = SEGMENTS_13DDAD3, routes = ROUTES }) => ({
  [CONSUMER_POLICY_FILE]: { schema: 1, routeDenyPolicies: [{ id: "agent-deny", routes: "packages/api/src/sdk/route-inventory.golden.json", policy: "packages/api/src/auth/agent-policy.json", prefix: "/v1/admin/" }] },
  "packages/api/src/sdk/route-inventory.golden.json": routes,
  "packages/api/src/auth/agent-policy.json": { denied, exceptions, irreversible: { segments, fragments: FRAGMENTS } },
});
const DENIED = ["DELETE /v1/admin/products/:id", "POST /v1/admin/orders/:code/cancel", "PATCH /v1/admin/settings/google", "PATCH /v1/admin/settings/apple"];

test("route-deny-policy: SellRight 13ddad3 denies the Apple settings route its marks do not cover (red); 0c47fc3 adds `apple` (green)", () => {
  assert.deepEqual(problems(run("route-deny-policy", denyFiles({ denied: DENIED }))), ["packages/api/src/auth/agent-policy.json: the denied route PATCH /v1/admin/settings/apple is not covered by the irreversible marks: its removal from denied would go unnoticed (.rightkit/consumer-policy.json routeDenyPolicies agent-deny)"]);
  assert.deepEqual(run("route-deny-policy", denyFiles({ denied: DENIED, segments: [...SEGMENTS_13DDAD3, "apple"] })), []);
});

test("route-deny-policy: b85815d's handoff authorize route is red until 2946e05 adds `handoff`", () => {
  const routes = { routes: [...ROUTES.routes, "POST /v1/admin/handoff/authorize"] };
  const denied = [...DENIED, "POST /v1/admin/handoff/authorize"];
  const segments = [...SEGMENTS_13DDAD3, "apple"];
  assert.deepEqual(run("route-deny-policy", denyFiles({ denied, segments, routes })).map((f) => f.subject), ["agent-deny:denied-not-covered:POST /v1/admin/handoff/authorize"]);
  assert.deepEqual(run("route-deny-policy", denyFiles({ denied, segments: [...segments, "handoff"], routes })), []);
});

test("route-deny-policy: the reviewed exceptions stay allowed, and each other invariant of the deny set has its own finding", () => {
  const segments = [...SEGMENTS_13DDAD3, "apple"];
  assert.deepEqual(run("route-deny-policy", denyFiles({ denied: DENIED, segments })), [], "review approval is excepted: reversible, not denied");
  const subjects = (opts) => run("route-deny-policy", denyFiles({ segments, ...opts })).map((f) => f.subject);
  assert.deepEqual(subjects({ denied: DENIED, exceptions: [] }), ["agent-deny:irreversible-not-denied:POST /v1/admin/reviews/{id}/approve"]);
  assert.deepEqual(subjects({ denied: DENIED.filter((k) => !k.startsWith("DELETE")) }), ["agent-deny:delete-not-denied:DELETE /v1/admin/products/{id}"]);
  assert.deepEqual(subjects({ denied: [...DENIED, "POST /v1/admin/orders/:code/refund"] }), ["agent-deny:not-a-route:POST /v1/admin/orders/{code}/refund"]);
  assert.deepEqual(subjects({ denied: DENIED, exceptions: ["POST /v1/admin/reviews/:id/approve", "PATCH /v1/admin/settings/apple"] }), ["agent-deny:denied-and-excepted:PATCH /v1/admin/settings/apple"]);
  assert.deepEqual(subjects({ denied: DENIED, exceptions: ["POST /v1/admin/reviews/:id/approve", "POST /v1/admin/gone"] }), ["agent-deny:exception-not-a-route:POST /v1/admin/gone"]);
});

test("route-deny-policy: an OpenAPI document is a route source, joined with the table's other routes", () => {
  const files = denyFiles({ denied: DENIED, segments: [...SEGMENTS_13DDAD3, "apple"], routes: { routes: ["GET /v1/shop/products"] } });
  files["openapi.json"] = { openapi: "3.0.0", paths: { "/v1/admin/products/{id}": { delete: {}, patch: {} }, "/v1/admin/settings/apple": { patch: {} }, "/v1/admin/settings/google": { patch: {} }, "/v1/admin/orders/{code}/cancel": { post: {} }, "/v1/admin/reviews/{id}/approve": { post: {} } } };
  files[CONSUMER_POLICY_FILE].routeDenyPolicies[0].routes = ["openapi.json", "packages/api/src/sdk/route-inventory.golden.json"];
  assert.deepEqual(run("route-deny-policy", files), []);
  assert.equal(irreversible({ segments: ["mode"] }, "/v1/admin/settings/payments/stripe-mode"), true, "'-' delimits a segment, as the regex's [/-] did");
  assert.equal(irreversible({ segments: ["token"] }, "/v1/admin/devices/{token}"), false, "a path parameter is not a segment");
  assert.equal(routeKey("PATCH /v1/a/:id{[0-9]+}/b"), "PATCH /v1/a/{id}/b");
});

// ---- spec-route-parity: the committed OpenAPI spec against the committed route table -------------------------------------------

const AI = "POST /v1/admin/payment-reconciliation/effects/{id}/approve-issuance";
const parityFiles = (paths, routes = ["GET /v1/shop/products", AI, "GET /.well-known/apple-app-site-association"], undocumented = ["GET /.well-known/apple-app-site-association"]) => ({
  [CONSUMER_POLICY_FILE]: { schema: 1, specRouteParity: [{ id: "storefront-client-openapi", spec: "packages/storefront-client/openapi.json", routes: "packages/api/src/sdk/route-inventory.golden.json" }] },
  "packages/api/src/sdk/route-inventory.golden.json": { count: routes.length, routes, undocumented },
  "packages/storefront-client/openapi.json": { openapi: "3.0.0", paths },
});

test("spec-route-parity: SellRight e0dc46b adds approve-issuance to the route table but not the spec (red); bdee6d5 regenerates (green)", () => {
  const before = { "/v1/shop/products": { get: {} } };
  assert.deepEqual(problems(run("spec-route-parity", parityFiles(before))), [`packages/storefront-client/openapi.json has no operation for the documented route ${AI} in packages/api/src/sdk/route-inventory.golden.json: the committed spec is older than the routes (.rightkit/consumer-policy.json specRouteParity storefront-client-openapi)`]);
  assert.deepEqual(run("spec-route-parity", parityFiles({ ...before, "/v1/admin/payment-reconciliation/effects/{id}/approve-issuance": { post: {}, parameters: [] } })), []);
});

test("spec-route-parity: declared undocumented routes stay out of the spec; a contradiction or a stray operation is red", () => {
  const full = { "/v1/shop/products": { get: {} }, "/v1/admin/payment-reconciliation/effects/{id}/approve-issuance": { post: {} } };
  const subjects = (paths, routes, undocumented) => run("spec-route-parity", parityFiles(paths, routes, undocumented)).map((f) => f.subject);
  assert.deepEqual(subjects({ ...full, "/.well-known/apple-app-site-association": { get: {} } }), ["storefront-client-openapi:undocumented-in-spec:GET /.well-known/apple-app-site-association"]);
  assert.deepEqual(subjects({ ...full, "/v1/admin/system/canary": { post: {} } }), ["storefront-client-openapi:not-a-route:POST /v1/admin/system/canary"], "c52d810 / 57a3824: the spec documents canary, the table lacks it");
  assert.deepEqual(subjects(full, undefined, ["GET /.well-known/apple-app-site-association", "GET /gone"]), ["storefront-client-openapi:undocumented-not-a-route:GET /gone"]);
});

// ---- layer-reach: check-imports.mjs import-l1-transitive over declared globs and lexically read imports ------------------------

const LAYER_POLICY = {
  schema: 1,
  layerBoundaries: [{
    id: "storefront-l2", root: "packages/storefront", aliases: { "~/": "src/" },
    from: { include: ["src/components/**"], exclude: ["src/theme/**", "**/*.test.*"] },
    forbid: { include: ["src/utils/**", "src/config/**", "src/theme/**", "src/platform/**", "src/contexts/**"], exclude: ["src/utils/image-cache.ts", "src/config/**", "src/theme/**"] },
    stop: ["src/platform/views.ts"],
  }],
};
// packages/storefront at a61d0f5: an L2 component reaches theme.config.ts (pure, allowed), which imports src/utils/shop-categories.ts
const layerFiles = (from = "~/utils/shop-categories") => ({
  [CONSUMER_POLICY_FILE]: LAYER_POLICY,
  "packages/storefront/src/components/home/HomeSocialSections.tsx": "import { component$ } from '@qwik.dev/core';\nimport { VerificationButton } from '~/platform/views';\nimport { theme, policySentence, type StoreTheme } from '~/theme/theme.config';\n",
  "packages/storefront/src/theme/theme.config.ts": `import { projectShopCategories } from '${from}';\n\nexport function identityFromStaticTheme(): import('~/utils/sellright').SrStoreIdentity {\n\treturn null!;\n}\n`,
  "packages/storefront/src/utils/shop-categories.ts": "import type { FacetWithValues } from '~/types';\n",
  "packages/storefront/src/config/shop-categories.ts": "import type { FacetWithValues } from '~/types';\n",
  "packages/storefront/src/utils/sellright.ts": "export type SrStoreIdentity = {};\n",
  "packages/storefront/src/platform/views.ts": "import { TurnstileMount } from '~/platform/widgets/TurnstileMount';\n",
  "packages/storefront/src/platform/widgets/TurnstileMount.tsx": "import { useContext } from '~/contexts/app';\n",
});

test("layer-reach: SellRight a61d0f5 reaches src/utils/shop-categories.ts through theme.config.ts (red); a50f3dc moves it to src/config (green)", () => {
  assert.deepEqual(problems(run("layer-reach", layerFiles())), ["packages/storefront/src/theme/theme.config.ts:1 '~/utils/shop-categories': src/components/home/HomeSocialSections.tsx -> src/theme/theme.config.ts -> src/utils/shop-categories.ts reaches a file the layer boundary forbids (.rightkit/consumer-policy.json layerBoundaries storefront-l2)"]);
  assert.deepEqual(run("layer-reach", layerFiles("~/config/shop-categories")), [], "a reviewed pure layer, a public API not walked, and a type-position import");
});

test("layer-reach: type-only imports are not edges; a load the lexer cannot read is a finding; relative and index resolution", () => {
  const files = layerFiles("~/config/shop-categories");
  files["packages/storefront/src/components/a/A.tsx"] = "import type { X } from '~/contexts/app';\nimport { type Y } from '../../contexts/app';\nconst s = \"import x from '~/contexts/app'\"; // import z from '~/contexts/app'\nexport const C = () => <p>Don't import from '~/contexts/app'</p>;\nimport { B } from './b';\n";
  files["packages/storefront/src/components/a/b/index.ts"] = "export const B = await import(name);\n";
  files["packages/storefront/src/contexts/app.ts"] = "export type X = 1;\n";
  assert.deepEqual(run("layer-reach", files).map((f) => f.subject), ["storefront-l2:unreadable:src/components/a/b/index.ts"]);
  files["packages/storefront/src/components/a/b/index.ts"] = "export const B = (await import('../../../contexts/app.js')).value;\n";
  assert.deepEqual(run("layer-reach", files).map((f) => f.subject), ["storefront-l2:src/components/a/A.tsx->src/contexts/app.ts", "storefront-l2:src/components/a/b/index.ts->src/contexts/app.ts"]);
});

test("the lexer and resolver: what is an import, and where it lands", () => {
  const specs = moduleSpecifiers("export * from './e';\nexport type { F } from './f';\nexport { g } from \"./g\";\nexport const h = 1;\nconst r = /import('x')/g;\nimport.meta.url;\nrequire('./r');\nx.import('./no');\n");
  assert.deepEqual(specs.map((s) => `${s.via}:${s.spec}:${s.typeOnly}`), ["export:./e:false", "export:./f:true", "export:./g:false", "require:./r:false"]);
  const fileSet = new Set(["p/src/a.ts", "p/src/b/index.tsx", "p/src/c.tsx"]);
  const ctx = { root: "p", aliases: { "~/": "src/" }, fileSet, extensions: [".ts", ".tsx"] };
  assert.equal(resolveSpecifier("~/a", "p/src/x.ts", ctx), "p/src/a.ts");
  assert.equal(resolveSpecifier("./b", "p/src/x.ts", ctx), "p/src/b/index.tsx");
  assert.equal(resolveSpecifier("./c.js", "p/src/x.ts", ctx), "p/src/c.tsx");
  assert.equal(resolveSpecifier("@qwik.dev/core", "p/src/x.ts", ctx), null);
  assert.equal(matchGlob("src/**", "src/a/b.ts"), true);
  assert.equal(matchGlob("src/*.ts", "src/a/b.ts"), false);
  assert.equal(matchGlob("src/routes/[...slug]/index.tsx", "src/routes/[...slug]/index.tsx"), true);
});

test("layer-reach: a load inside a template hole or a value-position typeof import() is an edge; type positions and plain templates are not", () => {
  const edges = (src) => moduleSpecifiers(src).filter((s) => !s.typeOnly).map((s) => s.spec);
  assert.deepEqual(edges("const x = `${require('./forbidden')}`;"), ["./forbidden"], "a require in a template hole");
  assert.deepEqual(edges("const x = `${`${import('./forbidden')}`}`;"), ["./forbidden"], "a load in a nested hole");
  assert.deepEqual(edges("const x = typeof import('./forbidden');"), ["./forbidden"], "typeof of a value starts the load");
  assert.deepEqual(edges("const x = c ? a : typeof import('./forbidden');"), ["./forbidden"]);
  assert.deepEqual(edges("const y = import('./forbidden').default;"), ["./forbidden"]);
  for (const typeOnly of ["import type { X } from './x';", "type T = typeof import('./x');", "let v: typeof import('./x');", "export type T = { a: typeof import('./x') };", "export function f(): import('./x').T { return null!; }"]) assert.deepEqual(edges(typeOnly), [], typeOnly);
  assert.deepEqual(moduleSpecifiers("const z = `plain ${a + b} text`;"), [], "a template with no call");
  const files = (body) => ({ [CONSUMER_POLICY_FILE]: { schema: 1, layerBoundaries: [{ id: "l2", root: "app", aliases: { "~/": "src/" }, from: { include: ["src/components/**"] }, forbid: { include: ["src/utils/**"] } }] }, "app/src/components/A.ts": body, "app/src/utils/forbidden.ts": "export const v = 1;\n" });
  assert.equal(run("layer-reach", files("const x = `${require('../utils/forbidden')}`;\n")).length, 1);
  assert.equal(run("layer-reach", files("const x = typeof import('../utils/forbidden');\n")).length, 1);
  assert.deepEqual(run("layer-reach", files("type T = typeof import('../utils/forbidden');\nlet v: typeof import('../utils/forbidden');\nconst z = `plain`;\n")), []);
});

// ---- the real entry point --------------------------------------------------------------------------------------------------------

test("rightkit-gate check runs the four rules over a commit: red on the 13ddad3 deny set, green on 0c47fc3", async () => {
  const t = tree(denyFiles({ denied: DENIED }));
  try {
    const git = (...args) => assert.equal(spawnSync("git", args, { cwd: t.root, encoding: "utf8" }).status, 0, args.join(" "));
    git("init", "-q"); git("add", "-A"); git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "red");
    const lines = [];
    assert.equal(await main(["--repo", t.root, "--only", IDS.join(",")], { out: (s) => lines.push(s), err: (s) => lines.push(s), central: null }), 1);
    assert.match(lines[0], /^FAIL route-deny-policy \(CA-13\): packages\/api\/src\/auth\/agent-policy\.json: the denied route PATCH \/v1\/admin\/settings\/apple is not covered/);
    fs.writeFileSync(path.join(t.root, "packages/api/src/auth/agent-policy.json"), JSON.stringify({ denied: DENIED, exceptions: ["POST /v1/admin/reviews/:id/approve"], irreversible: { segments: [...SEGMENTS_13DDAD3, "apple"], fragments: FRAGMENTS } }));
    git("add", "-A"); git("-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "green");
    lines.length = 0;
    assert.equal(await main(["--repo", t.root, "--only", IDS.join(",")], { out: (s) => lines.push(s), err: (s) => lines.push(s), central: null }), 0);
    assert.match(lines.at(-1), /passed 4 rules/);
  } finally { t.done(); }
});
