#!/usr/bin/env node
// Claude Code PreToolUse hook for the one door (door-policy.mjs): reads the hook input on stdin and prints the decision. Registered in
// the repository's .claude/settings.json by `rightkit-gate install-hook`. Any input it cannot read allows (a hook never breaks the tool).
import fs from "node:fs";
import { claudeDoorDecision } from "../door-policy.mjs";

let input = {};
try { input = JSON.parse(fs.readFileSync(0, "utf8") || "{}"); } catch { input = {}; }
process.stdout.write(`${JSON.stringify(claudeDoorDecision(input))}\n`);
