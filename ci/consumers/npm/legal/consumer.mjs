import { validateInstalledArtifact } from './provenance.mjs';
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { readFileSync } from 'node:fs';

const recipeId = 'legal-manifest-receipt-digest-consumer';
const result = {
  schemaVersion: 1, recipeId, ok: false, nativeHostQualified: false,
  host: { platform: process.platform, arch: process.arch, node: process.version },
  artifact: null, scenarios: [], errors: [],
};
const required = name => { assert(process.env[name], `${name} is required`); return process.env[name]; };
const hex = (ch) => ch.repeat(64);
// Fixture manifest: every field is a synthetic value; URLs use the reserved .invalid TLD and are never fetched.
const fixtureManifest = () => ({
  schema: 2, suite: 'right-suite-desktop', appKey: 'fixture-suite', productName: 'Fixture Suite',
  licensor: 'Fixture Licensor', effectiveDate: '2026-10-20', acceptanceVersion: '2026-10-20.1',
  documents: [
    { id: 'license', role: 'license', title: 'License', version: '1.0.0', sha256: hex('a'), path: 'legal/license.md', publicUrl: 'https://example.invalid/legal/license/', treatment: 'agree', material: true },
    { id: 'eula', role: 'eula', title: 'EULA', version: '1.0.0', sha256: hex('b'), path: 'legal/eula.md', publicUrl: 'https://example.invalid/legal/eula/', treatment: 'agree', material: true },
    { id: 'acceptable-use', role: 'acceptable_use', title: 'Acceptable use', version: '1.0.0', sha256: hex('c'), path: 'legal/acceptable-use.md', publicUrl: 'https://example.invalid/legal/acceptable-use/', treatment: 'agree', material: true },
    { id: 'product-schedule', role: 'product_schedule', title: 'Product schedule', version: '1.0.0', sha256: hex('d'), path: 'legal/product-schedule.md', publicUrl: 'https://example.invalid/legal/product-schedule/', treatment: 'agree', material: true },
    { id: 'privacy', role: 'privacy_notice', title: 'Privacy', version: '1.0.0', sha256: hex('e'), path: 'legal/privacy.md', publicUrl: 'https://example.invalid/legal/privacy/', treatment: 'acknowledge', material: false },
    { id: 'third-party-notices', role: 'third_party_notices', title: 'Third-party notices', version: '1.0.0', sha256: hex('f'), path: 'legal/third-party-notices.md', publicUrl: 'https://example.invalid/legal/third-party-notices/', treatment: 'notice', material: false },
  ],
});
// Fixture failures report fixed scenario names only: never serialize an untrusted host error.
let stage = 'artifact-provenance';
try {
  const archive = required('RIGHTKIT_CONSUMER_ARTIFACT');
  assert(archive.startsWith('/') || /^[A-Za-z]:[\\/]/.test(archive), 'artifact must be absolute');
  const sha256 = required('RIGHTKIT_CONSUMER_ARTIFACT_SHA256');
  const version = required('RIGHTKIT_CONSUMER_EXPECTED_VERSION');
  const resolution = required('RIGHTKIT_CONSUMER_RESOLUTION');
  assert.equal(createHash('sha256').update(readFileSync(archive)).digest('hex'), sha256);
  const root = import.meta.resolve('@rightkit/legal');
  const browser = import.meta.resolve('@rightkit/legal/browser');
  const tooling = import.meta.resolve('@rightkit/legal/tooling');
  const provenance = validateInstalledArtifact({
    fixtureUrl: import.meta.url, archive, sha256, version, resolution, packageName: '@rightkit/legal', rootResolvedUrl: root,
    entries: { '@rightkit/legal': root, '@rightkit/legal/browser': browser, '@rightkit/legal/tooling': tooling },
  });
  result.artifact = provenance.artifact;
  const legal = await import(provenance.entries['@rightkit/legal']);
  const browserApi = await import(provenance.entries['@rightkit/legal/browser']);
  const toolingApi = await import(provenance.entries['@rightkit/legal/tooling']);
  const run = async (name, body) => { stage = name; const evidence = await body(); result.scenarios.push({ name, ok: true, evidence }); };

  await run('default-policy-render-digest-bound', async () => {
    const documents = legal.renderDefaultPolicies({ appName: 'Fixture Suite App' });
    assert.deepEqual(documents.map(doc => doc.kind), ['eula', 'privacy', 'refund', 'terms']);
    for (const doc of documents) {
      assert.equal(doc.sha256, legal.sha256Hex(doc.content));
      assert.ok(doc.content.endsWith('\n') && !doc.content.endsWith('\n\n'));
    }
    assert.deepEqual(legal.renderDefaultPolicies({ appName: 'Fixture Suite App' }).map(doc => doc.sha256), documents.map(doc => doc.sha256));
    return { documents: documents.length, digestBound: true, deterministic: true };
  });

  await run('manifest-validates-and-declares-without-clock', async () => {
    const manifest = fixtureManifest();
    assert.equal(legal.assertLegalManifest(manifest), manifest);
    const declaration = legal.acceptanceDeclaration(manifest);
    assert.equal(declaration.documents.length, 6);
    assert.equal('acceptedAt' in declaration, false);
    assert.equal(declaration.documents.some(doc => 'path' in doc || 'publicUrl' in doc), false);
    return { declaredDocuments: declaration.documents.length, clockFieldPresent: false };
  });

  await run('receipt-current-until-material-change', async () => {
    const manifest = fixtureManifest();
    const receipt = legal.createAcceptanceReceipt(manifest, { acceptedAt: '2026-10-20T00:00:00.000Z', surface: 'first_run', eligibilityBasis: 'individual' });
    assert.equal(legal.needsLegalAcceptance(manifest, receipt), false);
    const revised = fixtureManifest();
    revised.documents[1] = { ...revised.documents[1], version: '1.0.1' };
    assert.equal(legal.needsLegalAcceptance(revised, receipt), true);
    return { currentReceiptAccepted: true, materialChangeReprompts: true };
  });

  await run('material-digest-tracks-agreements-only', async () => {
    const base = fixtureManifest();
    const privacyOnly = fixtureManifest();
    privacyOnly.documents[4] = { ...privacyOnly.documents[4], sha256: hex('9') };
    assert.equal(legal.materialAcceptanceDigest(privacyOnly), legal.materialAcceptanceDigest(base));
    const eulaChanged = fixtureManifest();
    eulaChanged.documents[1] = { ...eulaChanged.documents[1], sha256: hex('8') };
    assert.notEqual(legal.materialAcceptanceDigest(eulaChanged), legal.materialAcceptanceDigest(base));
    assert.equal(await browserApi.materialAcceptanceDigestAsync(base), legal.materialAcceptanceDigest(base));
    return { nonMaterialIgnored: true, materialChangeDetected: true, webCryptoParity: true };
  });

  await run('unsafe-manifests-refused', async () => {
    const mutations = [
      m => { m.documents[0].path = '../license.md'; },
      m => { m.documents[0].publicUrl = 'http://example.invalid/legal/license/'; },
      m => { m.documents[4].treatment = 'agree'; },
      m => { m.documents[5].treatment = 'agree'; },
      m => { m.documents.splice(0, 1); },
      m => { m.appKey = 'Fixture'; },
    ];
    for (const mutate of mutations) {
      const manifest = fixtureManifest();
      mutate(manifest);
      assert.throws(() => legal.assertLegalManifest(manifest));
    }
    return { refusedMutations: mutations.length };
  });

  await run('tooling-subpath-isolated-from-root', async () => {
    assert.equal(typeof toolingApi.syncLegalSnapshot, 'function');
    assert.equal(typeof toolingApi.assertFinalLegalSnapshot, 'function');
    assert.equal(typeof toolingApi.generateNpmLicenseAppendix, 'function');
    assert.equal('syncLegalSnapshot' in legal, false);
    assert.equal(typeof browserApi.sha256HexAsync, 'function');
    return { toolingExports: 3, rootLeaksTooling: false };
  });

  stage = 'qualification-complete';
  result.ok = true;
} catch {
  result.errors.push({ stage, code: 'consumer-assertion-failed' });
  process.exitCode = 1;
}
process.stdout.write(`${JSON.stringify(result)}\n`);
