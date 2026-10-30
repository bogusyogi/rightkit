// The wire limits of the RustRight broker protocol and the gate's handling of what crosses them (GB-53; BROKER-CONTRACT section 14).
//
// Both limits are 1 MiB, NEWLINE INCLUDED, in ONE unit: MAX_REQUEST_BYTES (a request line) and MAX_BROKER_LINE_BYTES (a response line). The longest body is
// therefore 1 MiB - 1 bytes. A request longer than that is refused by the client before it connects (`request exceeds input cap`) and by the broker with
// RESULT REJECTED (exit 125), so no job exists. The broker never sends a response line over the limit: a non-terminal event that would be longer is
// replaced by {"type":"FRAME_OVER_CAP","for":<type>,"bytes":<n>,"limit":1048576}; a terminal RESULT that would be longer by {"type":"RESULT",...,"status":"FAILED",
// "code":"RK_RESPONSE_OVER_CAP",...}. A consumer that captures output treats FRAME_OVER_CAP as TRUNCATION: it refuses (or verifies the complete output log named by the
// receipt's diagnosticsPath); it never reads a result that lost its middle as complete. The receipt and the evidence files stay readable.
export const MAX_REQUEST_BYTES = 1024 * 1024;
export const MAX_BROKER_LINE_BYTES = 1024 * 1024;
export const FRAME_OVER_CAP = "FRAME_OVER_CAP";
export const RESPONSE_OVER_CAP_CODE = "RK_RESPONSE_OVER_CAP";
export const REQUEST_OVER_CAP_CODE = "RK_REQUEST_OVER_CAP";
/**
 * What the gate reserves for the request line beside a carrier's own bytes (the client's envelope: command, argv, environment names, protocol, runtime, ids,
 * the managed-environment record). A deliberately generous bound, so a request the gate lets through is never one the client would refuse for size.
 */
export const REQUEST_ENVELOPE_RESERVE_BYTES = 64 * 1024;

/** Whether a line of `bodyBytes` bytes (without its newline) fits a limit: the newline is counted, so the longest body is limit - 1. */
export const lineFits = (bodyBytes, limit = MAX_BROKER_LINE_BYTES) => Number.isInteger(bodyBytes) && bodyBytes >= 0 && bodyBytes + 1 <= limit;

/** A refusal reason when a request whose carrier is `carrierBytes` long, plus the envelope reserve, cannot fit one request line; else null. */
export function requestOverCap(carrierBytes, { reserve = REQUEST_ENVELOPE_RESERVE_BYTES } = {}) {
  return lineFits(carrierBytes + reserve) ? null : `the request is ${carrierBytes} bytes of declared inputs and identity plus a ${reserve}-byte envelope, more than the ${MAX_REQUEST_BYTES}-byte request line (the newline counts); the client refuses it before it connects and no job would exist`;
}

/** Why one decoded response event is a truncation or an over-cap outcome (`FRAME_OVER_CAP`, a terminal `RK_RESPONSE_OVER_CAP`), or null. */
export function overCapProblem(event) {
  if (!event || typeof event !== "object") return null;
  if (event.type === FRAME_OVER_CAP) return { code: "frame-over-cap", message: `the broker replaced a ${event.for ?? "response"} frame of ${event.bytes ?? "?"} bytes with ${FRAME_OVER_CAP} (limit ${event.limit ?? MAX_BROKER_LINE_BYTES}): the captured output lost that frame, so it is truncated and refused; the complete output log named by the receipt's diagnosticsPath is the evidence` };
  if (event.code === RESPONSE_OVER_CAP_CODE) return { code: "response-over-cap", message: `the broker's terminal result exceeded the ${MAX_BROKER_LINE_BYTES}-byte line limit and was replaced by ${RESPONSE_OVER_CAP_CODE} (original status ${event.originalStatus ?? "unknown"}, ${event.bytes ?? "?"} bytes): a typed failure, never a success; the receipt and evidence files stay readable on the host` };
  return null;
}

/**
 * Parses captured client output as RustRight's client reads the wire: every complete newline-terminated frame is drained and checked against the limit FIRST, and
 * only then is the one incomplete frame still waiting for its newline bounded (a frame already at the limit without a newline cannot fit it). A limit-sized frame
 * followed by the start of the next one in the same read is valid. Returns `{ frames: [text], overCap: [event], error: string | null }`; lines that are not JSON
 * objects (cargo's own output) are kept as frames and never judged.
 */
export function drainFrames(text, limit = MAX_BROKER_LINE_BYTES) {
  const frames = []; const overCap = [];
  let rest = Buffer.from(String(text), "utf8");
  for (;;) {
    const at = rest.indexOf(0x0a);
    if (at < 0) break;
    const line = rest.subarray(0, at + 1);
    rest = rest.subarray(at + 1);
    if (line.length > limit) return { frames, overCap, error: `a frame of ${line.length} bytes (newline included) exceeds the ${limit}-byte limit` };
    const body = line.subarray(0, line.length - 1).toString("utf8");
    frames.push(body);
    if (body.startsWith("{")) { try { const event = JSON.parse(body); if (overCapProblem(event)) overCap.push(event); } catch { /* not a protocol frame */ } }
  }
  if (rest.length >= limit) return { frames, overCap, error: `an incomplete frame of ${rest.length} bytes has no newline within the ${limit}-byte limit` };
  if (rest.length > 0) frames.push(rest.toString("utf8"));
  return { frames, overCap, error: null };
}
