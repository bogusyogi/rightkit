// Position-aware scan of a Cargo manifest so a dependency's version string can be
// replaced in place. The text between the replaced quotes is the only thing that
// changes; comments, ordering, tables, features and formatting are untouched.

export interface VersionRef {
  /** Effective package name: the `package = "..."` rename if present, else the key. */
  name: string;
  keyName: string;
  /** Current version requirement text. */
  spec: string;
  /** Offsets of the text between the quotes. */
  start: number;
  end: number;
  /** Table the dependency was declared in, for example `workspace.dependencies`. */
  table: string;
}

class TomlSyntaxError extends Error {}

type StringValue = { kind: "string"; start: number; end: number; text: string; simple: boolean };
type Value = StringValue | { kind: "inline"; entries: Array<{ key: string[]; value: Value }> } | { kind: "other" };

const DEP_SECTIONS = new Set(["dependencies", "dev-dependencies", "build-dependencies"]);

class Scanner {
  pos = 0;
  constructor(readonly text: string) {}

  fail(message: string): never {
    throw new TomlSyntaxError(`${message} at offset ${this.pos}`);
  }

  peek(offset = 0): string {
    return this.text[this.pos + offset] ?? "";
  }

  eof(): boolean {
    return this.pos >= this.text.length;
  }

  /** Skip spaces and tabs only. */
  skipInline(): void {
    while (this.peek() === " " || this.peek() === "\t") this.pos++;
  }

  skipComment(): void {
    if (this.peek() === "#") while (!this.eof() && this.peek() !== "\n") this.pos++;
  }

  /** Skip whitespace, newlines and comments. */
  skipAll(): void {
    for (;;) {
      const ch = this.peek();
      if (ch === " " || ch === "\t" || ch === "\n" || ch === "\r") this.pos++;
      else if (ch === "#") this.skipComment();
      else return;
    }
  }

  parseKey(): string[] {
    const parts: string[] = [];
    for (;;) {
      this.skipInline();
      const ch = this.peek();
      if (ch === '"' || ch === "'") {
        parts.push(this.parseString().text);
      } else {
        const start = this.pos;
        while (/[A-Za-z0-9_-]/.test(this.peek())) this.pos++;
        if (this.pos === start) this.fail("expected a key");
        parts.push(this.text.slice(start, this.pos));
      }
      this.skipInline();
      if (this.peek() === ".") {
        this.pos++;
        continue;
      }
      return parts;
    }
  }

  parseString(): StringValue {
    const quote = this.peek();
    if (this.text.startsWith(quote.repeat(3), this.pos)) {
      const open = this.pos + 3;
      let i = open;
      for (;;) {
        if (i >= this.text.length) this.fail("unterminated multi-line string");
        if (quote === '"' && this.text[i] === "\\") {
          i += 2;
          continue;
        }
        if (this.text.startsWith(quote.repeat(3), i)) {
          // Up to two extra quotes may directly precede the closing delimiter.
          let end = i;
          while (this.text[end + 3] === quote) end++;
          this.pos = end + 3;
          return { kind: "string", start: open, end, text: this.text.slice(open, end), simple: false };
        }
        i++;
      }
    }
    const open = this.pos + 1;
    let i = open;
    let simple = true;
    for (;;) {
      const ch = this.text[i];
      if (ch === undefined || ch === "\n") this.fail("unterminated string");
      if (quote === '"' && ch === "\\") {
        simple = false;
        i += 2;
        continue;
      }
      if (ch === quote) break;
      i++;
    }
    this.pos = i + 1;
    return { kind: "string", start: open, end: i, text: this.text.slice(open, i), simple };
  }

  parseValue(): Value {
    this.skipInline();
    const ch = this.peek();
    if (ch === '"' || ch === "'") return this.parseString();
    if (ch === "[") {
      this.pos++;
      for (;;) {
        this.skipAll();
        if (this.peek() === "]") {
          this.pos++;
          return { kind: "other" };
        }
        this.parseValue();
        this.skipAll();
        if (this.peek() === ",") this.pos++;
        else if (this.peek() !== "]") this.fail("expected , or ] in array");
      }
    }
    if (ch === "{") {
      this.pos++;
      const entries: Array<{ key: string[]; value: Value }> = [];
      for (;;) {
        this.skipAll();
        if (this.peek() === "}") {
          this.pos++;
          return { kind: "inline", entries };
        }
        const key = this.parseKey();
        this.skipInline();
        if (this.peek() !== "=") this.fail("expected = in inline table");
        this.pos++;
        entries.push({ key, value: this.parseValue() });
        this.skipAll();
        if (this.peek() === ",") this.pos++;
        else if (this.peek() !== "}") this.fail("expected , or } in inline table");
      }
    }
    // Number, boolean or date: runs to a delimiter.
    const start = this.pos;
    while (!this.eof() && !/[\s,\]}#]/.test(this.peek())) this.pos++;
    if (this.pos === start) this.fail("expected a value");
    return { kind: "other" };
  }
}

interface DepAccumulator {
  keyName: string;
  table: string;
  packageName?: string;
  version?: { spec: string; start: number; end: number };
}

/** True for a table path that holds dependency entries. */
function isDepTable(path: string[]): boolean {
  if (path.length === 1) return DEP_SECTIONS.has(path[0]);
  if (path.length === 2) return path[0] === "workspace" && path[1] === "dependencies";
  if (path.length === 3) return path[0] === "target" && DEP_SECTIONS.has(path[2]);
  return false;
}

/** Scan a Cargo manifest and return every dependency version string with its offsets. Throws on syntax it cannot follow. */
export function findVersionRefs(text: string): VersionRef[] {
  const scanner = new Scanner(text);
  const deps = new Map<string, DepAccumulator>();
  let header: string[] = [];

  const accumulator = (tablePath: string[], keyName: string): DepAccumulator => {
    const id = `${tablePath.join("\u0000")}\u0001${keyName}`;
    let acc = deps.get(id);
    if (!acc) {
      acc = { keyName, table: tablePath.join(".") };
      deps.set(id, acc);
    }
    return acc;
  };

  const setField = (acc: DepAccumulator, field: string, value: Value): void => {
    if (value.kind !== "string" || !value.simple) return;
    if (field === "package") acc.packageName = value.text;
    if (field === "version") acc.version = { spec: value.text, start: value.start, end: value.end };
  };

  const record = (key: string[], value: Value): void => {
    if (isDepTable(header)) {
      const [keyName, ...rest] = key;
      const acc = accumulator(header, keyName);
      if (rest.length === 0) {
        if (value.kind === "string") setField(acc, "version", value);
        else if (value.kind === "inline") {
          for (const entry of value.entries) if (entry.key.length === 1) setField(acc, entry.key[0], entry.value);
        }
      } else if (rest.length === 1) {
        setField(acc, rest[0], value);
      }
    } else if (header.length >= 2 && isDepTable(header.slice(0, -1))) {
      if (key.length === 1) setField(accumulator(header.slice(0, -1), header[header.length - 1]), key[0], value);
    }
  };

  for (;;) {
    scanner.skipAll();
    if (scanner.eof()) break;
    if (scanner.peek() === "[") {
      const array = scanner.peek(1) === "[";
      scanner.pos += array ? 2 : 1;
      header = scanner.parseKey();
      scanner.skipInline();
      if (scanner.peek() !== "]") scanner.fail("expected ] in table header");
      scanner.pos++;
      if (array) {
        if (scanner.peek() !== "]") scanner.fail("expected ]] in array table header");
        scanner.pos++;
        header = [...header, "[]"];
      }
      scanner.skipInline();
      scanner.skipComment();
      continue;
    }
    const key = scanner.parseKey();
    scanner.skipInline();
    if (scanner.peek() !== "=") scanner.fail("expected = after key");
    scanner.pos++;
    const value = scanner.parseValue();
    scanner.skipInline();
    scanner.skipComment();
    record(key, value);
  }

  const refs: VersionRef[] = [];
  for (const acc of deps.values()) {
    if (!acc.version) continue;
    refs.push({
      name: acc.packageName ?? acc.keyName,
      keyName: acc.keyName,
      spec: acc.version.spec,
      start: acc.version.start,
      end: acc.version.end,
      table: acc.table,
    });
  }
  return refs.sort((a, b) => a.start - b.start);
}

export interface EditResult {
  text: string;
  /** Number of version strings rewritten. */
  count: number;
}

/**
 * Replace every dependency version string of package `name` whose text is exactly
 * `oldSpec` with `newSpec`. Everything outside those string bodies is preserved byte for byte.
 */
export function replaceCargoVersion(text: string, name: string, oldSpec: string, newSpec: string): EditResult {
  if (!/^[0-9A-Za-z.^~=<>*+-]+$/.test(newSpec)) throw new Error(`refusing unsafe version text: ${newSpec}`);
  const targets = findVersionRefs(text).filter((ref) => ref.name === name && ref.spec === oldSpec);
  let out = text;
  for (const ref of [...targets].reverse()) out = out.slice(0, ref.start) + newSpec + out.slice(ref.end);
  return { text: out, count: targets.length };
}
