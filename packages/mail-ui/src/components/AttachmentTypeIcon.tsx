import { attachmentTypeInfo } from "../lib/attachmentTypes";

interface Props {
  filename: string;
  mimeType?: string | null;
  bytes?: Uint8Array;
  size?: "sm" | "md" | "lg";
  className?: string;
}

export default function AttachmentTypeIcon({
  filename,
  mimeType = "",
  bytes,
  size = "md",
  className = "",
}: Props) {
  const info = attachmentTypeInfo(filename, mimeType, bytes);
  return (
    <span
      className={[
        "filetype-icon",
        `filetype-icon--${info.tone}`,
        `filetype-icon--${size}`,
        className,
      ]
        .filter(Boolean)
        .join(" ")}
      title={`${info.label} attachment`}
      aria-hidden="true"
    >
      <span className="filetype-icon-label">{info.label}</span>
    </span>
  );
}
