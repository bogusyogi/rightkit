export function looksLikeDiff(text: string): boolean {
  const trimmed = text.trimStart()
  return trimmed.startsWith('---') || trimmed.startsWith('@@') || trimmed.startsWith('diff --git')
}
