export function hasOutgoingPayload(text: string, attachmentCount: number): boolean {
  return text.trim().length > 0 || attachmentCount > 0
}

interface MessageAttachment {
  type: 'image'
  data: string
  mimeType: string
}

export function buildSessionMessageBody(
  text: string,
  nonce: string,
  attachments: MessageAttachment[] | undefined,
  workspaceRoot: string,
): {
  text: string
  nonce: string
  attachments?: MessageAttachment[]
  workspaceRoot?: string
} {
  const trimmedWorkspaceRoot = workspaceRoot.trim()
  return {
    text,
    nonce,
    attachments: attachments?.length ? attachments : undefined,
    workspaceRoot: trimmedWorkspaceRoot || undefined,
  }
}
