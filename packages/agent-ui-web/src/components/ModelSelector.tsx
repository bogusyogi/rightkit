import { useState } from 'react'

const MODEL_OPTIONS = [
  'openrouter/auto',
  'anthropic/claude-sonnet-4.5',
  'openai/gpt-5-codex',
  'x-ai/grok-build-0.1',
  'deepseek/deepseek-v4-pro',
  'qwen/qwen3-coder',
]

export function ModelSelector({
  currentModel,
  options,
  onModelChange,
}: {
  currentModel: string
  options?: string[]
  onModelChange: (model: string) => Promise<void>
}) {
  const [pending, setPending] = useState(false)
  const modelOptions = options?.length ? options : MODEL_OPTIONS
  const value = currentModel || modelOptions[0]

  async function change(model: string) {
    setPending(true)
    try {
      await onModelChange(model)
    } finally {
      setPending(false)
    }
  }

  return (
    <select
      value={value}
      disabled={pending}
      onChange={(event) => void change(event.target.value)}
      title="Session model"
      style={{
        height: '30px',
        minWidth: '180px',
        border: '1px solid var(--border)',
        borderRadius: '8px',
        backgroundColor: 'var(--surface)',
        color: 'var(--text)',
        fontSize: '12px',
        padding: '0 8px',
        outline: 'none',
      }}
    >
      {modelOptions.map((model) => (
        <option key={model} value={model}>
          {model}
        </option>
      ))}
    </select>
  )
}
