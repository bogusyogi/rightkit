import { Button, Dialog, SkeletonRows, Input } from "@rightkit/react-shell/react";
// Contacts — a browsable list (parity with contacts.google.com): search +
// full contact list with avatars, a create/edit modal, delete, and the
// import/export/dedupe tools. Group filtering reads the active group from the
// store (set in ContactsNav). Directory search falls back to the People API for
// addresses outside the personal contact list.
import { memo, useEffect, useMemo, useRef, useState } from "react";
import Icon from "@rightkit/mail-ui/components/Icons";
import { AccountSwitcher } from "@rightkit/mail-ui/components/AccountSwitcher";
import { colorFor, initialOf } from "@rightkit/mail-ui/lib/mail";
import { buildVcf, parseVcf, parseContactsCsv, type VCardContact } from "../lib/vcard";
import { useAppStore } from "@rightkit/mail-ui/store/index";
import { showDialog } from "@rightkit/mail-ui/hooks/dialog";
import {
  searchRecipients,
  listAllContacts,
  createContact,
  updateContact,
  deleteContact,
  saveTextDownload,
  type ContactFull,
  type Recipient,
} from "@rightkit/mail-ui/lib/ipc";
import { EMPTY_EDITOR, type Editor } from "./ContactsView.types";
function ContactsViewInner() {
  const openComposeTo = useAppStore((s) => s.openComposeTo);
  const activeAccount = useAppStore((s) => s.activeAccount);
  const setErr = useAppStore((s) => s.setErr);
  const showUndo = useAppStore((s) => s.showUndo);
  const setView = useAppStore((s) => s.setView);
  const contactsQuery = useAppStore((s) => s.contactsQuery);
  const setContactsQuery = useAppStore((s) => s.setContactsQuery);
  const contactsBusy = useAppStore((s) => s.contactsBusy);
  const setContactsBusy = useAppStore((s) => s.setContactsBusy);
  const contactDupes = useAppStore((s) => s.contactDupes);
  const setContactDupes = useAppStore((s) => s.setContactDupes);
  const [contacts, setContacts] = useState<ContactFull[]>([]);
  const [dirResults, setDirResults] = useState<Recipient[]>([]);
  const [editor, setEditor] = useState<Editor | null>(null);
  const fileRef = useRef<HTMLInputElement>(null);
  const searchTimer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  function loadAll() {
    if (!activeAccount) return;
    setContactsBusy(true);
    listAllContacts(activeAccount)
      .then((cs) =>
        setContacts(
          cs.sort((a, b) => (a.display_name || a.emails[0] || "").localeCompare(b.display_name || b.emails[0] || ""))
        )
      )
      .catch((e) => setErr(String(e)))
      .finally(() => setContactsBusy(false));
  }
  useEffect(() => {
    loadAll();
    // eslint-disable-next-line react-hooks/exhaustive-deps -- loadAll is a plain function redefined every render (closes over setErr/setContactsBusy); tracking it would refetch the full contact list on every render instead of only on account change.
  }, [activeAccount]);
  // Local filter is instant; the directory lookup (≥2 chars) is debounced so we
  // don't fire an IPC call on every keystroke.
  function onSearch(q: string) {
    setContactsQuery(q);
    clearTimeout(searchTimer.current);
    if (!activeAccount || q.trim().length < 2) {
      setDirResults([]);
      return;
    }
    searchTimer.current = setTimeout(() => {
      searchRecipients(activeAccount, q.trim())
        .then((rs) => setDirResults(rs))
        .catch(() => setDirResults([]));
    }, 280);
  }
  const query = contactsQuery.trim().toLowerCase();
  const filtered = useMemo(() => {
    if (!query) return contacts;
    return contacts.filter(
      (c) =>
        c.display_name.toLowerCase().includes(query) ||
        c.emails.some((e) => e.toLowerCase().includes(query)) ||
        c.org.toLowerCase().includes(query)
    );
  }, [contacts, query]);
  // Directory hits not already in the personal list (only while searching).
  const extraDir = useMemo(() => {
    if (query.length < 2) return [];
    const known = new Set(contacts.flatMap((c) => c.emails.map((e) => e.toLowerCase())));
    return dirResults.filter((r) => r.email && !known.has(r.email.toLowerCase()));
  }, [dirResults, contacts, query]);
  function emailContact(addr: string) {
    setView("inbox");
    setTimeout(() => openComposeTo(addr, "", ""), 0);
  }
  function startCreate() {
    setEditor(EMPTY_EDITOR);
  }
  function startEdit(c: ContactFull) {
    setEditor({
      resource_name: c.resource_name,
      etag: c.etag,
      name: c.display_name,
      emails: c.emails.length ? c.emails : [""],
      phones: c.phones.length ? c.phones : [""],
    });
  }
  function saveEditor() {
    if (!activeAccount || !editor) return;
    const name = editor.name.trim();
    const emails = editor.emails.map((e) => e.trim()).filter(Boolean);
    const phones = editor.phones.map((p) => p.trim()).filter(Boolean);
    if (!name && emails.length === 0) {
      setErr("A contact needs at least a name or an email.");
      return;
    }
    setContactsBusy(true);
    const op = editor.resource_name
      ? updateContact(activeAccount, editor.resource_name, editor.etag, name, emails, phones)
      : createContact(activeAccount, name, emails, phones);
    op.then(() => {
      showUndo(editor.resource_name ? "Contact saved" : "Contact created", [], "");
      setEditor(null);
      loadAll();
    })
      .catch((e) => setErr(String(e)))
      .finally(() => setContactsBusy(false));
  }
  async function removeContact(c: ContactFull) {
    if (!activeAccount) return;
    if ((await showDialog({ kind: "confirm", message: `Delete "${c.display_name || c.emails[0]}"? Moves to Google's contact trash (30-day undo).` })) !== "ok")
      return;
    setContactsBusy(true);
    deleteContact(activeAccount, c.resource_name)
      .then(() => {
        showUndo("Contact deleted", [], "");
        setContacts((prev) => prev.filter((x) => x.resource_name !== c.resource_name));
      })
      .catch((e) => setErr(String(e)))
      .finally(() => setContactsBusy(false));
  }
  function exportContacts() {
    if (!activeAccount || contactsBusy) return;
    const vcf = buildVcf(
      contacts.map((c) => ({ name: c.display_name, emails: c.emails, phones: c.phones, org: c.org }))
    );
    // Saved natively to Downloads (exclusive atomic publish), like every other download.
    saveTextDownload("contacts.vcf", vcf)
      .then(() => showUndo(`Exported ${contacts.length} contacts`, [], ""))
      .catch((e) => setErr(String(e)));
  }
  function importContactsFile(file: File) {
    if (!activeAccount) return;
    file
      .text()
      .then(async (text) => {
        const parsed: VCardContact[] = file.name.toLowerCase().endsWith(".csv")
          ? parseContactsCsv(text)
          : parseVcf(text);
        if (parsed.length === 0) {
          setErr("No contacts found in that file.");
          return;
        }
        if ((await showDialog({ kind: "confirm", message: `Import ${parsed.length} contact(s) into ${activeAccount}?` })) !== "ok") return;
        setContactsBusy(true);
        let ok = 0;
        for (const c of parsed) {
          try {
            await createContact(activeAccount, c.name, c.emails, c.phones);
            ok++;
          } catch (e) {
            setErr(String(e));
            break;
          }
        }
        showUndo(`Imported ${ok} of ${parsed.length} contacts`, [], "");
        loadAll();
      })
      .catch((e) => setErr(String(e)))
      .finally(() => setContactsBusy(false));
  }
  function findContactDupes() {
    if (!activeAccount || contactsBusy) return;
    const byEmail = new Map<string, ContactFull[]>();
    for (const c of contacts) {
      for (const e of c.emails) {
        const key = e.trim().toLowerCase();
        if (!key) continue;
        const arr = byEmail.get(key) ?? [];
        if (!arr.includes(c)) arr.push(c);
        byEmail.set(key, arr);
      }
    }
    const seen = new Set<string>();
    const groups: { primary: ContactFull; dups: ContactFull[] }[] = [];
    for (const arr of byEmail.values()) {
      if (arr.length < 2) continue;
      const key = arr.map((c) => c.resource_name).sort().join("|");
      if (seen.has(key)) continue;
      seen.add(key);
      groups.push({ primary: arr[0], dups: arr.slice(1) });
    }
    setContactDupes(groups);
    if (groups.length === 0) showUndo("No duplicate contacts found", [], "");
  }
  async function mergeDupeGroup(group: { primary: ContactFull; dups: ContactFull[] }) {
    if (!activeAccount || contactsBusy) return;
    if ((await showDialog({ kind: "confirm", message: `Merge ${group.dups.length} duplicate(s) into "${group.primary.display_name || group.primary.emails[0]}"?` })) !== "ok")
      return;
    const all = [group.primary, ...group.dups];
    const emails = Array.from(new Set(all.flatMap((c) => c.emails.map((e) => e.trim())).filter(Boolean)));
    const phones = Array.from(new Set(all.flatMap((c) => c.phones.map((p) => p.trim())).filter(Boolean)));
    const name = all.map((c) => c.display_name).find((n) => n.trim()) || emails[0] || "";
    setContactsBusy(true);
    updateContact(activeAccount, group.primary.resource_name, group.primary.etag, name, emails, phones)
      .then(async () => {
        for (const d of group.dups) await deleteContact(activeAccount!, d.resource_name);
        showUndo("Merged duplicates", [], "");
        setContactDupes((prev) => (prev ? prev.filter((g) => g !== group) : prev));
        loadAll();
      })
      .catch((e) => setErr(String(e)))
      .finally(() => setContactsBusy(false));
  }
  const setEd = (patch: Partial<Editor>) => setEditor((e) => (e ? { ...e, ...patch } : e));
  return (
    <div className="contactsview">
      <div className="contacts-bar">
        <h2 className="workspace-title">Contacts</h2>
        <span className="contacts-count">{contacts.length}</span>
        <div className="drive-bar-spacer" />
        <Input
          className="workspace-search"
          placeholder="Search contacts & directory…"
          value={contactsQuery}
          onChange={(e) => onSearch(e.target.value)}
        />
        <Button variant="ghost" size="sm" disabled={contactsBusy} onClick={exportContacts}>Export</Button>
        <Button variant="ghost" size="sm" disabled={contactsBusy} onClick={() => fileRef.current?.click()}>Import</Button>
        <Button variant="ghost" size="sm" disabled={contactsBusy} onClick={findContactDupes}>Find duplicates</Button>
        <Button variant="primary" size="sm" onClick={startCreate}>
          <Icon name="person-add" size={15} /> Create
        </Button>
        <AccountSwitcher />
        <Input
          ref={fileRef}
          type="file"
          accept=".vcf,.csv"
          style={{ display: "none" }}
          onChange={(e) => {
            const f = e.target.files?.[0];
            if (f) importContactsFile(f);
            e.target.value = "";
          }}
        />
      </div>
      <div className="contacts-scroll">
        {contactDupes && (
          <div className="dupe-panel">
            <div className="labels-head">
              <span>Duplicates</span>
              <Button variant="ghost" size="sm" onClick={() => setContactDupes(null)}>Dismiss</Button>
            </div>
            {contactDupes.length === 0 ? (
              <p className="hint">No duplicate contacts.</p>
            ) : (
              contactDupes.map((g) => (
                <div className="contact-row" key={g.primary.resource_name}>
                  <span className="contact-av" style={{ background: colorFor(g.primary.emails[0] || g.primary.display_name) }}>
                    {initialOf(g.primary.display_name || g.primary.emails[0])}
                  </span>
                  <span className="contact-name">{g.primary.display_name || g.primary.emails[0]}</span>
                  <span className="contact-sub">{g.dups.length + 1} copies · {g.primary.emails[0]}</span>
                  <Button variant="ghost" size="sm" disabled={contactsBusy} onClick={() => mergeDupeGroup(g)}>Merge</Button>
                </div>
              ))
            )}
          </div>
        )}
        {contactsBusy && contacts.length === 0 ? (
          <SkeletonRows rows={10} />
        ) : filtered.length === 0 && extraDir.length === 0 ? (
          <div className="workspace-empty">
            <Icon name="people" size={42} />
            <p className="hint">{query ? "No matching contacts." : "No contacts yet."}</p>
            {!query && <Button variant="primary" size="sm" onClick={startCreate}>Create your first contact</Button>}
          </div>
        ) : (
          <div className="contacts-list">
            {filtered.map((c) => (
              <div className="contact-row" key={c.resource_name}>
                <span className="contact-av" style={{ background: colorFor(c.emails[0] || c.display_name) }}>
                  {initialOf(c.display_name || c.emails[0])}
                </span>
                <span className="contact-name">{c.display_name || "(no name)"}</span>
                <span className="contact-email">{c.emails[0] || ""}</span>
                <span className="contact-sub">{c.phones[0] || c.org || ""}</span>
                <div className="contact-actions">
                  {c.emails[0] && (
                    <Button variant="ghost" size="sm" title="Email" onClick={() => emailContact(c.emails[0])}>Email</Button>
                  )}
                  <Button variant="ghost" size="sm" title="Edit" onClick={() => startEdit(c)}>Edit</Button>
                  <Button variant="danger" size="sm" title="Delete" onClick={() => removeContact(c)}>Delete</Button>
                </div>
              </div>
            ))}
            {extraDir.length > 0 && (
              <>
                <div className="labels-head"><span>From the directory</span></div>
                {extraDir.map((r) => (
                  <div className="contact-row" key={r.email}>
                    {r.photo_url ? (
                      <img className="contact-av" src={r.photo_url} alt="" />
                    ) : (
                      <span className="contact-av" style={{ background: colorFor(r.email) }}>{initialOf(r.name || r.email)}</span>
                    )}
                    <span className="contact-name">{r.name || r.email}</span>
                    <span className="contact-email">{r.email}</span>
                    <span className="contact-sub" />
                    <div className="contact-actions">
                      <Button variant="ghost" size="sm" onClick={() => emailContact(r.email)}>Email</Button>
                      <Button
                        variant="ghost" size="sm"
                        onClick={() => setEditor({ resource_name: "", etag: "", name: r.name || "", emails: [r.email], phones: [""] })}
                      >
                        Add to contacts
                      </Button>
                    </div>
                  </div>
                ))}
              </>
            )}
          </div>
        )}
      </div>
      {editor && (
        <Dialog title={editor.resource_name ? "Edit contact" : "New contact"} width={440} onClose={() => setEditor(null)}>
            <Input
              className="event-title-input"
              placeholder="Name"
              autoFocus
              value={editor.name}
              onChange={(e) => setEd({ name: e.target.value })}
            />
            <div className="ce-field-group">
              <span className="ce-label">Email</span>
              {editor.emails.map((em, i) => (
                <Input
                  key={i}
                  className="workspace-search ce-input"
                  type="email"
                  placeholder="name@example.com"
                  value={em}
                  onChange={(e) => setEd({ emails: editor.emails.map((v, j) => (j === i ? e.target.value : v)) })}
                />
              ))}
              <Button variant="ghost" size="sm" onClick={() => setEd({ emails: [...editor.emails, ""] })}>+ Add email</Button>
            </div>
            <div className="ce-field-group">
              <span className="ce-label">Phone</span>
              {editor.phones.map((ph, i) => (
                <Input
                  key={i}
                  className="workspace-search ce-input"
                  placeholder="+1 555 000 0000"
                  value={ph}
                  onChange={(e) => setEd({ phones: editor.phones.map((v, j) => (j === i ? e.target.value : v)) })}
                />
              ))}
              <Button variant="ghost" size="sm" onClick={() => setEd({ phones: [...editor.phones, ""] })}>+ Add phone</Button>
            </div>
            <div className="ws-form-foot">
              <Button variant="primary" disabled={contactsBusy} onClick={saveEditor}>
                {editor.resource_name ? "Save" : "Create"}
              </Button>
              <Button variant="ghost" size="sm" onClick={() => setEditor(null)}>Cancel</Button>
            </div>
        </Dialog>
      )}
    </div>
  );
}
export const ContactsView = memo(ContactsViewInner);
