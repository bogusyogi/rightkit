import { invoke } from "./invoke";
import type { DriveFile } from "./workspace";

export interface AttachmentMeta {
  attachment_id: string;
  filename: string;
  mime_type: string;
  size: number;
}

export interface AttachmentPreview {
  filename: string;
  mime_type: string;
  data_b64: string;
}

export function listAttachments(accountId: string, messageId: string): Promise<AttachmentMeta[]> {
  return invoke<AttachmentMeta[]>("list_attachments", { accountId, messageId });
}

export interface InlineImageRef {
  cid: string;
  mime_type: string;
  hash: string;
  data_b64: string;
}

export function inlineImageRefs(accountId: string, messageId: string): Promise<InlineImageRef[]> {
  return invoke<InlineImageRef[]>("inline_image_refs", { accountId, messageId });
}

export interface RemoteImage {
  mime_type: string;
  hash: string;
  /** True when the URL's host matched the tracker blocklist (a placeholder
   * was returned and no network fetch happened). */
  blocked: boolean;
}

export function fetchRemoteImage(url: string): Promise<RemoteImage> {
  return invoke<RemoteImage>("fetch_remote_image", { url });
}

export function downloadAttachment(
  accountId: string,
  messageId: string,
  attachmentId: string,
  filename: string
): Promise<string> {
  return invoke<string>("download_attachment", { accountId, messageId, attachmentId, filename });
}

export function previewAttachment(
  accountId: string,
  messageId: string,
  attachmentId: string,
  filename: string,
  mimeType: string
): Promise<AttachmentPreview> {
  return invoke<AttachmentPreview>("preview_attachment", {
    accountId,
    messageId,
    attachmentId,
    filename,
    mimeType,
  });
}

export function saveAttachmentToDrive(
  accountId: string,
  messageId: string,
  attachmentId: string,
  filename: string,
  mimeType: string,
  parentId: string | null = null
): Promise<DriveFile> {
  return invoke<DriveFile>("save_attachment_to_drive", {
    accountId,
    messageId,
    attachmentId,
    filename,
    mimeType,
    parentId,
  });
}

export function getRawMessage(accountId: string, messageId: string): Promise<string> {
  return invoke<string>("get_raw_message", { accountId, messageId });
}

export function scanFile(path: string): Promise<string> {
  return invoke<string>("scan_file", { path });
}

/** Save text the UI built (the contacts export) to Downloads; resolves to the saved path. */
export function saveTextDownload(filename: string, text: string): Promise<string> {
  return invoke<string>("save_text_download", { filename, text });
}

export function downloadEml(
  accountId: string,
  messageId: string,
  filename: string
): Promise<string> {
  return invoke<string>("download_eml", { accountId, messageId, filename });
}
