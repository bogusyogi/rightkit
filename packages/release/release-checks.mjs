/** Run only app-configured pre-build checks; release safeguards never route here. */
export async function runConfiguredChecks({ packageManager, checks = [], workdir, skipChecks = false, run }) {
  if (skipChecks) return;
  for (const script of checks) await run(packageManager, script, workdir);
}
