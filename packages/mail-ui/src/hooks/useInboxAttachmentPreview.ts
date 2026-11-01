import { useCallback, useState } from "react";
import {
  type AttachmentPreviewPayload,
} from "../components/AttachmentPreviewModal.js";
import {
  downloadAttachment,
  getThread,
  listAttachments,
  previewAttachment,
  type AttachmentMeta,
  type Thread,
} from "../lib/ipc.js";

interface InboxAttachmentPreview extends AttachmentPreviewPayload {
  accountId: string;
  messageId: string;
  attachmentId: string;
}

export function useInboxAttachmentPreview({
  saveAndScan,
  setErr,
}: {
  saveAndScan: (name: string, loader: () => Promise<string>) => void;
  setErr: (message: string) => void;
}) {
  const [attachmentPreview, setAttachmentPreview] =
    useState<InboxAttachmentPreview | null>(null);
  const [openingAttachmentKey, setOpeningAttachmentKey] = useState<
    string | null
  >(null);

  const findThreadAttachment = useCallback(
    async (
      thread: Thread,
      filename: string,
    ): Promise<{ messageId: string; attachment: AttachmentMeta }> => {
      const msgs = await getThread(thread.account_id, thread.thread_id);
      for (const msg of msgs) {
        if (!msg.has_attachments) continue;
        const attachments = await listAttachments(thread.account_id, msg.message_id);
        const exact = attachments.find((a) => a.filename === filename);
        const loose =
          exact ??
          attachments.find(
            (a) => a.filename.toLowerCase() === filename.toLowerCase(),
          );
        if (loose) return { messageId: msg.message_id, attachment: loose };
      }
      throw new Error(`Attachment not found: ${filename}`);
    },
    [],
  );

  const openAttachmentPreview = useCallback(
    async (thread: Thread, filename: string) => {
      const key = `${thread.account_id}:${thread.thread_id}:${filename}`;
      setOpeningAttachmentKey(key);
      try {
        const { messageId, attachment } = await findThreadAttachment(
          thread,
          filename,
        );
        const preview = await previewAttachment(
          thread.account_id,
          messageId,
          attachment.attachment_id,
          attachment.filename,
          attachment.mime_type,
        );
        setAttachmentPreview({
          filename: preview.filename,
          mimeType: preview.mime_type,
          dataB64: preview.data_b64,
          accountId: thread.account_id,
          messageId,
          attachmentId: attachment.attachment_id,
        });
      } catch (e) {
        setErr(String(e));
      } finally {
        setOpeningAttachmentKey(null);
      }
    },
    [findThreadAttachment, setErr],
  );

  const downloadPreviewAttachment = useCallback(() => {
    if (!attachmentPreview) return;
    saveAndScan(attachmentPreview.filename, () =>
      downloadAttachment(
        attachmentPreview.accountId,
        attachmentPreview.messageId,
        attachmentPreview.attachmentId,
        attachmentPreview.filename,
      ),
    );
  }, [attachmentPreview, saveAndScan]);

  return {
    attachmentPreview,
    setAttachmentPreview,
    openingAttachmentKey,
    openAttachmentPreview,
    downloadPreviewAttachment,
  };
}
