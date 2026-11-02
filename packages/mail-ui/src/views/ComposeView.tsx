// ComposeView — extracted from App.tsx (M5). Form interaction comes from the
// `useCompose` hook; the send-dispatch/undo lifecycle (which outlives the
// drawer) stays in App and is passed in as a few lifecycle-trigger props.
import { useRef } from "react";
import Icon from "../components/Icons.js";
import { ComposeEditor } from "../components/ComposeEditor.js";
import { RecipientField } from "../components/RecipientField.js";
import { useAppStore, composeEditorRef } from "../store/index.js";
import { useCompose } from "../hooks/useCompose.js";
import { plainToHtml } from "../lib/html.js";

import { Button, ContextMenu, IconButton, Input, SearchIcon, Select } from "@rightkit/react-shell/react";
interface Props {
  sendCompose: () => void;
  sendComposeArchive: () => void;
  doSaveDraft: (close: boolean) => void;
  scheduleCompose: (whenMs: number) => void;
  closeCompose: () => void;
  scheduleAutosave: () => void;
}

export function ComposeView({
  sendCompose, sendComposeArchive, doSaveDraft, scheduleCompose, closeCompose, scheduleAutosave,
}: Props) {
  const {
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
    insertSnippet,
    onRecipientChange,
    onRecipientKeyDown,
    recipSuggest,
    runComposeAi,
  } = useCompose();
  const aiBtnRef = useRef<HTMLButtonElement>(null);
  const snippetBtnRef = useRef<HTMLButtonElement>(null);
  const attachRef = useRef<HTMLInputElement>(null);
  const activeAccount = useAppStore((s) => s.activeAccount);
  const aiBusy = useAppStore((s) => s.aiBusy);
  const aiMenuOpen = useAppStore((s) => s.aiMenuOpen);
  const aliases = useAppStore((s) => s.aliases);
  const compose = useAppStore((s) => s.compose);
  const scheduleAt = useAppStore((s) => s.scheduleAt);
  const scheduleOpen = useAppStore((s) => s.scheduleOpen);
  const sending = useAppStore((s) => s.sending);
  const setAiMenuOpen = useAppStore((s) => s.setAiMenuOpen);
  const setCompose = useAppStore((s) => s.setCompose);
  const setErr = useAppStore((s) => s.setErr);
  const setRecipientSug = useAppStore((s) => s.setRecipientSug);
  const setScheduleAt = useAppStore((s) => s.setScheduleAt);
  const setScheduleOpen = useAppStore((s) => s.setScheduleOpen);
  const setSnippetOpen = useAppStore((s) => s.setSnippetOpen);
  const sigFor = useAppStore((s) => s.sigFor);
  const showPro = useAppStore((s) => s.showPro);
  const snippetOpen = useAppStore((s) => s.snippetOpen);
  const snippets = useAppStore((s) => s.snippets);

  // Rendered only when a compose is open (App gates on `compose`); narrow here.
  if (!compose) return null;

  return (
        <div className="compose">
          <div className="compose-head">
            <span>New message</span>
            <IconButton
              label="Close"
              onClick={closeCompose}
            >
              ✕
            </IconButton>
          </div>
          <div className="compose-fields" onInput={scheduleAutosave}>
            <label className="compose-row">
              <span className="cl">From</span>
              <Select
                className="compose-from"
                label="From"
                value={compose.from}
                options={(aliases.length
                  ? aliases
                  : [{ email: activeAccount ?? "", name: "", is_default: true }]
                ).map((a) => ({ value: a.email, label: a.name ? `${a.name} <${a.email}>` : a.email }))}
                onChange={(newFrom) => {
                  const oldSig = sigFor(compose.from, compose.accountId);
                  const newSig = sigFor(newFrom, compose.accountId);
                  const editor = composeEditorRef.current;
                  if (editor && oldSig !== newSig) {
                    let html = editor.getHTML();
                    if (oldSig) {
                      const sentinel = "-- ";
                      const idx = html.lastIndexOf(sentinel);
                      if (idx !== -1) {
                        html = html.slice(0, idx);
                      }
                    }
                    if (newSig) {
                      html += `<p>-- <br>${newSig.replace(/\n/g, "<br>")}</p>`;
                    }
                    editor.setHTML(html);
                  }
                  setCompose({ ...compose, from: newFrom });
                }}
              />
            </label>
            <label className="compose-row">
              <span className="cl">To</span>
              <RecipientField
                value={compose.to}
                onChange={(v) => onRecipientChange("to", v)}
                onKeyDown={(e) => onRecipientKeyDown("to", e)}
                onBlur={() => window.setTimeout(() => setRecipientSug(null), 180)}
                placeholder="recipient@example.com"
              >
                {recipSuggest("to")}
              </RecipientField>
              {!compose.showCc && (
                <Button
                  variant="ghost" size="sm"
                  onClick={() => setCompose({ ...compose, showCc: true })}
                >
                  Cc/Bcc
                </Button>
              )}
            </label>
            {compose.showCc && (
              <>
                <label className="compose-row">
                  <span className="cl">Cc</span>
                  <RecipientField
                    value={compose.cc}
                    onChange={(v) => onRecipientChange("cc", v)}
                    onKeyDown={(e) => onRecipientKeyDown("cc", e)}
                    onBlur={() => window.setTimeout(() => setRecipientSug(null), 180)}
                  >
                    {recipSuggest("cc")}
                  </RecipientField>
                </label>
                <label className="compose-row">
                  <span className="cl">Bcc</span>
                  <RecipientField
                    value={compose.bcc}
                    onChange={(v) => onRecipientChange("bcc", v)}
                    onKeyDown={(e) => onRecipientKeyDown("bcc", e)}
                    onBlur={() => window.setTimeout(() => setRecipientSug(null), 180)}
                  >
                    {recipSuggest("bcc")}
                  </RecipientField>
                </label>
              </>
            )}
            <Input
              className="compose-subject"
              value={compose.subject}
              onChange={(e) => setCompose({ ...compose, subject: e.target.value })}
              placeholder="Subject"
            />
            <ComposeEditor
              key={compose.nonce}
              ref={composeEditorRef}
              initialHtml={compose.bodyHtml ?? plainToHtml(compose.body)}
            />
            {compose.attachments.length > 0 && (
              <div className="compose-attachments">
                {compose.attachments.map((a, idx) => (
                  <span className="att-chip" key={idx} title={a.filename}>
                    <Icon name="folder" size={13} />
                    <span className="att-name">{a.filename}</span>
                    <IconButton label="Remove"
                      className="att-x"
                      title="Remove"
                      onClick={() => removeComposeAttachment(idx)}
                    >
                      ✕
                    </IconButton>
                  </span>
                ))}
              </div>
            )}
          </div>
          <div className="compose-foot">
            <Button variant="primary" disabled={sending} onClick={sendCompose}>
              {sending ? "Sending…" : "Send"}
            </Button>
            {compose.threadId && (
              <Button
                variant="ghost" size="sm"
                disabled={sending}
                title="Send this reply and archive the conversation"
                onClick={sendComposeArchive}
              >
                Send &amp; Archive
              </Button>
            )}
            <Button variant="ghost" size="sm" onClick={() => doSaveDraft(true)}>
              Save draft
            </Button>
            {showPro && (
              <span className="ai-wrap">
                <Button
                  ref={aiBtnRef}
                  variant="ghost" size="sm"
                  disabled={aiBusy}
                  onClick={() => setAiMenuOpen((o) => !o)}
                  title="AI writing help (BYOK)"
                >
                  {aiBusy ? "AI…" : "AI"}
                </Button>
                {aiMenuOpen && aiBtnRef.current && (
                  <ContextMenu
                    anchor={aiBtnRef.current}
                    placement="top-start"
                    label="AI writing help"
                    showLead={false}
                    onClose={() => setAiMenuOpen(false)}
                    items={[
                      { id: "proofread", label: "Proofread", run: () => runComposeAi("proofread") },
                      { id: "shorter", label: "Make shorter", run: () => runComposeAi("shorter") },
                      { id: "warmer", label: "Warmer", run: () => runComposeAi("warmer") },
                      { id: "more_direct", label: "More direct", run: () => runComposeAi("more_direct") },
                    ]}
                  />
                )}
              </span>
            )}
            {snippets.length > 0 && (
              <span className="ai-wrap">
                <Button
                  ref={snippetBtnRef}
                  variant="ghost" size="sm"
                  onClick={() => setSnippetOpen((o) => !o)}
                  title="Insert a saved snippet"
                >
                  Snippets
                </Button>
                {snippetOpen && snippetBtnRef.current && (
                  <ContextMenu
                    anchor={snippetBtnRef.current}
                    placement="top-start"
                    label="Snippets"
                    showLead={false}
                    onClose={() => setSnippetOpen(false)}
                    items={snippets.map((sn, i) => ({ id: String(i), label: sn.name, run: () => insertSnippet(sn) }))}
                  />
                )}
              </span>
            )}
            <Button
              variant="ghost" size="sm"
              onClick={() => setScheduleOpen((o) => !o)}
              title="Schedule send"
            >
              Schedule
            </Button>
            {scheduleOpen && (
              <span className="schedule-inline">
                <Input
                  type="datetime-local"
                  value={scheduleAt}
                  onChange={(e) => setScheduleAt(e.target.value)}
                />
                <Button
                  variant="primary" size="sm"
                  onClick={() =>
                    scheduleAt
                      ? scheduleCompose(new Date(scheduleAt).getTime())
                      : setErr("Pick a time to schedule.")
                  }
                >
                  Set
                </Button>
              </span>
            )}
            <Button variant="ghost" size="sm" onClick={closeCompose}>
              Discard
            </Button>
            {/* Attach — paperclip, pushed to the far right of the footer. */}
            <IconButton label="Attach files" className="attach-btn" style={{ marginLeft: "auto" }} onClick={() => attachRef.current?.click()}>
              <Icon name="paperclip" size={17} />
            </IconButton>
            <Input
              ref={attachRef}
              type="file"
              multiple
              style={{ display: "none" }}
              onChange={(e) => {
                addComposeAttachments(e.target.files);
                e.currentTarget.value = "";
              }}
            />
            <span className="ai-wrap">
              <IconButton label="Attach from Drive" className="attach-btn" onClick={openDrivePicker}>
                <Icon name="drive" size={17} />
              </IconButton>
              {drivePickerOpen && (
                <div className="drive-picker">
                  <div className="drive-picker-head">
                    <IconButton
                      label="Back"
                      disabled={drivePath.length <= 1 || driveLoading}
                      onClick={backDriveFolder}
                    >
                      <Icon name="chevron-left" size={15} />
                    </IconButton>
                    <span>{drivePath[drivePath.length - 1]?.name ?? "My Drive"}</span>
                    <IconButton
                      label="Close"
                      onClick={() => setDrivePickerOpen(false)}
                    >
                      <Icon name="close" size={14} />
                    </IconButton>
                  </div>
                  <div className="drive-picker-search">
                    <Input
                      value={driveQuery}
                      onChange={(e) => setDriveQuery(e.target.value)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") searchDrivePicker();
                      }}
                      placeholder="Search Drive"
                    />
                    <IconButton label="Search" onClick={searchDrivePicker}>
                      <SearchIcon />
                    </IconButton>
                  </div>
                  <div className="drive-picker-list">
                    {driveLoading ? (
                      <div className="drive-picker-empty">Loading…</div>
                    ) : driveFiles.length === 0 ? (
                      <div className="drive-picker-empty">No files</div>
                    ) : (
                      driveFiles.slice(0, 30).map((file) => (
                        <Button variant="ghost"
                          className="drive-picker-item"
                          key={file.id}
                          onClick={() =>
                            file.is_folder ? openDriveFolder(file) : attachDriveFile(file)
                          }
                        >
                          <Icon name={file.is_folder ? "folder" : "doc"} size={15} />
                          <span>{file.name}</span>
                        </Button>
                      ))
                    )}
                  </div>
                </div>
              )}
            </span>
          </div>
        </div>
  );
}
