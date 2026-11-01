import { useCallback, useEffect, useRef, useState } from 'react';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import {
  cancelLocalImage,
  errorMessage,
  fetchLocalImageRuntime,
  getLocalImageJob,
  isTauri,
  listLocalImages,
  resumeLocalImage,
  submitLocalImage
} from '../../lib/api';
import type {
  LocalImageConversationSnapshot,
  LocalImageInstallProgress,
  LocalImageJob,
  LocalImageJobEventPayload,
  LocalImageRuntimeStatus
} from '../../lib/types';
import { buildRetryRequest, isActiveJob } from './logic';

/**
 * Live state for the local image chat: conversations, runtime status, install
 * progress. Tauri events refresh immediately; a poll covers browser mode and
 * missed events. Only active while `enabled`.
 */
export function useLocalImageLive(enabled: boolean, onProfilesChanged: () => void, onArtifactCommitted?: () => void) {
  const [conversations, setConversations] = useState<LocalImageConversationSnapshot[]>([]);
  const [runtime, setRuntime] = useState<LocalImageRuntimeStatus | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [installProgress, setInstallProgress] = useState<Record<string, LocalImageInstallProgress>>({});
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const convRef = useRef<LocalImageConversationSnapshot[]>([]);
  const statesRef = useRef<Map<string, string>>(new Map());
  const profilesChangedRef = useRef(onProfilesChanged);
  const committedRef = useRef(onArtifactCommitted);
  profilesChangedRef.current = onProfilesChanged;
  committedRef.current = onArtifactCommitted;
  convRef.current = conversations;

  const refreshConversations = useCallback(async () => {
    try {
      const list = await listLocalImages();
      setConversations(list.conversations);
    } catch (e) {
      setError(errorMessage(e));
    }
  }, []);

  const refreshRuntime = useCallback(async () => {
    try {
      setRuntime(await fetchLocalImageRuntime());
    } catch (e) {
      setError(errorMessage(e));
    }
  }, []);

  const refreshJob = useCallback(async (jobId: string) => {
    try {
      const snap = await getLocalImageJob(jobId);
      const known = convRef.current.some((c) => c.conversation.id === snap.job.conversation_id);
      if (!known) return void (await refreshConversations());
      setConversations((cur) => cur.map((entry) => {
        if (entry.conversation.id !== snap.job.conversation_id) return entry;
        const jobs = entry.jobs.some((j) => j.id === snap.job.id) ? entry.jobs.map((j) => (j.id === snap.job.id ? snap.job : j)) : [...entry.jobs, snap.job];
        const m = snap.message;
        const messages = m ? (entry.messages.some((x) => x.id === m.id) ? entry.messages.map((x) => (x.id === m.id ? m : x)) : [...entry.messages, m]) : entry.messages;
        return { ...entry, jobs, messages, artifacts: { ...entry.artifacts, [snap.job.id]: snap.artifacts } };
      }));
    } catch {
      await refreshConversations();
    }
  }, [refreshConversations]);

  useEffect(() => {
    if (!enabled) return;
    setLoading(true);
    void Promise.all([refreshConversations(), refreshRuntime()]).finally(() => setLoading(false));
  }, [enabled, refreshConversations, refreshRuntime]);

  // Notify when a job newly succeeds (Gallery refresh hook).
  useEffect(() => {
    for (const snap of conversations) {
      for (const job of snap.jobs) {
        const prev = statesRef.current.get(job.id);
        statesRef.current.set(job.id, job.state);
        if (job.state === 'succeeded' && prev !== undefined && prev !== 'succeeded') committedRef.current?.();
      }
    }
  }, [conversations]);

  useEffect(() => {
    if (!enabled) return;
    const timer = window.setInterval(() => {
      const active = convRef.current.some((c) => c.jobs.some((j) => isActiveJob(j)));
      if (active || !isTauri()) void refreshConversations();
      if (active) void refreshRuntime();
    }, 1500);
    return () => window.clearInterval(timer);
  }, [enabled, refreshConversations, refreshRuntime]);

  useEffect(() => {
    if (!enabled || !isTauri()) return;
    let cancelled = false;
    const offs: UnlistenFn[] = [];
    const reg = (p: Promise<UnlistenFn>) => void p.then((off) => (cancelled ? off() : offs.push(off))).catch(() => undefined);
    reg(listen<LocalImageJobEventPayload>('local-image://job', (ev) => {
      void refreshJob(ev.payload.job_id);
      void refreshRuntime();
    }));
    reg(listen<LocalImageInstallProgress>('local-image://install', (ev) => {
      setInstallProgress((cur) => ({ ...cur, [ev.payload.profile_id]: ev.payload }));
      profilesChangedRef.current();
    }));
    return () => {
      cancelled = true;
      offs.forEach((off) => off());
    };
  }, [enabled, refreshJob, refreshRuntime]);

  const act = useCallback(async (fn: () => Promise<unknown>) => {
    try {
      setError(null);
      await fn();
      await refreshConversations();
    } catch (e) {
      setError(errorMessage(e).replace(/^[a-z][a-z0-9_]*:\s*/, ''));
    }
  }, [refreshConversations]);

  const cancel = useCallback((job: LocalImageJob) => act(() => cancelLocalImage(job.id)), [act]);
  const resume = useCallback((job: LocalImageJob) => act(() => resumeLocalImage(job.id)), [act]);
  const retry = useCallback((job: LocalImageJob) => act(() => submitLocalImage(buildRetryRequest(job, crypto.randomUUID()))), [act]);

  return { conversations, runtime, selectedId, setSelectedId, installProgress, loading, error, setError, refreshConversations, refreshRuntime, cancel, resume, retry };
}

export type LocalImageLive = ReturnType<typeof useLocalImageLive>;
