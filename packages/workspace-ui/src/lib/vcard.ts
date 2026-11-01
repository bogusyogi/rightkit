// Minimal vCard 3.0 + simple CSV (de)serialization for contacts import/export.
// Pure functions, no DOM/IPC — unit-testable.

export interface VCardContact {
  name: string;
  emails: string[];
  phones: string[];
  org?: string;
}

function escapeVcard(v: string): string {
  // RFC 6350 escaping for the values we emit.
  return v.replace(/\\/g, "\\\\").replace(/\n/g, "\\n").replace(/,/g, "\\,").replace(/;/g, "\\;");
}

function unescapeVcard(v: string): string {
  return v.replace(/\\n/gi, "\n").replace(/\\,/g, ",").replace(/\\;/g, ";").replace(/\\\\/g, "\\");
}

/** Serialize contacts to a vCard 3.0 (.vcf) document. */
export function buildVcf(contacts: VCardContact[]): string {
  const cards = contacts.map((c) => {
    const lines = ["BEGIN:VCARD", "VERSION:3.0", `FN:${escapeVcard(c.name || "")}`];
    for (const e of c.emails) if (e.trim()) lines.push(`EMAIL:${escapeVcard(e.trim())}`);
    for (const p of c.phones) if (p.trim()) lines.push(`TEL:${escapeVcard(p.trim())}`);
    if (c.org && c.org.trim()) lines.push(`ORG:${escapeVcard(c.org.trim())}`);
    lines.push("END:VCARD");
    return lines.join("\r\n");
  });
  return cards.join("\r\n") + "\r\n";
}

/** Parse a vCard document (one or many cards) into contacts. */
export function parseVcf(text: string): VCardContact[] {
  const out: VCardContact[] = [];
  // Unfold folded lines (RFC 6350 §3.2: a CRLF + space/tab continues a line).
  const unfolded = text.replace(/\r\n[ \t]/g, "").replace(/\n[ \t]/g, "");
  const blocks = unfolded.split(/END:VCARD/i);
  for (const block of blocks) {
    if (!/BEGIN:VCARD/i.test(block)) continue;
    const c: VCardContact = { name: "", emails: [], phones: [], org: "" };
    for (const raw of block.split(/\r?\n/)) {
      const line = raw.trim();
      // Strip any property parameters before the ':' (e.g. EMAIL;TYPE=WORK:).
      const colon = line.indexOf(":");
      if (colon < 0) continue;
      const key = line.slice(0, colon).split(";")[0].toUpperCase();
      const val = unescapeVcard(line.slice(colon + 1).trim());
      if (!val) continue;
      if (key === "FN") c.name = val;
      else if (key === "N" && !c.name) {
        const [family = "", given = ""] = val.split(";");
        c.name = [given, family].filter(Boolean).join(" ");
      } else if (key === "EMAIL") c.emails.push(val);
      else if (key === "TEL") c.phones.push(val);
      else if (key === "ORG") c.org = val.split(";")[0];
    }
    if (c.name || c.emails.length) out.push(c);
  }
  return out;
}

/** Best-effort CSV parse: needs a header row with name/email columns (Google/Outlook export style). */
export function parseContactsCsv(text: string): VCardContact[] {
  const rows = text.split(/\r?\n/).filter((r) => r.trim());
  if (rows.length < 2) return [];
  const split = (r: string) => r.split(",").map((c) => c.replace(/^"|"$/g, "").trim());
  const header = split(rows[0]).map((h) => h.toLowerCase());
  const nameIdx = header.findIndex((h) => h.includes("name"));
  const emailIdx = header.findIndex((h) => h.includes("e-mail") || h.includes("email"));
  const phoneIdx = header.findIndex((h) => h.includes("phone"));
  const out: VCardContact[] = [];
  for (let i = 1; i < rows.length; i++) {
    const cols = split(rows[i]);
    const name = nameIdx >= 0 ? cols[nameIdx] ?? "" : "";
    const email = emailIdx >= 0 ? cols[emailIdx] ?? "" : "";
    const phone = phoneIdx >= 0 ? cols[phoneIdx] ?? "" : "";
    if (name || email) {
      out.push({ name, emails: email ? [email] : [], phones: phone ? [phone] : [], org: "" });
    }
  }
  return out;
}
