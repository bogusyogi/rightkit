/* eslint-disable react-refresh/only-export-components */
export interface UsageState {
  promptTokens: number
  completionTokens: number
  cacheReadTokens: number
  cacheCreationTokens: number
  costUsd: number
}

export const EMPTY_USAGE: UsageState = {
  promptTokens: 0,
  completionTokens: 0,
  cacheReadTokens: 0,
  cacheCreationTokens: 0,
  costUsd: 0,
}

export function UsagePanel({ usage }: { usage: UsageState }) {
  const totalTokens =
    usage.promptTokens +
    usage.completionTokens +
    usage.cacheReadTokens +
    usage.cacheCreationTokens

  return (
    <div
      title={`${totalTokens.toLocaleString()} tokens`}
      style={{
        display: 'flex',
        alignItems: 'center',
        gap: '8px',
        height: '30px',
        padding: '0 10px',
        border: '1px solid var(--border)',
        borderRadius: '8px',
        backgroundColor: 'var(--surface)',
        color: 'var(--muted)',
        fontSize: '12px',
        whiteSpace: 'nowrap',
      }}
    >
      <span>${usage.costUsd.toFixed(4)}</span>
      <span style={{ opacity: 0.65 }}>{totalTokens.toLocaleString()} tok</span>
    </div>
  )
}
