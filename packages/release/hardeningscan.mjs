#!/usr/bin/env node
import { createReadStream } from "node:fs";
import { lstat, readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const args = process.argv.slice(2);
if (args.length === 0 || args.includes("-h") || args.includes("--help")) {
  console.error("usage: node hardeningscan.mjs <artifact-path>...");
  process.exit(args.length === 0 ? 2 : 0);
}

const root = process.cwd();
const findings = [];

const fileNameRules = [
  [/\.map$/i, "source map shipped"],
  [/\.pdb$/i, "Windows debug symbols shipped"],
  [/\.dSYM(?:$|[/\\])/i, "macOS debug symbols shipped"],
  [/\.mlpackage(?:$|[/\\])/i, "raw CoreML package shipped"],
  [/[/\\]docs[/\\]experiments(?:$|[/\\])/i, "experiment docs shipped"],
  [/[/\\]\.scratch(?:$|[/\\])/i, "scratch directory shipped"],
  [/[/\\]\.git(?:$|[/\\])/i, "git metadata shipped"],
  [/[/\\]node_modules(?:$|[/\\])/i, "node_modules shipped"],
  [/[/\\](?:src|tests?|benches|benchmarks)(?:$|[/\\])/i, "source/test tree shipped"],
  [/[/\\]tdt_w15_p6(?:$|[/\\])/i, "stale TDT p6 model shipped"],
];

const contentRules = [
  [/\/Users\/adrdsouza\/claude/gi, "local source path leaked"],
  [/\/Users\/adrdsouza\/\.cargo/gi, "local cargo path leaked"],
  [/C:\\Users\\adrdsouza\\(?:claude|\.cargo)/gi, "local Windows source path leaked"],
  [/docs\/experiments/gi, "experiment path leaked"],
  [/l3_cleanup_prompt/gi, "L3 cleanup prompt marker leaked"],
  [/SYSTEM_PROMPT/gi, "system prompt marker leaked"],
  [/OPENAI_API_KEY|ANTHROPIC_API_KEY|APPLE_PASSWORD|NOTARY_PROFILE/gi, "secret/env key name leaked"],
  [/--enable-gpl|--enable-nonfree/gi, "GPL/nonfree ffmpeg flag leaked"],
  [/tdt_w15_p6/gi, "stale TDT p6 string leaked"],
  [/Canary-Qwen|Nemotron|dqlstm|bias-experiments/gi, "experiment/model-history marker leaked"],
];

const maxPatternLen = Math.max(...contentRules.map(([r]) => String(r).length), 256);

function add(kind, file, message, detail = "") {
  findings.push({
    kind,
    file: path.relative(root, file) || file,
    message,
    detail,
  });
}

async function walk(p) {
  const st = await lstat(p);
  const normalized = p.split(path.sep).join("/");
  for (const [rule, message] of fileNameRules) {
    if (rule.test(normalized)) add("path", p, message);
  }
  if (st.isDirectory()) {
    const entries = await readdir(p);
    await Promise.all(entries.map((name) => walk(path.join(p, name))));
    return;
  }
  if (st.isFile()) await scanFile(p);
}

function scanFile(file) {
  return new Promise((resolve) => {
    const stream = createReadStream(file, { highWaterMark: 256 * 1024 });
    let tail = "";
    stream.on("data", (chunk) => {
      const text = tail + chunk.toString("latin1");
      for (const [rule, message] of contentRules) {
        if (/ffmpeg-LICENSE\.txt$/i.test(file) && message === "GPL/nonfree ffmpeg flag leaked") {
          continue;
        }
        rule.lastIndex = 0;
        if (rule.test(text)) add("content", file, message, rule.source);
      }
      tail = text.slice(-maxPatternLen);
    });
    stream.on("error", (err) => {
      add("read", file, "could not scan file", err.message);
      resolve();
    });
    stream.on("end", resolve);
  });
}

for (const arg of args) {
  const target = path.resolve(arg);
  try {
    await walk(target);
  } catch (err) {
    add("target", target, "could not scan target", err.message);
  }
}

const unique = new Map();
for (const finding of findings) {
  unique.set(`${finding.kind}\0${finding.file}\0${finding.message}`, finding);
}

const deduped = [...unique.values()].sort((a, b) =>
  `${a.file} ${a.message}`.localeCompare(`${b.file} ${b.message}`),
);

if (deduped.length) {
  console.error(`hardeningscan: ${deduped.length} finding(s)`);
  for (const f of deduped) {
    console.error(`- [${f.kind}] ${f.file}: ${f.message}`);
  }
  process.exit(1);
}

console.log("hardeningscan: clean");
