// APP-SES-011 — portable, renderer-only session share code.
//
// This is the Hermes Agent share-code port adapted to the host app seam. The
// payload is a view graph only: callers provide session labels/status metadata
// and edges, never transcript messages, tool results, prompts, or credentials.
// It is versioned, checksummed, URL-safe, and has no daemon or server storage.

export interface SessionShareNode {
  id: string;
  label: string;
  category: string;
  status: SessionShareStatus;
  model?: string | null;
  timestamp?: number | null;
}

export type SessionShareStatus = "idle" | "running" | "needs-input" | "failed" | "background" | "unread";

export interface SessionShareEdge {
  source: string;
  target: string;
}

export interface SessionShareGraph {
  nodes: SessionShareNode[];
  edges: SessionShareEdge[];
  clusters: Array<{ category: string; count: number }>;
  stats: Record<string, number>;
}

const VERSION = 1;
const PREFIX = "CRS";
const MAX_LABEL = 96;
const MAX_MODEL = 96;
const REC_BITS = 12;
const REC_MAX = (1 << REC_BITS) - 1;

const STATUSES = ["idle", "running", "needs-input", "failed", "background", "unread"] as const;

type Bit = 0 | 1;

const trim = (value: string, max = MAX_LABEL): string =>
  value.length > max ? value.slice(0, max) : value;

const finiteTs = (value?: number | null): number | null =>
  typeof value === "number" && Number.isFinite(value) ? Math.max(0, Math.round(value)) : null;

class BitWriter {
  private bits: number[] = [];

  bit(value: Bit | boolean): void {
    this.bits.push(value ? 1 : 0);
  }

  uint(value: number, width: number): void {
    let next = value >>> 0;
    for (let index = 0; index < width; index += 1) {
      this.bits.push(next & 1);
      next >>>= 1;
    }
  }

  varint(value: number): void {
    let next = Math.max(0, Math.floor(value));
    do {
      const group = next & 0x7f;
      next = Math.floor(next / 128);
      this.bit(next > 0);
      this.uint(group, 7);
    } while (next > 0);
  }

  str(value: string): void {
    const bytes = new TextEncoder().encode(value);
    this.varint(bytes.length);
    for (const byte of bytes) this.uint(byte, 8);
  }

  bytes(): Uint8Array {
    const output = new Uint8Array(Math.ceil(this.bits.length / 8));
    for (let index = 0; index < this.bits.length; index += 1) {
      if (this.bits[index]) output[index >> 3] |= 1 << (index & 7);
    }
    return output;
  }
}

class BitReader {
  private position = 0;

  constructor(private readonly bytes: Uint8Array) {}

  bit(): number {
    if (this.position >= this.bytes.length * 8) throw new RangeError("share code truncated");
    const index = this.position++;
    return (this.bytes[index >> 3] >> (index & 7)) & 1;
  }

  uint(width: number): number {
    let value = 0;
    for (let index = 0; index < width; index += 1) value |= this.bit() << index;
    return value >>> 0;
  }

  varint(): number {
    let value = 0;
    let shift = 0;
    for (;;) {
      const continued = this.bit();
      value += this.uint(7) * 2 ** shift;
      shift += 7;
      if (!continued) return value;
      if (shift > 35) throw new RangeError("share code integer is too large");
    }
  }

  str(): string {
    const length = this.varint();
    if (length > 1_000_000) throw new RangeError("share code string is too large");
    const bytes = new Uint8Array(length);
    for (let index = 0; index < length; index += 1) bytes[index] = this.uint(8);
    return new TextDecoder().decode(bytes);
  }
}

// The Hermes donor uses DEFLATE from fflate. the host app desktop package does
// not carry that dependency, so this seam keeps the same bit-packed schema and
// frame while using a tiny bounded LZ stream. It is synchronous and browser
// native, retains the donor's compact/repetitive-payload behavior, and is not
// intended to provide confidentiality.
function pack(bytes: Uint8Array): Uint8Array {
  const output: number[] = [];
  const candidates = new Map<string, number[]>();
  let index = 0;

  const keyAt = (position: number): string =>
    `${bytes[position] ?? 0},${bytes[position + 1] ?? 0},${bytes[position + 2] ?? 0}`;
  const remember = (position: number): void => {
    if (position >= bytes.length) return;
    const key = keyAt(position);
    const list = candidates.get(key) ?? [];
    while (list.length && position - list[0] > 4095) list.shift();
    if (list.length >= 24) list.shift();
    list.push(position);
    candidates.set(key, list);
  };
  const matchAt = (position: number): { offset: number; length: number } => {
    let bestOffset = 0;
    let bestLength = 0;
    for (const source of candidates.get(keyAt(position)) ?? []) {
      let length = 0;
      while (length < 130 && position + length < bytes.length && bytes[source + length] === bytes[position + length]) length += 1;
      if (length > bestLength && length >= 4) {
        bestLength = length;
        bestOffset = position - source;
      }
    }
    return { offset: bestOffset, length: bestLength };
  };

  while (index < bytes.length) {
    const match = matchAt(index);
    if (match.length >= 4) {
      output.push(0x80 | (match.length - 3), match.offset & 0xff, match.offset >>> 8);
      for (let position = index; position < index + match.length; position += 1) remember(position);
      index += match.length;
      continue;
    }
    const literalStart = index;
    remember(index++);
    while (index < bytes.length && index - literalStart < 128) {
      if (matchAt(index).length >= 4) break;
      remember(index++);
    }
    output.push(index - literalStart - 1, ...bytes.slice(literalStart, index));
  }
  return Uint8Array.from(output);
}

function unpack(bytes: Uint8Array): Uint8Array {
  const output: number[] = [];
  let index = 0;
  while (index < bytes.length) {
    const control = bytes[index++];
    if (control & 0x80) {
      const length = (control & 0x7f) + 3;
      if (index + 2 > bytes.length) throw new RangeError("share code match is truncated");
      const offset = bytes[index++] | (bytes[index++] << 8);
      if (offset === 0 || offset > output.length) throw new RangeError("share code match is invalid");
      for (let count = 0; count < length; count += 1) output.push(output[output.length - offset]);
    } else {
      const length = control + 1;
      if (index + length > bytes.length) throw new RangeError("share code literal is truncated");
      for (let count = 0; count < length; count += 1) output.push(bytes[index++]);
    }
    if (output.length > 4_000_000) throw new RangeError("share code is too large");
  }
  return Uint8Array.from(output);
}

function checksum16(bytes: Uint8Array): number {
  let hash = 0x811c9dc5;
  for (const byte of bytes) {
    hash ^= byte;
    hash = Math.imul(hash, 0x01000193);
  }
  return (hash >>> 0) & 0xffff;
}

function toBase64Url(bytes: Uint8Array): string {
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function fromBase64Url(value: string): Uint8Array {
  if (!/^[A-Za-z0-9_-]+$/.test(value)) throw new Error("invalid base64url");
  const binary = atob(value.replace(/-/g, "+").replace(/_/g, "/") + "=".repeat((4 - (value.length % 4)) % 4));
  const bytes = new Uint8Array(binary.length);
  for (let index = 0; index < binary.length; index += 1) bytes[index] = binary.charCodeAt(index);
  return bytes;
}

export class SessionShareCodeError extends Error {}

function indexBits(length: number): number {
  return length <= 1 ? 1 : Math.ceil(Math.log2(length));
}

function writeGraph(writer: BitWriter, graph: SessionShareGraph): void {
  const dictionary = new Map<string, number>();
  const strings: string[] = [];
  const stringId = (value: string): number => {
    const existing = dictionary.get(value);
    if (existing !== undefined) return existing;
    const id = strings.length;
    dictionary.set(value, id);
    strings.push(value);
    return id;
  };
  for (const node of graph.nodes) {
    stringId(trim(node.label));
    stringId(trim(node.category, 48));
    stringId(trim(node.model ?? "", MAX_MODEL));
  }
  const timestamps = graph.nodes.map((node) => finiteTs(node.timestamp)).filter((value): value is number => value !== null);
  const minimum = timestamps.length ? Math.min(...timestamps) : 0;
  const maximum = timestamps.length ? Math.max(...timestamps) : 0;
  const span = maximum - minimum;
  writer.varint(minimum);
  writer.varint(maximum);
  writer.varint(strings.length);
  for (const value of strings) writer.str(value);
  writer.varint(graph.nodes.length);
  for (const node of graph.nodes) {
    writer.varint(stringId(trim(node.label)));
    writer.varint(stringId(trim(node.category, 48)));
    writer.varint(stringId(trim(node.model ?? "", MAX_MODEL)));
    writer.uint(Math.max(0, STATUSES.indexOf(node.status)), 3);
    const timestamp = finiteTs(node.timestamp);
    writer.bit(timestamp !== null);
    if (timestamp !== null) writer.uint(span > 0 ? Math.round(((timestamp - minimum) / span) * REC_MAX) : 0, REC_BITS);
  }
  const order = new Map(graph.nodes.map((node, index) => [node.id, index]));
  const edges = graph.edges.filter((edge) => order.has(edge.source) && order.has(edge.target));
  const bits = indexBits(graph.nodes.length);
  writer.varint(edges.length);
  for (const edge of edges) {
    writer.uint(order.get(edge.source) as number, bits);
    writer.uint(order.get(edge.target) as number, bits);
  }
}

function readGraph(reader: BitReader): SessionShareGraph {
  const minimum = reader.varint();
  const maximum = reader.varint();
  const span = maximum - minimum;
  const dictionary: string[] = [];
  for (let index = 0, length = reader.varint(); index < length; index += 1) dictionary.push(reader.str());
  const nodes: SessionShareNode[] = [];
  const count = reader.varint();
  for (let index = 0; index < count; index += 1) {
    const label = dictionary[reader.varint()] ?? "";
    const category = dictionary[reader.varint()] ?? "";
    const model = dictionary[reader.varint()] ?? "";
    const status = STATUSES[reader.uint(3)] ?? "idle";
    const timestamp = reader.bit() ? minimum + (span > 0 ? Math.round((reader.uint(REC_BITS) / REC_MAX) * span) : 0) : null;
    nodes.push({ category, id: `session-${index}`, label, model: model || null, status, timestamp });
  }
  const bits = indexBits(nodes.length);
  const edges: SessionShareEdge[] = [];
  for (let index = 0, length = reader.varint(); index < length; index += 1) {
    const source = nodes[reader.uint(bits)];
    const target = nodes[reader.uint(bits)];
    if (source && target) edges.push({ source: source.id, target: target.id });
  }
  const clusterCounts = new Map<string, number>();
  for (const node of nodes) clusterCounts.set(node.category, (clusterCounts.get(node.category) ?? 0) + 1);
  const clusters = [...clusterCounts.entries()].map(([category, count]) => ({ category, count })).sort((a, b) => b.count - a.count);
  return { clusters, edges, nodes, stats: { imported: 1 } };
}

export function encodeSessionShareCode(graph: SessionShareGraph): string {
  const body = pack(new BitWriterWithBody(graph).bytes());
  const frame = new BitWriter();
  frame.uint(VERSION, 8);
  frame.uint(checksum16(body), 16);
  const head = frame.bytes();
  const output = new Uint8Array(head.length + body.length);
  output.set(head);
  output.set(body, head.length);
  return PREFIX + toBase64Url(output);
}

// Kept as a small adapter so the body writer remains testable without exposing
// the codec's framing details.
class BitWriterWithBody extends BitWriter {
  constructor(graph: SessionShareGraph) {
    super();
    writeGraph(this, graph);
  }
}

export function decodeSessionShareCode(code: string): SessionShareGraph {
  const cleaned = code.replace(/\s+/g, "");
  const raw = cleaned.startsWith(PREFIX) ? cleaned.slice(PREFIX.length) : cleaned;
  try {
    const frame = fromBase64Url(raw);
    if (frame.length <= 3) throw new Error("code is too short");
    const header = new BitReader(frame.subarray(0, 3));
    const version = header.uint(8);
    const stored = header.uint(16);
    const body = frame.subarray(3);
    if (version !== VERSION) throw new Error(`code is version ${version}; this build reads version ${VERSION}`);
    if (checksum16(body) !== stored) throw new Error("checksum mismatch");
    return readGraph(new BitReader(unpack(body)));
  } catch (error) {
    throw new SessionShareCodeError(error instanceof Error ? `Session share code is malformed: ${error.message}` : "Session share code is malformed.");
  }
}

// Aliases make the direct Hermes share-code port convenient for future graph
// surfaces while the session-specific names remain the public the host app API.
export const encodeShareCode = encodeSessionShareCode;
export const decodeShareCode = decodeSessionShareCode;
export const ShareCodeError = SessionShareCodeError;
