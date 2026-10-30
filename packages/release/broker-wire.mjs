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
    const protocolLike = line[0] === 0x7b; // `{`: a protocol frame; a tool's own long output line is not a wire frame and is never judged here
    if (protocolLike && line.length > limit) return { frames, overCap, error: `a frame of ${line.length} bytes (newline included) exceeds the ${limit}-byte limit` };
    const body = line.subarray(0, line.length - 1).toString("utf8");
    frames.push(body);
    if (body.startsWith("{")) { try { const event = JSON.parse(body); if (overCapProblem(event)) overCap.push(event); } catch { /* not a protocol frame */ } }
  }
  if (rest[0] === 0x7b && rest.length >= limit) return { frames, overCap, error: `an incomplete frame of ${rest.length} bytes has no newline within the ${limit}-byte limit` };
  if (rest.length > 0) frames.push(rest.toString("utf8"));
  return { frames, overCap, error: null };
}

// ---- GB-84: the typed diagnosis of one STATUS or RESULT reply (rustright_protocol::diagnose_control_reply, vector control-status-vectors.v1.json) ----
const TERMINAL_STATES = new Set(["SUCCEEDED", "FAILED", "CANCELLED", "TIMED_OUT", "REJECTED"]);
const REFUSAL_CLASSES = Object.freeze({ RK_ADMISSION_PAUSED: "paused", RK_JOB_SUSPENDED: "suspended", RK_JOB_NOT_FOUND: "not-found", RK_ACTIVATION_PENDING: "activation-pending", RK_CUTOVER_DRAIN_PENDING: "cutover-drain" });
const isObject = (v) => v !== null && typeof v === "object" && !Array.isArray(v);

/**
 * What a client concludes from one STATUS or RESULT line about a request it knows by id. Only `terminal` is a build outcome, and only after the receipt's own signed
 * validation; every other kind keeps the broker's code, state and identity instead of collapsing into a generic receipt failure, and a known request is reconciled by
 * its durable id, never resubmitted because a diagnosis improved:
 *   { kind: "live", id, state }                  non-terminal with an absent or null receipt (pending; never a result, never green)
 *   { kind: "terminal", id, state, receipt }      a terminal state with a receipt object whose id is the request's
 *   { kind: "unavailable", id, reason }           the front gate's STATUS with state UNAVAILABLE and its diagnostic code
 *   { kind: "refused", code, refusal, message }   a RESULT with status REJECTED: paused, suspended, not-found, activation-pending, cutover-drain, other
 *   { kind: "malformed", message }                not a typed object, another request's id or receipt, no state, a terminal state without a receipt, a receipt that is not an object
 */
export function diagnoseControlReply(event, expectedId = null) {
  const malformed = (message) => ({ kind: "malformed", message });
  if (!isObject(event) || typeof event.type !== "string") return malformed("the reply is not a typed object");
  const id = typeof event.id === "string" ? event.id : null;
  if (expectedId !== null && id !== null && expectedId !== id) return malformed("the reply names another request");
  const receiptOf = (state) => {
    const receipt = event.receipt;
    if (isObject(receipt)) {
      const receiptId = typeof receipt.id === "string" ? receipt.id : null;
      const want = expectedId ?? id;
      if (want !== null && receiptId !== null && want !== receiptId) return malformed("the receipt is for another request");
      if (want !== null && receiptId === null) return malformed("the receipt names no request");
      return { kind: "terminal", id, state, receipt };
    }
    if (receipt === undefined || receipt === null) return TERMINAL_STATES.has(state) ? malformed("a finished job's reply carries no receipt") : { kind: "live", id, state };
    return malformed("the receipt is not an object");
  };
  if (event.type === "STATUS") {
    if (typeof event.state !== "string" || event.state === "") return malformed("the status carries no state");
    if (event.state === "UNAVAILABLE") {
      const reason = (isObject(event.diagnostic) && typeof event.diagnostic.code === "string" ? event.diagnostic.code : null) ?? (typeof event.code === "string" ? event.code : null) ?? "unavailable";
      return { kind: "unavailable", id, reason };
    }
    return receiptOf(event.state);
  }
  if (event.type === "RESULT") {
    if (event.status === "REJECTED") {
      const code = typeof event.code === "string" ? event.code : null;
      return { kind: "refused", code, refusal: (code !== null && Object.hasOwn(REFUSAL_CLASSES, code) ? REFUSAL_CLASSES[code] : "other"), message: typeof event.error === "string" ? event.error : "" };
    }
    return typeof event.status === "string" && TERMINAL_STATES.has(event.status) ? receiptOf(event.status) : malformed("the result carries no terminal status");
  }
  return malformed("the reply is not a STATUS or RESULT");
}

// ---- GB-54: pause and resume in the typed protocol (BROKER-CONTRACT section 14) ----
// `PAUSE_ADMISSION` and `RESUME_ADMISSION` are variants of RustRight's `BrokerRequest` and `ADMISSION` a variant of `BrokerEvent`, defined once in rustright-protocol
// and answered by the broker's front gate (authentication unchanged, whether or not the job service is up). Wire:
//   {"type":"PAUSE_ADMISSION","protocol","runtime","reason"?:string,"holder"?:{"pid":u32,"startTimeMs"?:u64}}
//   {"type":"RESUME_ADMISSION","protocol","runtime"}
//   reply {"type":"ADMISSION","paused":bool,"reason"?,"holder"?,"persistError"?}
// Compatibility: unknown optional fields are ignored; a `reason` or `holder` of the wrong shape is refused (it used to be dropped silently, leaving a pause without its holder).
export const PAUSE_ADMISSION = "PAUSE_ADMISSION";
export const RESUME_ADMISSION = "RESUME_ADMISSION";
export const ADMISSION_EVENT = "ADMISSION";
export const ADMISSION_PAUSED_CODE = "RK_ADMISSION_PAUSED";
const isU32 = (v) => Number.isInteger(v) && v >= 0 && v <= 0xffffffff;
const isUint = (v) => Number.isSafeInteger(v) && v >= 0;

const holderProblem = (holder) => {
  if (!isObject(holder) || !isU32(holder.pid)) return "holder must be {pid: u32, startTimeMs?: u64}";
  if (holder.startTimeMs !== undefined && !isUint(holder.startTimeMs)) return "holder.startTimeMs must be a non-negative integer";
  return null;
};

/** Parses one admission control request as RustRight's canonical codec does: `{ ok: true, request }` (known fields only) or `{ ok: false, message }`; nothing changes on a refusal. */
export function parseAdmissionRequest(value) {
  if (!isObject(value) || (value.type !== PAUSE_ADMISSION && value.type !== RESUME_ADMISSION)) return { ok: false, message: "not a PAUSE_ADMISSION or RESUME_ADMISSION request" };
  if (typeof value.protocol !== "string" || value.protocol === "") return { ok: false, message: "protocol is required" };
  if (!isObject(value.runtime)) return { ok: false, message: "runtime identity is required" };
  const request = { type: value.type, protocol: value.protocol, runtime: value.runtime };
  if (value.type === PAUSE_ADMISSION) {
    if (value.reason !== undefined && value.reason !== null) { if (typeof value.reason !== "string") return { ok: false, message: "reason must be a string" }; request.reason = value.reason; }
    if (value.holder !== undefined && value.holder !== null) { const problem = holderProblem(value.holder); if (problem) return { ok: false, message: problem }; request.holder = { pid: value.holder.pid, ...(value.holder.startTimeMs !== undefined ? { startTimeMs: value.holder.startTimeMs } : {}) }; }
  }
  return { ok: true, request };
}

/** The wire request a gate-side caller builds: the same function the parser judges, so a request that is built is a request that parses. */
export function admissionRequest(type, { protocol, runtime, reason, holder } = {}) {
  const parsed = parseAdmissionRequest({ type, protocol, runtime, ...(type === PAUSE_ADMISSION ? { reason, holder } : {}) });
  if (!parsed.ok) throw Object.assign(new Error(`${type}: ${parsed.message}`), { code: "RK_ADMISSION_REQUEST_INVALID" });
  return parsed.request;
}

/** The `ADMISSION` reply: `{ ok: true, paused, reason?, holder?, persistError? }` or `{ ok: false, message }`. `persistError` means the pause holds for this run but could not be written to survive a restart. */
export function parseAdmissionEvent(event) {
  if (!isObject(event) || event.type !== ADMISSION_EVENT) return { ok: false, message: "not an ADMISSION event" };
  if (typeof event.paused !== "boolean") return { ok: false, message: "ADMISSION carries no boolean paused" };
  if (event.holder !== undefined && holderProblem(event.holder)) return { ok: false, message: holderProblem(event.holder) };
  for (const name of ["reason", "persistError"]) if (event[name] !== undefined && typeof event[name] !== "string") return { ok: false, message: `${name} must be a string` };
  return { ok: true, paused: event.paused, ...(event.reason !== undefined ? { reason: event.reason } : {}), ...(event.holder !== undefined ? { holder: event.holder } : {}), ...(event.persistError !== undefined ? { persistError: event.persistError } : {}) };
}

/**
 * The typed refusal the client's captured output carries, if it carries one: a `RESULT` with status `REJECTED` (diagnoseControlReply), for example `RK_ADMISSION_PAUSED`
 * (exit 125: admission is paused and NOTHING was spawned). `{ code, refusal, message }` or null. Never a build outcome.
 */
export function refusalInCapturedOutput(text) {
  for (const frame of drainFrames(String(text ?? "")).frames) {
    if (!frame.startsWith("{")) continue;
    let event; try { event = JSON.parse(frame); } catch { continue; }
    const diagnosis = diagnoseControlReply(event);
    if (diagnosis.kind === "refused") return { code: diagnosis.code, refusal: diagnosis.refusal, message: diagnosis.message };
  }
  return null;
}

// ---- GB-52: an end-to-end operation deadline and explicit cancellation for carried calls (BROKER-CONTRACT section 16) ----
// RustRight's client bounds one submit-and-watch operation with RUSTRIGHT_OPERATION_DEADLINE_MS (absent or 0: none; a user-set test cap or stall limit is never replaced
// by a universal duration). Past it the client does not abandon or resubmit the job: it sends CANCEL by id and polls STATUS for the terminal state within
// RUSTRIGHT_CANCEL_WAIT_MS (default 30000), failing with `operation deadline of N ms passed; request <id>: cancelConfirmed: <bool>, terminal state: <state | not observed>`.
// An unobserved state keeps ownership unknown. `RK_BROKER_BUSY` (every pipe instance busy: a wait) is distinct from `RK_BROKER_UNAVAILABLE`.
export const OPERATION_DEADLINE_ENV = "RUSTRIGHT_OPERATION_DEADLINE_MS";
export const CANCEL_WAIT_ENV = "RUSTRIGHT_CANCEL_WAIT_MS";
export const DEFAULT_CANCEL_WAIT_MS = 30_000;
/** The gate's remaining stage budget, as an absolute epoch-ms instant a stage sets for its children (unset: no budget). */
export const STAGE_DEADLINE_AT_ENV = "RK_GATE_STAGE_DEADLINE_AT";

/**
 * The environment delta that gives one carried call its operation deadline: the caller's own RUSTRIGHT_OPERATION_DEADLINE_MS is never replaced (a user-set cap wins);
 * otherwise `deadlineMs` (a scan's or stage's own limit) or the time left to `deadlineAt` (STAGE_DEADLINE_AT_ENV in `env`), at least 1 ms. `{}` when there is no budget.
 */
export function operationDeadlineEnv({ env = {}, deadlineMs = null, deadlineAt = null, now = Date.now() } = {}) {
  const set = Object.entries(env).find(([key]) => key.toUpperCase() === OPERATION_DEADLINE_ENV);
  if (set && String(set[1]).trim() !== "") return {};
  const at = deadlineAt ?? (Number.isFinite(Number(env[STAGE_DEADLINE_AT_ENV])) && Number(env[STAGE_DEADLINE_AT_ENV]) > 0 ? Number(env[STAGE_DEADLINE_AT_ENV]) : null);
  const budget = deadlineMs ?? (at !== null ? at - now : null);
  if (!Number.isFinite(budget) || budget === null) return {};
  return { [OPERATION_DEADLINE_ENV]: String(Math.max(1, Math.trunc(budget))) };
}

/** The outer guard on the client process: the deadline, the cancel wait and a margin, so the client's own cancel and terminal-state wait finish before the gate kills it. */
export function clientGuardMs(env, { margin = 15_000 } = {}) {
  const deadline = Number(env?.[OPERATION_DEADLINE_ENV]);
  if (!Number.isFinite(deadline) || deadline <= 0) return null;
  const wait = Number(env?.[CANCEL_WAIT_ENV]);
  return deadline + (Number.isFinite(wait) && wait >= 0 ? wait : DEFAULT_CANCEL_WAIT_MS) + margin;
}

/** What the client reported when its operation deadline passed, or null: `{ code, deadlineMs, requestId, cancelConfirmed, terminalState }`; busy and unavailable are named apart. */
export function operationOutcomeInOutput(text) {
  const t = String(text ?? "");
  const deadline = /operation deadline of (\d+) ms passed; request ([0-9A-Za-z._-]+): cancelConfirmed: (true|false), terminal state: (not observed|[A-Z_]+)/.exec(t);
  if (deadline) return { code: "operation-deadline", deadlineMs: Number(deadline[1]), requestId: deadline[2], cancelConfirmed: deadline[3] === "true", terminalState: deadline[4].trim().startsWith("not observed") ? null : deadline[4].trim() };
  if (/RK_OPERATION_DEADLINE/.test(t)) return { code: "operation-deadline", deadlineMs: null, requestId: null, cancelConfirmed: null, terminalState: null };
  if (/RK_BROKER_BUSY/.test(t)) return { code: "broker-busy" };
  if (/RK_BROKER_UNAVAILABLE/.test(t)) return { code: "broker-unavailable" };
  return null;
}
