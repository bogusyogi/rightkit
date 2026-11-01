import type { Account, PublishJob } from "./types.js";

export interface AccountOrdinal {
  /** "Account A", "Account B". Unique within one venture and platform. */
  label: string;
  letter: string;
  index: number;
  of: number;
}

/**
 * Two accounts on one platform inside one venture get letters A, B in list order. The label is
 * always shown with the handle, so the letter is never the only distinguisher.
 */
export function accountOrdinals(accounts: readonly Account[]): Map<string, AccountOrdinal> {
  const groups = new Map<string, Account[]>();
  for (const account of accounts) {
    const key = `${account.venture.id}|${account.platform}`;
    const group = groups.get(key) ?? [];
    group.push(account);
    groups.set(key, group);
  }
  const out = new Map<string, AccountOrdinal>();
  for (const group of groups.values()) {
    group.forEach((account, index) => {
      const letter = String.fromCharCode(65 + index);
      out.set(account.id, { label: `Account ${letter}`, letter, index, of: group.length });
    });
  }
  return out;
}

/** Short form of a digest: algorithm prefix removed, first eight characters kept. */
export function shortHash(hash: string): string {
  return hash.replace(/^[a-z0-9]+:/, "").slice(0, 8);
}

/** Character count in code points, which is how the preview counts what a reader sees. */
export function charCount(text: string): number {
  return [...text].length;
}

export function awaitingDecision(job: PublishJob): boolean {
  return job.approval.status === "pending" || job.approval.status === "invalidated";
}

export function isApproved(job: PublishJob): boolean {
  return job.approval.status === "approved";
}

export function mediaReady(job: PublishJob): boolean {
  return job.mediaItems.length > 0 && job.mediaItems.every((m) => m.status === "ready");
}

export function jobsByAccount(jobs: readonly PublishJob[]): Map<string, PublishJob[]> {
  const map = new Map<string, PublishJob[]>();
  for (const job of jobs) {
    const list = map.get(job.accountId) ?? [];
    list.push(job);
    map.set(job.accountId, list);
  }
  return map;
}
