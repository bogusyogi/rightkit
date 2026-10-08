import { createHash } from 'node:crypto';
import { win32 } from 'node:path';

function refuse(message) { throw Object.assign(new TypeError(`RK_CLASS1_CHILD_SPEC: ${message}`), { code: 'RK_CLASS1_CHILD_SPEC' }); }
function path(value) {
  if (typeof value !== 'string' || !/^[A-Za-z]:[\\/]/.test(value) || /[\x00-\x1f<>:"|?*]/.test(value.slice(2)) || value.split(/[\\/]/).some((part) => part === '..' || /[. ]$/.test(part) || /^(con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\.|$)/i.test(part))) refuse('canonical local Windows path required');
  return win32.normalize(value).replace(/[\\]+$/, '');
}
function overlaps(left, right) { const rel = win32.relative(left, right); return !rel || (!rel.startsWith('..\\') && rel !== '..' && !win32.isAbsolute(rel)); }

/** Data contract, NOT Node spawn options. rightkit-sandbox has no CLI/bin entry.
 * Lead must map these grants to SandboxPolicy::{read_paths,exec_paths,write_paths},
 * NetworkPolicy::Deny & Sandbox::spawn (SECURITY_CAPABILITIES, suspended child,
 * kill-on-close Job Object). Never spread this into child_process.spawn.
 * Adapter must resolve/pin physical paths, reject reparses/overlap again, lease
 * only traverse rights on ancestors & retain ACL leases until all children exit.
 * Additional tool/cache read grants require separate authenticated input closure.
 */
export function restrictedChildSpec(buildRoot, options = {}) {
  if (!options || typeof options !== 'object' || Array.isArray(options) || Object.keys(options).some((key) => key !== 'outputDir')) refuse('unknown output option');
  const { outputDir = `${buildRoot}.output` } = options;
  const root = path(buildRoot); const output = path(outputDir);
  const spec = { appContainerName: `RightKit.Class1.${createHash('sha256').update(`${root.toLowerCase()}|${output.toLowerCase()}`).digest('hex').slice(0, 32)}`, capabilities: [], readOnlyPaths: [root], writablePaths: [output] };
  validateRestrictedChildSpec(spec);
  for (const list of [spec.capabilities, spec.readOnlyPaths, spec.writablePaths]) Object.freeze(list);
  return Object.freeze(spec);
}

export function validateRestrictedChildSpec(spec) {
  if (!spec || typeof spec !== 'object' || Array.isArray(spec) || Object.keys(spec).sort().join(',') !== 'appContainerName,capabilities,readOnlyPaths,writablePaths') refuse('unknown or missing fields');
  if (!/^RightKit\.Class1\.[a-f0-9]{32}$/.test(spec.appContainerName ?? '')) refuse('invalid AppContainer identity');
  if (!Array.isArray(spec.capabilities) || spec.capabilities.length) refuse('capabilities must be empty (network denied)');
  if (!Array.isArray(spec.readOnlyPaths) || spec.readOnlyPaths.length !== 1 || !Array.isArray(spec.writablePaths) || spec.writablePaths.length !== 1) refuse('one build root & one per-job output required');
  const root = path(spec.readOnlyPaths[0]); const output = path(spec.writablePaths[0]);
  if (root !== spec.readOnlyPaths[0] || output !== spec.writablePaths[0] || root.length <= 2 || output.length <= 2) refuse('paths must be canonical directories below volume root');
  if (overlaps(root, output) || overlaps(output, root)) refuse('output & build root must be disjoint');
  const expected = `RightKit.Class1.${createHash('sha256').update(`${root.toLowerCase()}|${output.toLowerCase()}`).digest('hex').slice(0, 32)}`;
  if (spec.appContainerName !== expected) refuse('AppContainer identity does not bind grant set');
  return true;
}
