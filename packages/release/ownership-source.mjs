// Code-only patterns may consume string arguments, but must start outside
// literals/comments. Keep offsets/newlines intact for multiline call reporting.
export function sourceNonCode(text, rust) {
  const nonCode = new Uint8Array(text.length);
  for (let i = 0; i < text.length;) {
    const start = i;
    if (text.startsWith("//", i) || text.startsWith("/*", i)) {
      if (text[i + 1] === "/") {
        i = text.indexOf("\n", i + 2);
        if (i === -1) i = text.length;
      } else {
        i += 2;
        let depth = 1;
        while (i < text.length && depth) {
          if (rust && text.startsWith("/*", i)) { depth++; i += 2; }
          else if (text.startsWith("*/", i)) { depth--; i += 2; }
          else i++;
        }
      }
      nonCode.fill(1, start, i);
      continue;
    }
    const raw = rust && (i === 0 || !/[\w]/.test(text[i - 1])) && /^(?:br|cr|r)(#*)"/.exec(text.slice(i));
    if (raw) {
      const closing = `"${raw[1]}`, end = text.indexOf(closing, i + raw[0].length);
      i = end === -1 ? text.length : end + closing.length;
    } else if (text[i] === '"' || (!rust && (text[i] === "'" || text[i] === "`")) ||
      (rust && text[i] === "'" && /^'(?:\\(?:u\{[\da-fA-F]+\}|x[\da-fA-F]{2}|[^\r\n])|[^'\\\r\n])'/.test(text.slice(i)))) {
      const quote = text[i++];
      while (i < text.length) {
        if (text[i] === "\\") i = Math.min(i + 2, text.length);
        else if (text[i++] === quote) break;
      }
    } else { i++; continue; }
    nonCode.fill(1, start, i);
  }
  return nonCode;
}

