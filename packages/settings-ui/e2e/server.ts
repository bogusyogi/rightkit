/**
 * Real local host for the e2e: serves the built harness and answers the `rightkit_settings_*` / `rightkit_secrets_*`
 * host-services commands over HTTP so the browser exercises `createShellBackends` for real. Settings persist to a JSON file on disk (revisioned, atomic rename);
 * secrets live only in this process's memory and are never echoed; the licence commands answer with LicenseView objects (0.3 contract);
 * models (an app-supplied backend: the shell host registers no models command) simulate an install with progress events pushed over server-sent events.
 *
 * This is a test host, not the Rust handlers: those are the host crate's job (see API.md "host contract").
 */
import { existsSync, readFileSync, renameSync, statSync, writeFileSync } from "node:fs";
import { createServer, type Server, type ServerResponse } from "node:http";
import { extname, join, normalize } from "node:path";

const MIME: Record<string, string> = { ".html": "text/html", ".js": "text/javascript", ".css": "text/css", ".svg": "image/svg+xml", ".json": "application/json" };

/** A LicenseView with every field set, as the native host returns it. */
const licenseView = (over: Record<string, unknown> = {}) => ({
  mode: "configured",
  status: "none",
  lifecycle: "unlicensed",
  tier: null,
  features: [],
  kind: null,
  stage: null,
  validUntil: null,
  confirmationDueAt: null,
  staleSince: null,
  licenseIdSuffix: null,
  busy: false,
  unresolved: false,
  revisionState: "none",
  message: "none",
  ...over,
});

export interface TestHost {
  server: Server;
  origin: string;
  settingsFile: string;
  /** Raw request bodies seen per method, to assert on what crossed the wire. */
  wire: Array<{ method: string; body: string }>;
  secrets: Map<string, string>;
  logs: unknown[];
  failNextSettingsPatch: boolean;
  licenseRequests: Array<{ path: string; body: Record<string, unknown> }>;
  /** Open server-sent-event streams right now (a browser allows only 6 connections per origin). */
  sseOpen(): number;
  close(): void;
}

interface ModelRow {
  id: string;
  label: string;
  description: string;
  sizeBytes: number;
  state: "missing" | "installed" | "downloading" | "verifying" | "error";
  progress?: number;
  active?: boolean;
  fit?: { kind: "measured" | "estimated" | "unsupported" | "unqualified" };
}

export async function startHost(staticDir: string, settingsFile: string): Promise<TestHost> {
  const secrets = new Map<string, string>();
  const wire: TestHost["wire"] = [];
  const logs: unknown[] = [];
  const licenseRequests: TestHost["licenseRequests"] = [];
  let licenseState: Record<string, unknown> = licenseView();
  const sse = new Set<ServerResponse>();
  const push = (call: string, event: unknown) => {
    for (const res of sse) res.write(`data: ${JSON.stringify({ call, event })}\n\n`);
  };
  const models: ModelRow[] = [
    { id: "asr-small", label: "Speech model (small)", description: "Fast, English", sizeBytes: 482_000_000, state: "installed", active: true, fit: { kind: "measured" } },
    { id: "gen-api", label: "Cloud generator", description: "Provider API", state: "missing", fit: { kind: "api" } },
    { id: "asr-shared", label: "Parakeet (shared)", description: "Orthic store", sizeBytes: 640_000_000, state: "installed", shared: true, fit: { kind: "measured" } },
    { id: "asr-large", label: "Speech model (large)", description: "Most accurate", sizeBytes: 3_100_000_000, state: "missing", fit: { kind: "estimated" } },
    { id: "llm-huge", label: "Cleanup model (huge)", description: "Needs 48 GB", sizeBytes: 41_000_000_000, state: "missing", fit: { kind: "unsupported" } },
  ];
  const timers = new Map<string, ReturnType<typeof setInterval>>();

  const readSettings = (): { values: Record<string, unknown>; revision: number } =>
    existsSync(settingsFile) ? JSON.parse(readFileSync(settingsFile, "utf8")) : { values: {}, revision: 1 };
  const writeSettings = (s: { values: Record<string, unknown>; revision: number }) => {
    const tmp = `${settingsFile}.tmp`;
    writeFileSync(tmp, JSON.stringify(s));
    renameSync(tmp, settingsFile);
  };

  const host = {} as TestHost;
  const rpc = (method: string, params: any): { status: number; body: unknown } => {
    switch (method) {
      case "rightkit_settings_get":
        return { status: 200, body: readSettings() };
      case "rightkit_settings_subscribe":
        return { status: 200, body: readSettings().revision };
      case "rightkit_settings_unsubscribe":
        return { status: 200, body: null };
      case "rightkit_settings_patch": {
        // Scope the injected disk failure to its intended dictation rollback write so
        // delayed writes from earlier pages cannot consume this test control flag.
        if (host.failNextSettingsPatch && Object.prototype.hasOwnProperty.call(params.patch, "dictation.cleanup")) {
          host.failNextSettingsPatch = false;
          return { status: 500, body: { error: "disk full" } };
        }
        const cur = readSettings();
        if (params.expectedRevision !== null && params.expectedRevision !== undefined && params.expectedRevision !== cur.revision) {
          return { status: 409, body: { error: "settings changed elsewhere", code: "revision_conflict" } };
        }
        for (const [k, v] of Object.entries(params.patch as Record<string, unknown>)) {
          if (v === null) delete cur.values[k];
          else cur.values[k] = v;
        }
        cur.revision += 1;
        writeSettings(cur);
        push("rightkit://settings-changed", { keys: Object.keys(params.patch), revision: cur.revision });
        return { status: 200, body: cur };
      }
      case "rightkit_secrets_presence":
        return { status: 200, body: (params.refs as Array<{ service: string; account: string }>).map((r) => (secrets.has(`${r.service}/${r.account}`) ? "present" : "absent")) };
      case "rightkit_secrets_set":
        if (params.intent !== "create" && params.intent !== "replace") return { status: 400, body: { error: "intent required", code: "invalid" } };
        secrets.set(`${params.secret.service}/${params.secret.account}`, params.value);
        return { status: 200, body: null };
      case "rightkit_secrets_clear":
        secrets.delete(`${params.secret.service}/${params.secret.account}`);
        return { status: 200, body: null };
      case "rightkit.models.list":
        return { status: 200, body: models };
      case "rightkit.models.install": {
        const m = models.find((x) => x.id === params.id)!;
        m.state = "downloading";
        m.progress = 0;
        const t = setInterval(() => {
          m.progress = Math.min(1, (m.progress ?? 0) + 0.25);
          if (m.progress >= 1) {
            clearInterval(t);
            timers.delete(m.id);
            m.state = "installed";
            delete m.progress;
          }
          push("rightkit.models.entry", { ...m });
        }, 120);
        timers.set(m.id, t);
        return { status: 200, body: null };
      }
      case "rightkit.models.cancel": {
        const m = models.find((x) => x.id === params.id)!;
        clearInterval(timers.get(m.id));
        m.state = "missing";
        delete m.progress;
        return { status: 200, body: null };
      }
      case "rightkit.models.remove": {
        const m = models.find((x) => x.id === params.id)!;
        m.state = "missing";
        m.active = false;
        return { status: 200, body: null };
      }
      case "rightkit.logs.write":
        logs.push(...(params.events ?? [params]));
        return { status: 200, body: null };
      case "rightkit.logs.collect":
        return { status: 200, body: { app: "acme", events: logs.length, recent: logs.slice(-5) } };
      case "rightkit.logs.clear":
        logs.length = 0;
        return { status: 200, body: null };
      case "rightkit_license_status":
        return { status: 200, body: licenseState };
      case "rightkit_license_activate":
        licenseRequests.push({ path: "activate", body: params });
        if (params.key !== "GOOD-KEY-0001") return { status: 200, body: licenseView({ status: "invalid", lifecycle: "invalid", message: "keyNotAccepted" }) };
        licenseState = licenseView({ status: "active", lifecycle: "lifetime", tier: "pro", features: ["updates"], kind: "lifetime", stage: "final", revisionState: "active", licenseIdSuffix: "0001" });
        return { status: 200, body: licenseState };
      case "rightkit_license_deactivate":
        licenseRequests.push({ path: "deactivate", body: params });
        licenseState = licenseView({ revisionState: "tombstone" });
        return { status: 200, body: licenseState };
      default:
        return { status: 404, body: { error: `unknown method ${method}` } };
    }
  };

  const server = createServer((req, res) => {
    const url = new URL(req.url ?? "/", "http://x");
    if (req.method === "GET" && url.pathname === "/events") {
      res.writeHead(200, { "content-type": "text/event-stream", "cache-control": "no-cache" });
      res.write(": open\n\n");
      sse.add(res);
      req.on("close", () => sse.delete(res));
      return;
    }
    if (req.method === "POST") {
      let raw = "";
      req.on("data", (d) => (raw += d));
      req.on("end", () => {
        const body = raw ? JSON.parse(raw) : {};
        let out: { status: number; body: unknown };
        if (url.pathname.startsWith("/rpc/")) {
          const method = decodeURIComponent(url.pathname.slice(5));
          wire.push({ method, body: raw });
          out = rpc(method, body);
        } else out = { status: 404, body: {} };
        res.writeHead(out.status, { "content-type": "application/json" });
        res.end(JSON.stringify(out.body));
      });
      return;
    }
    const path = normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, "");
    let file = join(staticDir, path === "/" || path === "." ? "index.html" : path);
    if (!existsSync(file) || statSync(file).isDirectory()) file = join(staticDir, "index.html");
    res.setHeader("content-type", MIME[extname(file)] ?? "application/octet-stream");
    res.end(readFileSync(file));
  });
  await new Promise<void>((resolve) => server.listen(0, "127.0.0.1", resolve));
  const addr = server.address();
  Object.assign(host, {
    server,
    origin: `http://127.0.0.1:${typeof addr === "object" && addr ? addr.port : 0}`,
    settingsFile,
    wire,
    secrets,
    logs,
    failNextSettingsPatch: false,
    licenseRequests,
    sseOpen: () => sse.size,
    close() {
      for (const t of timers.values()) clearInterval(t);
      for (const r of sse) r.end();
      server.close();
    },
  });
  return host;
}
