// Display-only helpers for the agent approval card. Text from an agent or a
// message can hide itself with bidi controls and invisible characters; these
// functions make such characters visible. They never change what is sent or
// hashed.

// C0/C1 controls except \n and \t, bidi controls, zero-width and other invisible
// format characters, soft hyphen, line and paragraph separators.
const HIDDEN =
  // eslint-disable-next-line no-control-regex -- the point is to match control characters
  /[\u0000-\u0008\u000B-\u001F\u007F-\u009F\u00AD\u061C\u200B-\u200F\u2028-\u2029\u202A-\u202E\u2060-\u2064\u2066-\u2069\uFEFF]/g;

/** Replace hidden characters with a visible marker such as ⟦U+202E⟧. */
export function visibleText(s: string): string {
  return s.replace(HIDDEN, (c) => {
    const hex = (c.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, "0");
    return `⟦U+${hex}⟧`;
  });
}

/** Character and line counts of a body as written (CRLF counts as one break). */
export function bodyStats(body: string): { chars: number; lines: number } {
  const text = body.replace(/\r\n/g, "\n");
  return { chars: Array.from(text).length, lines: text === "" ? 0 : text.split("\n").length };
}

/** Body for display: hidden characters marked, runs of 3+ blank lines folded. */
export function displayBody(body: string): string {
  const lines = visibleText(body.replace(/\r\n/g, "\n")).split("\n");
  const out: string[] = [];
  let i = 0;
  while (i < lines.length) {
    if (lines[i].trim() !== "") {
      out.push(lines[i++]);
      continue;
    }
    let j = i;
    while (j < lines.length && lines[j].trim() === "") j++;
    const n = j - i;
    if (n >= 3) out.push(`[${n} blank lines]`);
    else out.push(...lines.slice(i, j));
    i = j;
  }
  return out.join("\n");
}
