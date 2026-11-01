/* eslint-disable react-refresh/only-export-components */
import { createContext, useContext, useState } from 'react'

export interface DaemonConfig {
  base: string
  token: string
  session: string
  workspaceRoot: string
}

export interface DaemonContextValue extends DaemonConfig {
  setSession: (session: string) => void
}

function readConfig(): DaemonConfig {
  const p = new URLSearchParams(window.location.search)
  const env = (import.meta.env ?? {}) as ImportMetaEnv & {
    VITE_RIGHTKIT_DAEMON_URL?: string
    VITE_RIGHTKIT_DAEMON_TOKEN?: string
  }
  const runtimeWindow = window as Window & { __CR_TOKEN?: string }
  const envBase = env.VITE_RIGHTKIT_DAEMON_URL
  const envToken = env.VITE_RIGHTKIT_DAEMON_TOKEN
  return {
    base: p.get('base') || envBase || window.location.origin,
    token: envToken || runtimeWindow.__CR_TOKEN || '',
    session: p.get('session') || 'default',
    workspaceRoot: p.get('workspace') || '',
  }
}

export function daemonHeaders(token: string): Record<string, string> {
  const h: Record<string, string> = { 'Content-Type': 'application/json' }
  if (token) h['Authorization'] = `Bearer ${token}`
  return h
}

export function eventStreamUrl(base: string, session: string, after: number): string {
  const url = new URL(`${base}/sessions/${session}/events`)
  url.searchParams.set('after', String(after))
  return url.toString()
}

const DaemonContext = createContext<DaemonContextValue>({
  base: '',
  token: '',
  session: 'default',
  workspaceRoot: '',
  setSession: () => {},
})

export function DaemonProvider({ children }: { children: React.ReactNode }) {
  const [cfg, setCfg] = useState(readConfig)
  const setSession = (session: string) => {
    const nextSession = session.trim()
    if (!nextSession) return
    setCfg((current) => ({ ...current, session: nextSession }))
    const url = new URL(window.location.href)
    url.searchParams.set('session', nextSession)
    window.history.replaceState(null, '', url.toString())
  }
  return <DaemonContext.Provider value={{ ...cfg, setSession }}>{children}</DaemonContext.Provider>
}

export function useDaemon(): DaemonContextValue {
  return useContext(DaemonContext)
}
