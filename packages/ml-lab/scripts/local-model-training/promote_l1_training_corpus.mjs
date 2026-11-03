#!/usr/bin/env node
import { createHash } from "node:crypto";
import { existsSync, readFileSync, renameSync, writeFileSync } from "node:fs";
import path from "node:path";
import { pathToFileURL } from "node:url";
import { promoteAcceptedRows } from "./adjudication_pipeline.mjs";

const sha256 = (value) => `sha256:${createHash("sha256").update(value).digest("hex")}`;
const readJsonl = (file) => existsSync(file) ? readFileSync(file, "utf8").split(/\r?\n/u).filter(Boolean).map(JSON.parse) : [];
const atomic = (file, content) => { const temp = `${file}.tmp-${process.pid}`; writeFileSync(temp, content, "utf8"); renameSync(temp, file); };

export function promoteFromFiles({ corpusFiles, resolverFile, freezeFile, targetFile, judgeFile, semanticEvidenceFile, outputFile }) {
  const result = promoteAcceptedRows({ rows: corpusFiles.flatMap(readJsonl), resolvers: readJsonl(resolverFile), freezeManifest: JSON.parse(readFileSync(freezeFile, "utf8")), cloudResults: readJsonl(targetFile), judgeReceipts: readJsonl(judgeFile), semanticEvidence: semanticEvidenceFile ? readJsonl(semanticEvidenceFile) : [] });
  const content = `${result.accepted.map(JSON.stringify).join("\n")}\n`; atomic(outputFile, content);
  return { schema_version: "heardright.l1_promotion_summary.v1", status: "complete", accepted_rows: result.accepted.length, output: path.resolve(outputFile), sha256: sha256(content) };
}

function cli() {
  const args = process.argv.slice(2); const value = (flag) => { const i = args.indexOf(flag); return i >= 0 ? args[i + 1] : undefined; }; const values = (flag) => args.filter((_, i) => args[i - 1] === flag);
  const options = { corpusFiles: values("--corpus"), resolverFile: value("--resolvers"), freezeFile: value("--freeze"), targetFile: value("--targets"), judgeFile: value("--judges"), semanticEvidenceFile: value("--semantic-evidence"), outputFile: value("--out") };
  if (!options.corpusFiles.length || !options.resolverFile || !options.freezeFile || !options.targetFile || !options.judgeFile || !options.outputFile) throw new Error("usage: --corpus C [...] --resolvers R --freeze F --targets T --judges J [--semantic-evidence E] --out O");
  console.log(JSON.stringify(promoteFromFiles(options)));
}

if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) { try { cli(); } catch (error) { console.error(error.stack ?? error.message); process.exitCode = 1; } }
