// Compose-form interaction (attachments, recipient autocomplete, snippets, AI).
// Extracted from App (M5). The send-dispatch/undo lifecycle stays in App because
// it must outlive the compose drawer; this hook is the in-drawer form behavior.
import { Button } from "@rightkit/react-shell/react";
import { useRef, useState } from "react";
import Icon from "../components/Icons";
import { colorFor, initialOf } from "../lib/mail";
import { plainToHtml } from "../lib/html";
import { useAppStore, composeEditorRef } from "../store";
import {
  searchRecipients,
  expandContactGroup,
  aiRun,
  listDriveFiles,
  driveAttachFile,
  type OutAttachment,
  type ContactGroup,
  type AiAction,
  type DriveFile,
} from "../lib/ipc";

export function useCompose() {
  const activeAccount = useAppStore((s) => s.activeAccount);
  const contactGroups = useAppStore((s) => s.contactGroups);
  const recipientSug = useAppStore((s) => s.recipientSug);
  const setCompose = useAppStore((s) => s.setCompose);
  const setRecipientSug = useAppStore((s) => s.setRecipientSug);
  const setErr = useAppStore((s) => s.setErr);
  const showUndo = useAppStore((s) => s.showUndo);
  const setSnippetOpen = useAppStore((s) => s.setSnippetOpen);
  const setAiBusy = useAppStore((s) => s.setAiBusy);
  const setAiMenuOpen = useAppStore((s) => s.setAiMenuOpen);
  const recipientTimer = useRef<number | null>(null);
  // Highlighted index in the combined [groups, items] suggestion list.
  const [sugIdx, setSugIdx] = useState(0);
  const [drivePickerOpen, setDrivePickerOpen] = useState(false);
  const [driveFiles, setDriveFiles] = useState<DriveFile[]>([]);
  const [driveQuery, setDriveQuery] = useState("");
  const [drivePath, setDrivePath] = useState<Array<{ id: string | null; name: string }>>([
    { id: null, name: "My Drive" },
  ]);
  const [driveLoading, setDriveLoading] = useState(false);

  /** Read a File as bare base64 (strip the data: URL prefix). */
  function fileToB64(file: File): Promise<string> {
    return new Promise((resolve, reject) => {
      const r = new FileReader();
      r.onload = () => {
        const res = String(r.result);
        const comma = res.indexOf(",");
        resolve(comma >= 0 ? res.slice(comma + 1) : res);
      };
      r.onerror = () => reject(r.error);
      r.readAsDataURL(file);
    });
  }
  /** Add picked local files to the compose as base64 attachments (≤20 MB each). */
  async function addComposeAttachments(files: FileList | null) {
    if (!files) return;
    const added: OutAttachment[] = [];
    for (const f of Array.from(files)) {
      if (f.size > 20 * 1024 * 1024) {
        setErr(`${f.name} is too large (max 20 MB).`);
        continue;
      }
      try {
        added.push({
          filename: f.name,
          mimeType: f.type || "application/octet-stream",
          dataB64: await fileToB64(f),
        });
      } catch {
        setErr(`Could not read ${f.name}.`);
      }
    }
    if (added.length)
      setCompose((c) => (c ? { ...c, attachments: [...c.attachments, ...added] } : c));
  }
  function removeComposeAttachment(idx: number) {
    setCompose((c) => (c ? { ...c, attachments: c.attachments.filter((_, i) => i !== idx) } : c));
  }

  async function loadDrivePicker(folderId = drivePath[drivePath.length - 1]?.id ?? null, query = driveQuery) {
    if (!activeAccount) {
      setErr("Pick an account before attaching from Drive.");
      return;
    }
    setDriveLoading(true);
    try {
      const listing = await listDriveFiles(
        activeAccount,
        folderId ?? undefined,
        query.trim() || undefined,
      );
      setDriveFiles(listing.files);
    } catch (e) {
      setErr(String(e));
    } finally {
      setDriveLoading(false);
    }
  }

  function openDrivePicker() {
    setDrivePickerOpen((open) => {
      const next = !open;
      if (next) void loadDrivePicker();
      return next;
    });
  }

  function searchDrivePicker() {
    setDrivePath([{ id: null, name: "My Drive" }]);
    void loadDrivePicker(null, driveQuery);
  }

  function openDriveFolder(file: DriveFile) {
    if (!file.is_folder) return;
    const nextPath = [...drivePath, { id: file.id, name: file.name }];
    setDriveQuery("");
    setDrivePath(nextPath);
    void loadDrivePicker(file.id, "");
  }

  function backDriveFolder() {
    if (drivePath.length <= 1) return;
    const nextPath = drivePath.slice(0, -1);
    setDrivePath(nextPath);
    setDriveQuery("");
    void loadDrivePicker(nextPath[nextPath.length - 1]?.id ?? null, "");
  }

  function attachDriveFile(file: DriveFile) {
    if (!activeAccount || file.is_folder) return;
    setDriveLoading(true);
    driveAttachFile(activeAccount, file.id, file.mime_type, file.name)
      .then((attachment) => {
        setCompose((c) =>
          c ? { ...c, attachments: [...c.attachments, attachment] } : c,
        );
        showUndo(`Attached ${attachment.filename} from Drive`, [], "");
        setDrivePickerOpen(false);
      })
      .catch((e) => setErr(String(e)))
      .finally(() => setDriveLoading(false));
  }
  /** Debounced recipient autocomplete on the last token of a To/Cc/Bcc field. */
  function onRecipientChange(field: "to" | "cc" | "bcc", value: string) {
    setCompose((c) => (c ? { ...c, [field]: value } : c));
    setSugIdx(0);
    if (!activeAccount) return;
    const token = (value.split(/[,;]/).pop() || "").trim();
    if (recipientTimer.current) window.clearTimeout(recipientTimer.current);
    if (token.length < 2) {
      setRecipientSug(null);
      return;
    }
    const tl = token.toLowerCase();
    const groups = contactGroups.filter((g) => g.name.toLowerCase().includes(tl)).slice(0, 3);
    // Show local group matches immediately so something appears as you type; the
    // contact search fills in on a short debounce (the backend is local-first, so
    // cached contacts come back fast — no need for a long wait).
    if (groups.length) setRecipientSug({ field, items: [], groups });
    recipientTimer.current = window.setTimeout(() => {
      searchRecipients(activeAccount, token)
        .then((items) =>
          setRecipientSug(items.length || groups.length ? { field, items, groups } : null)
        )
        .catch(() => setRecipientSug(groups.length ? { field, items: [], groups } : null));
    }, 110);
  }
  /** Insert a picked recipient, replacing the in-progress last token. */
  function pickRecipient(field: "to" | "cc" | "bcc", email: string) {
    setCompose((c) => {
      if (!c) return c;
      const parts = (c[field] as string).split(/[,;]/);
      parts[parts.length - 1] = email;
      return { ...c, [field]: `${parts.join(", ").trim()}, ` };
    });
    setRecipientSug(null);
  }
  /** Expand a contact group to its members and append them to the field. */
  function pickGroup(field: "to" | "cc" | "bcc", g: ContactGroup) {
    if (!activeAccount) return;
    setRecipientSug(null);
    expandContactGroup(activeAccount, g.resource_name)
      .then((emails) => {
        if (emails.length === 0) {
          setErr(`"${g.name}" has no members with email addresses.`);
          return;
        }
        setCompose((c) => {
          if (!c) return c;
          const parts = (c[field] as string).split(/[,;]/);
          parts[parts.length - 1] = emails.join(", ");
          return { ...c, [field]: `${parts.join(", ").trim()}, ` };
        });
        showUndo(`Added ${emails.length} from ${g.name}`, [], "");
      })
      .catch((e) => setErr(String(e)));
  }
  /** Keyboard nav for the autocomplete: ↓/↑ move the highlight, Enter picks it,
   *  Esc closes. Only acts when this field's dropdown is open. */
  function onRecipientKeyDown(field: "to" | "cc" | "bcc", e: React.KeyboardEvent) {
    if (recipientSug?.field !== field) return;
    const total = recipientSug.groups.length + recipientSug.items.length;
    if (total === 0) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setSugIdx((i) => Math.min(total - 1, i + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setSugIdx((i) => Math.max(0, i - 1));
    } else if (e.key === "Enter" || e.key === "Tab") {
      // Tab/Enter accept the highlighted suggestion (preventDefault stops Tab from
      // jumping to the Cc/Bcc field before the pick lands).
      e.preventDefault();
      const i = Math.min(sugIdx, total - 1);
      if (i < recipientSug.groups.length) pickGroup(field, recipientSug.groups[i]);
      else pickRecipient(field, recipientSug.items[i - recipientSug.groups.length].email);
    } else if (e.key === "Escape") {
      setRecipientSug(null);
    }
  }
  /** The recipient-autocomplete dropdown for a field (or null when not active). */
  function recipSuggest(field: "to" | "cc" | "bcc") {
    if (recipientSug?.field !== field) return null;
    return (
      <div className="recip-suggest">
        {recipientSug.groups.map((g, gi) => (
          <Button variant="ghost"
            key={g.resource_name}
            className={`recip-item ${gi === sugIdx ? "active" : ""}`}
            onMouseEnter={() => setSugIdx(gi)}
            onMouseDown={(e) => {
              e.preventDefault();
              pickGroup(field, g);
            }}
          >
            <span className="recip-photo recip-init" style={{ background: "var(--muted-2)" }}>
              <Icon name="tag" size={13} />
            </span>
            <span className="recip-name">{g.name}</span>
            <span className="recip-email">{g.member_count} people</span>
          </Button>
        ))}
        {recipientSug.items.map((r, ri) => (
          <Button variant="ghost"
            key={r.email}
            className={`recip-item ${recipientSug.groups.length + ri === sugIdx ? "active" : ""}`}
            onMouseEnter={() => setSugIdx(recipientSug.groups.length + ri)}
            onMouseDown={(e) => {
              e.preventDefault();
              pickRecipient(field, r.email);
            }}
          >
            {r.photo_url ? (
              <img className="recip-photo" src={r.photo_url} alt="" />
            ) : (
              <span className="recip-photo recip-init" style={{ background: colorFor(r.email) }}>
                {initialOf(r.name || r.email)}
              </span>
            )}
            <span className="recip-name">{r.name || r.email}</span>
            <span className="recip-email">{r.email}</span>
          </Button>
        ))}
      </div>
    );
  }
  /** Insert a snippet's text into the compose editor. */
  function insertSnippet(s: { name: string; body: string }) {
    setSnippetOpen(false);
    composeEditorRef.current?.insertHTML(plainToHtml(s.body));
  }
  /** Run a BYOK AI rewrite over the editor's current text. */
  function runComposeAi(action: AiAction) {
    const text = composeEditorRef.current?.getText().trim() ?? "";
    if (!text) return;
    setAiBusy(true);
    setAiMenuOpen(false);
    aiRun(action, text)
      .then((out) => composeEditorRef.current?.setHTML(plainToHtml(out)))
      .catch((e) => setErr(String(e)))
      .finally(() => setAiBusy(false));
  }

  return {
    addComposeAttachments,
    attachDriveFile,
    backDriveFolder,
    driveFiles,
    driveLoading,
    drivePath,
    drivePickerOpen,
    driveQuery,
    openDriveFolder,
    openDrivePicker,
    removeComposeAttachment,
    searchDrivePicker,
    setDrivePickerOpen,
    setDriveQuery,
    onRecipientChange,
    onRecipientKeyDown,
    recipSuggest,
    insertSnippet,
    runComposeAi,
  };
}
