// `right-git artifacts fetch`: download the artifacts of one workflow run through `gh`, merge them into one
// output tree, verify a sha256 pin, and write a JSON receipt (Orthic-Labs/rightkit#439).
//
//   right-git artifacts fetch --run <id> --out <dir> [--name <glob>] [--repo owner/repo]
//                             [--expect-sha256 <hex> [--file <relative path>]] [--receipt <file>]
//
// Artifacts are listed first (gh api), filtered by the name glob, then each one is downloaded by its immutable
// numeric artifact id (gh api .../artifacts/<id>/zip, never by the mutable name) into a staging directory next
// to <dir>. Nothing reaches <dir> until every artifact downloaded, no two artifacts disagree about a file, the
// pin matched, and the receipt path does not collide with a fetched file. Files are then placed through
// no-follow destination checks (a symbolic link in any destination component refuses the fetch). A mismatch
// removes the staging directory and leaves <dir> as it was.
//
// The pin: with --file it is the sha256 of that one file in the merged tree (the renderer DLL, say); without it,
// the tree digest: sha256 over the lines "<sha256>  <relative path>\n" of every file, sorted by path.

import fs from "node:fs";
import path from "node:path";
import crypto from "node:crypto";
import { execFile as execFileCb } from "node:child_process";
import { promisify } from "node:util";

const execFileAsync = promisify(execFileCb);
const RUN_ID_RE = /^[0-9]{1,20}$/;
const NAME_GLOB_RE = /^[A-Za-z0-9._*?-]{1,120}$/;
const REPO_RE = /^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})\/[A-Za-z0-9._-]{1,100}$/;
const SHA256_RE = /^[0-9a-f]{64}$/;
const ARTIFACT_NAME_RE = /^[A-Za-z0-9._-]{1,120}$/;
export const RECEIPT_SCHEMA = "right-git.artifacts-receipt/1";

function fail(code, message) {
  const error = new Error(message);
  error.code = code;
  return error;
}

/** Glob (`*`, `?`) to an anchored RegExp. */
export function globToRegExp(glob) {
  const source = glob.replace(/[.+^${}()|[\]\\]/g, "\\$&").replace(/\*/g, ".*").replace(/\?/g, ".");
  return new RegExp(`^${source}$`);
}

/** Parse and validate argv for `artifacts fetch`. Throws a coded Error on bad input. */
export function parseFetchArgs(argv) {
  const opts = {};
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const value = () => {
      if (i + 1 >= argv.length || argv[i + 1].startsWith("--")) throw fail("ARTIFACTS_USAGE", `${arg} needs a value`);
      i += 1;
      return argv[i];
    };
    if (arg === "--run") opts.run = value();
    else if (arg === "--out") opts.out = value();
    else if (arg === "--name") opts.name = value();
    else if (arg === "--repo") opts.repo = value();
    else if (arg === "--expect-sha256") opts.expectSha256 = value().toLowerCase();
    else if (arg === "--file") opts.file = value();
    else if (arg === "--receipt") opts.receipt = value();
    else if (arg === "--json") opts.json = true;
    else throw fail("ARTIFACTS_USAGE", `unknown option: ${arg}`);
  }
  if (!opts.run || !RUN_ID_RE.test(opts.run)) throw fail("ARTIFACTS_USAGE", "--run <numeric run id> is required");
  if (!opts.out) throw fail("ARTIFACTS_USAGE", "--out <dir> is required");
  if (opts.name !== undefined && !NAME_GLOB_RE.test(opts.name)) throw fail("ARTIFACTS_USAGE", "--name must be a glob of letters, digits, '.', '_', '-', '*', '?'");
  if (opts.repo !== undefined && !REPO_RE.test(opts.repo)) throw fail("ARTIFACTS_USAGE", "--repo must be owner/repo");
  if (opts.expectSha256 !== undefined && !SHA256_RE.test(opts.expectSha256)) throw fail("ARTIFACTS_USAGE", "--expect-sha256 must be 64 hex characters");
  if (opts.file !== undefined) {
    if (opts.expectSha256 === undefined) throw fail("ARTIFACTS_USAGE", "--file needs --expect-sha256");
    if (path.isAbsolute(opts.file) || opts.file.split(/[\\/]/).includes("..")) throw fail("ARTIFACTS_USAGE", "--file must be a relative path inside the output tree");
  }
  return opts;
}

/** Refuse when `target` or any existing component between `root` (exclusive) and it is a symbolic link. */
function assertNoSymlinkPath(root, target) {
  const rel = path.relative(root, target);
  let current = root;
  for (const part of rel.split(path.sep).filter(Boolean)) {
    current = path.join(current, part);
    let stat;
    try {
      stat = fs.lstatSync(current);
    } catch (error) {
      if (error.code === "ENOENT") return;
      throw error;
    }
    if (stat.isSymbolicLink()) throw fail("ARTIFACTS_DEST_SYMLINK", `refusing to write through a symbolic link: ${path.relative(root, current) || current}`);
  }
}

function sha256File(file) {
  const hash = crypto.createHash("sha256");
  hash.update(fs.readFileSync(file));
  return hash.digest("hex");
}

function listFiles(root) {
  const out = [];
  const walk = (dir) => {
    for (const entry of fs.readdirSync(dir, { withFileTypes: true })) {
      const full = path.join(dir, entry.name);
      if (entry.isSymbolicLink()) throw fail("ARTIFACTS_SYMLINK", `artifact contains a symbolic link: ${path.relative(root, full)}`);
      if (entry.isDirectory()) walk(full);
      else if (entry.isFile()) out.push(full);
    }
  };
  walk(root);
  return out;
}

/** Digest of a file list [{path, sha256}]: sha256 over sorted "<sha256>  <path>\n" lines. */
export function treeDigest(files) {
  const lines = [...files].sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0)).map((file) => `${file.sha256}  ${file.path}\n`);
  return crypto.createHash("sha256").update(lines.join("")).digest("hex");
}

/**
 * Fetch, merge, verify. `deps.execFile(cmd, args, opts)` defaults to child_process.execFile (promisified).
 * Returns the receipt; throws a coded Error on any failure (staging removed, output tree untouched).
 */
export async function fetchArtifacts(options, deps = {}) {
  const execFile = deps.execFile ?? execFileAsync;
  const out = path.resolve(options.out);
  const repoArg = options.repo ?? "{owner}/{repo}";
  const gh = (args) => execFile("gh", args, { maxBuffer: 64 * 1024 * 1024 });

  let listed;
  try {
    const { stdout } = await gh(["api", "--paginate", "--slurp", `repos/${repoArg}/actions/runs/${options.run}/artifacts`]);
    const pages = JSON.parse(stdout);
    listed = (Array.isArray(pages) ? pages : [pages]).flatMap((page) => page.artifacts ?? []);
  } catch (error) {
    throw fail("ARTIFACTS_LIST", `gh could not list the artifacts of run ${options.run}: ${String(error.message ?? error).split("\n")[0]}`);
  }

  const matcher = options.name === undefined ? null : globToRegExp(options.name);
  const matched = listed.filter((artifact) => typeof artifact.name === "string" && (matcher === null || matcher.test(artifact.name)));
  if (matched.length === 0) throw fail("ARTIFACTS_NONE", `run ${options.run} has no artifact matching ${options.name ?? "*"}`);
  for (const artifact of matched) {
    if (!ARTIFACT_NAME_RE.test(artifact.name)) throw fail("ARTIFACTS_NAME", `refusing artifact with an unsafe name: ${JSON.stringify(artifact.name)}`);
    if (artifact.expired === true) throw fail("ARTIFACTS_EXPIRED", `artifact ${artifact.name} of run ${options.run} has expired`);
  }

  fs.mkdirSync(path.dirname(out), { recursive: true });
  const staging = fs.mkdtempSync(path.join(path.dirname(out), `.${path.basename(out)}.fetch-`));
  try {
    const merged = new Map();
    const perArtifact = [];
    for (const artifact of matched.sort((a, b) => (a.name < b.name ? -1 : 1))) {
      const dir = path.join(staging, "dl", artifact.name);
      fs.mkdirSync(dir, { recursive: true });
      if (!Number.isSafeInteger(artifact.id) || artifact.id <= 0) throw fail("ARTIFACTS_ID", `artifact ${artifact.name} has no numeric id; refusing to download by name`);
      const zip = path.join(staging, `${artifact.id}.zip`);
      try {
        const { stdout } = await execFile("gh", ["api", `repos/${repoArg}/actions/artifacts/${artifact.id}/zip`], { maxBuffer: 1024 * 1024 * 1024, encoding: "buffer" });
        fs.writeFileSync(zip, stdout);
        const [cmd, args] = process.platform === "win32" ? ["tar", ["-xf", zip, "-C", dir]] : ["unzip", ["-q", "-o", zip, "-d", dir]];
        await execFile(cmd, args, { maxBuffer: 64 * 1024 * 1024 });
      } catch (error) {
        throw fail("ARTIFACTS_DOWNLOAD", `could not download artifact ${artifact.name} (id ${artifact.id}): ${String(error.message ?? error).split("\n")[0]}`);
      }
      const zipSha256 = sha256File(zip);
      const files = [];
      for (const file of listFiles(dir)) {
        const rel = path.relative(dir, file).split(path.sep).join("/");
        const digest = sha256File(file);
        const earlier = merged.get(rel);
        if (earlier && earlier.sha256 !== digest) {
          throw fail("ARTIFACTS_CONFLICT", `artifacts ${earlier.artifact} and ${artifact.name} disagree about ${rel}`);
        }
        if (!earlier) merged.set(rel, { sha256: digest, source: file, artifact: artifact.name });
        files.push({ path: rel, sha256: digest, size: fs.statSync(file).size });
      }
      perArtifact.push({ name: artifact.name, id: artifact.id, zipSha256, files });
    }

    const tree = [...merged.entries()].map(([rel, entry]) => ({ path: rel, sha256: entry.sha256 }));
    const treeSha256 = treeDigest(tree);
    let pinned = null;
    if (options.expectSha256 !== undefined) {
      const subject = options.file === undefined ? "tree" : options.file.split(path.sep).join("/");
      const actual = options.file === undefined ? treeSha256 : merged.get(subject)?.sha256;
      if (actual === undefined) throw fail("ARTIFACTS_PIN_FILE", `--file ${subject} is not in the fetched artifacts`);
      if (actual !== options.expectSha256) {
        throw fail("ARTIFACTS_PIN_MISMATCH", `sha256 pin mismatch for ${options.file ?? "the artifact tree"}: expected ${options.expectSha256}, got ${actual}`);
      }
      pinned = { subject: options.file ?? "tree", expected: options.expectSha256, actual };
    }

    const receiptPath = options.receipt ? path.resolve(options.receipt) : null;
    if (receiptPath) {
      const key = (value) => path.resolve(value).toLowerCase();
      if (key(receiptPath) === key(out)) throw fail("ARTIFACTS_RECEIPT_COLLISION", "--receipt must not be the output directory");
      if (key(receiptPath).startsWith(`${key(out)}${path.sep}`)) {
        for (const rel of merged.keys()) {
          const target = key(path.join(out, ...rel.split("/")));
          if (target === key(receiptPath) || target.startsWith(`${key(receiptPath)}${path.sep}`)) {
            throw fail("ARTIFACTS_RECEIPT_COLLISION", `--receipt ${options.receipt} collides with fetched file ${rel}; choose a path outside the artifacts`);
          }
        }
      }
      try {
        if (fs.lstatSync(receiptPath).isSymbolicLink()) throw fail("ARTIFACTS_DEST_SYMLINK", `refusing to write the receipt through a symbolic link: ${options.receipt}`);
      } catch (error) {
        if (error.code !== "ENOENT") throw error;
      }
    }

    let outStat = null;
    try {
      outStat = fs.lstatSync(out);
    } catch (error) {
      if (error.code !== "ENOENT") throw error;
    }
    if (outStat?.isSymbolicLink()) throw fail("ARTIFACTS_DEST_SYMLINK", `refusing to write through a symbolic link: ${out}`);
    // Stage the verified tree fresh, re-verify it, refuse symlinked destinations, then rename each file into place.
    const verifiedTree = path.join(staging, "tree");
    for (const [rel, entry] of merged) {
      const staged = path.join(verifiedTree, ...rel.split("/"));
      fs.mkdirSync(path.dirname(staged), { recursive: true });
      fs.copyFileSync(entry.source, staged, fs.constants.COPYFILE_EXCL);
      if (sha256File(staged) !== entry.sha256) throw fail("ARTIFACTS_CHANGED", `${rel} changed while staging`);
    }
    for (const rel of merged.keys()) assertNoSymlinkPath(out, path.join(out, ...rel.split("/")));
    fs.mkdirSync(out, { recursive: true });
    let counter = 0;
    for (const [rel, entry] of merged) {
      const target = path.join(out, ...rel.split("/"));
      assertNoSymlinkPath(out, path.dirname(target));
      fs.mkdirSync(path.dirname(target), { recursive: true });
      assertNoSymlinkPath(out, path.dirname(target));
      counter += 1;
      const temp = path.join(path.dirname(target), `.${path.basename(target)}.${process.pid}.${counter}.tmp`);
      fs.copyFileSync(path.join(verifiedTree, ...rel.split("/")), temp, fs.constants.COPYFILE_EXCL);
      try {
        if (sha256File(temp) !== entry.sha256) throw fail("ARTIFACTS_CHANGED", `${rel} changed while publishing`);
        assertNoSymlinkPath(out, target);
        fs.renameSync(temp, target);
      } catch (error) {
        fs.rmSync(temp, { force: true });
        throw error;
      }
    }

    const receipt = {
      schema: RECEIPT_SCHEMA,
      run: options.run,
      repo: options.repo ?? null,
      name: options.name ?? null,
      out,
      artifacts: perArtifact,
      fileCount: merged.size,
      treeSha256,
      pin: pinned,
      verified: pinned !== null,
    };
    if (receiptPath) {
      fs.mkdirSync(path.dirname(receiptPath), { recursive: true });
      fs.writeFileSync(receiptPath, `${JSON.stringify(receipt, null, 2)}\n`, "utf8");
    }
    return receipt;
  } finally {
    fs.rmSync(staging, { recursive: true, force: true });
  }
}

/** CLI entry: argv after `artifacts`. Returns the process exit code. */
export async function artifactsCommand(argv, deps = {}, io = { log: console.log, error: console.error }) {
  const [sub, ...rest] = argv;
  if (sub !== "fetch") {
    io.error("right-git artifacts fetch --run <id> --out <dir> [--name <glob>] [--repo owner/repo] [--expect-sha256 <hex> [--file <path>]] [--receipt <file>] [--json]");
    return 2;
  }
  let options;
  try {
    options = parseFetchArgs(rest);
  } catch (error) {
    io.error(`right-git artifacts: ${error.message}`);
    return 2;
  }
  try {
    const receipt = await fetchArtifacts(options, deps);
    io.log(options.json ? JSON.stringify(receipt, null, 2) : `right-git artifacts: fetched ${receipt.artifacts.length} artifact(s), ${receipt.fileCount} file(s) into ${receipt.out} (tree sha256 ${receipt.treeSha256}${receipt.verified ? ", pin verified" : ""})`);
    return 0;
  } catch (error) {
    io.error(`right-git artifacts: ${error.code ?? "ERROR"}: ${error.message}`);
    return 1;
  }
}
