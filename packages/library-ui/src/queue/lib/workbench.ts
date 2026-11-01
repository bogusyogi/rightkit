import type { JobInfo } from "../contract.js";
import { getShell, isQaMode } from "./ipc/index.js";

export function selectWorkbenchJobId(jobs: JobInfo[], currentId: string | null): string | null {
  const current = currentId ? jobs.find((job) => job.id === currentId) : null;
  if (current?.needs_auth) return current.id;
  const authJob = jobs.find((job) => job.needs_auth);
  if (authJob) return authJob.id;
  if (current) return current.id;
  return jobs.find((job) => job.stage === "done")?.id ?? jobs[0]?.id ?? null;
}

/** Copy through the RightKit shell clipboard. QA mode keeps the page's navigator.clipboard seam. */
export async function copyTextToClipboard(text: string): Promise<void> {
  if (isQaMode) {
    await navigator.clipboard.writeText(text);
    return;
  }
  await getShell().clipboard.write({ text });
}
