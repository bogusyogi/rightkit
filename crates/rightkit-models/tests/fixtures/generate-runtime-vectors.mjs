// Deterministic TEST-ONLY seed; never an operational signing authority.
// Run from RightKit root: node crates/rightkit-models/tests/fixtures/generate-runtime-vectors.mjs
import { createPrivateKey, createPublicKey, createHash, sign as edSign } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import { buildRuntimeArtifactManifest, canonicalRuntimeArtifactManifest, signRuntimeArtifactManifest } from '../../../../packages/release/runtime-artifact-manifest.mjs';
import { RUNTIME_ARTIFACT_TRUSTED_KEY_ID } from '../../../../packages/release/model-promote.mjs';
const seed = Buffer.from(Array.from({length:32}, (_, i) => i));
const privateKey = createPrivateKey({key:Buffer.concat([Buffer.from('302e020100300506032b657004220420','hex'),seed]),format:'der',type:'pkcs8'});
const publicKey = createPublicKey(privateKey);
const body = Buffer.from(Array.from({length:2_000_000}, (_, i) => i % 251));
const legacyManifest = buildRuntimeArtifactManifest({
  artifactKind:'tts-model', entitlement:{appKey:'genright',tier:'pro'},
  distribution:{delivery:'private-r2',bucket:'rightapps-updates'}, target:{os:'any',arch:'any'},
  filename:'qwen.gguf', sha256:createHash('sha256').update(body).digest('hex'),sizeBytes:body.length,
  versions:{runtime:'qts-1', model:'Qwen3-1.7B',tokenizer:'v1',preprocessing:'v1',license:'Apache-2.0',provenance:'测试-é-🎙'},
  provenance:{source:'https://example.invalid/qwen',sourceRevision:'test',licenseId:'Apache-2.0',noticeSha256:'a'.repeat(64)},
  promotion:{authorityAppKey:'genright',promotionId:'test-only',promotedAt:'2026-10-08T00:00:00.000Z',evidenceSha256:'b'.repeat(64)},
});
const envelope = signRuntimeArtifactManifest(legacyManifest,privateKey,RUNTIME_ARTIFACT_TRUSTED_KEY_ID);
const artifactIds = ['qwen3-base','qwen3-chat','qwen3-code','qwen3-embed','qwen3-instruct','qwen3-vision'];
const artifacts = Object.fromEntries(artifactIds.map((artifactId, index) => {
  const manifest = buildRuntimeArtifactManifest({
    ...legacyManifest,
    artifactId,
    filename: `qwen-${index + 1}.gguf`,
    sha256: legacyManifest.object.sha256,
    sizeBytes: legacyManifest.object.sizeBytes,
    object: {
      ...legacyManifest.object,
      filename: `qwen-${index + 1}.gguf`,
    },
  });
  return [artifactId, signRuntimeArtifactManifest(manifest,privateKey,RUNTIME_ARTIFACT_TRUSTED_KEY_ID)];
}));
const uuidArtifactId = '123e4567-e89b-12d3-a456-426614174000';
const uuidArtifactManifest = buildRuntimeArtifactManifest({
  ...legacyManifest,
  artifactId: uuidArtifactId,
  filename: 'qwen-uuid.gguf',
  sha256: legacyManifest.object.sha256,
  sizeBytes: legacyManifest.object.sizeBytes,
});
// distribution.bucket lane contract (#903): private-r2 admits {rightsites-updates, rightapps-updates}, public-r2 admits
// {rightsites-downloads, rightapps-downloads}. Valid vectors are signed through the JS signer (which validates). Refused
// vectors are signed over the canonical bytes WITHOUT the JS validator, so they are correctly signed yet out of contract
// and only the verifier's lane check can refuse them. Test-only key above; never an operational authority.
const sortDeep = (v) => Array.isArray(v) ? v.map(sortDeep)
  : (v && typeof v === 'object') ? Object.fromEntries(Object.keys(v).sort().map((k) => [k, sortDeep(v[k])])) : v;
const signUnchecked = (manifest) => ({
  manifest,
  signature:{algorithm:'Ed25519', keyId:RUNTIME_ARTIFACT_TRUSTED_KEY_ID,
    value:edSign(null, Buffer.from(JSON.stringify(sortDeep(manifest)),'utf8'), privateKey).toString('base64url')},
});
const publicLaneManifest = buildRuntimeArtifactManifest({
  ...legacyManifest, artifactKind:'media-runtime', entitlement:{appKey:'genright',tier:'public'},
  distribution:{delivery:'public-r2',bucket:'rightapps-downloads'}, filename:'media.bin',
  sha256:legacyManifest.object.sha256, sizeBytes:legacyManifest.object.sizeBytes,
});
const withBucket = (manifest, delivery, bucket) => ({...manifest, distribution:{delivery, bucket}});
const rightsitesPrivate = withBucket(legacyManifest, 'private-r2', 'rightsites-updates');
const rightsitesPublic = withBucket(publicLaneManifest, 'public-r2', 'rightsites-downloads');
const sign = (manifest) => signRuntimeArtifactManifest(manifest,privateKey,RUNTIME_ARTIFACT_TRUSTED_KEY_ID);
const lanes = {
  legacyPrivate: envelope,
  legacyPublic: sign(publicLaneManifest),
  rightsitesPrivate: sign(rightsitesPrivate),
  rightsitesPublic: sign(rightsitesPublic),
  refused: {
    unknownBucket: signUnchecked(withBucket(legacyManifest, 'private-r2', 'other-updates')),
    privateNamesRightsitesDownloads: signUnchecked(withBucket(legacyManifest, 'private-r2', 'rightsites-downloads')),
    privateNamesLegacyDownloads: signUnchecked(withBucket(legacyManifest, 'private-r2', 'rightapps-downloads')),
    publicNamesRightsitesUpdates: signUnchecked(withBucket(publicLaneManifest, 'public-r2', 'rightsites-updates')),
    publicNamesLegacyUpdates: signUnchecked(withBucket(publicLaneManifest, 'public-r2', 'rightapps-updates')),
    privateNullBucket: signUnchecked(withBucket(legacyManifest, 'private-r2', null)),
  },
};
writeFileSync(new URL('./runtime-envelope.json', import.meta.url), JSON.stringify({
  publicKey:Array.from(Buffer.from(publicKey.export({format:'jwk'}).x,'base64url')),
  canonical:canonicalRuntimeArtifactManifest(legacyManifest), envelope, artifacts,
  artifactId:artifacts[artifactIds[0]],
  uuidArtifact:signRuntimeArtifactManifest(uuidArtifactManifest,privateKey,RUNTIME_ARTIFACT_TRUSTED_KEY_ID),
  windows:signRuntimeArtifactManifest({...legacyManifest,target:{os:'windows',arch:'x86_64'},pointerKey:'genright/runtime-artifacts/tts-model/windows/x86_64/current/manifest.json'},privateKey,RUNTIME_ARTIFACT_TRUSTED_KEY_ID),
  wrongSize:signRuntimeArtifactManifest({...legacyManifest,object:{...legacyManifest.object,sizeBytes:body.length+1}},privateKey,RUNTIME_ARTIFACT_TRUSTED_KEY_ID),
  lanes,
},null,2)+'\n');
