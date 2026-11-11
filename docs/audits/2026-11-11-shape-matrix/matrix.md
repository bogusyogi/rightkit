# RightKit shape matrix

Generated 2026-11-11T17:49Z from RightKit 55b330d.

Packages: 305 (222 crates, 53 npm, 30 Swift). Reviewed for shape: 305 (best 67, gaps 178, wrong 53, missing 7). Not yet reviewed: 0.

Cell: review fit · pin state on the app's main · planned lines and the share found in the package. Empty means the app has nothing for the package.

## Agent platform (60)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/agent-ui` | npm | n/a | gaps | CodeRight's desktop UI split into six packages; agent-ui-approvals is MailRight's approval card/queue and Pulse's hub remote views placed side by side, not merged |  |  | gaps · 1308L 98% | fits · 118530L 95% |  | gaps · 1395L 73% |  |  |  |  |
| `@rightkit/agent-ui-approvals` | npm | yes | gaps (family) | family verdict of @rightkit/agent-ui |  |  | gaps | fits |  | gaps · ahead |  |  |  |  |
| `@rightkit/agent-ui-components` | npm | yes | gaps (family) | family verdict of @rightkit/agent-ui |  |  | gaps | fits |  | gaps |  |  |  |  |
| `@rightkit/agent-ui-core` | npm | yes | gaps (family) | family verdict of @rightkit/agent-ui |  |  | gaps | fits |  | gaps |  |  |  |  |
| `@rightkit/agent-ui-hooks` | npm | yes | gaps (family) | family verdict of @rightkit/agent-ui |  |  | gaps | fits |  | gaps |  |  |  |  |
| `@rightkit/agent-ui-settings` | npm | yes | gaps (family) | family verdict of @rightkit/agent-ui |  |  | gaps | fits |  | gaps |  |  |  |  |
| `@rightkit/agent-ui-web` | npm | yes | gaps (family) | family verdict of @rightkit/agent-ui |  |  | gaps | fits |  | gaps |  |  |  |  |
| `@rightkit/agent-ui-workbench` | npm | yes | gaps (family) | family verdict of @rightkit/agent-ui |  |  | gaps | fits |  | gaps |  |  |  |  |
| `@rightkit/terminal-ui` | npm | yes | best | CodeRight's web terminal component |  |  |  | fits · 1370L 98% |  |  |  |  |  |  |
| `rightkit-agent-demo` | cargo | yes | gaps | CutRight's agent demo mode |  |  |  |  |  |  |  | fits · 4434L 79% |  |  |
| `rightkit-agent-engine` | cargo | yes | wrong | Two unrelated things under one name: the base crate is Pulse's always-on task contract (core/src/agent); the 14 -* crates are CodeRight's turn-loop engine |  |  | gaps · 5097L 13% | gaps · 124089L 82% |  |  |  |  |  |  |
| `rightkit-agent-engine-api-integrations` | cargo | no (0.1.2, registry 0.1.1) | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-api-platform` | cargo | no (0.1.2, registry 0.1.1) | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-api-sessions` | cargo | yes | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-api-workflows` | cargo | yes | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-attention` | cargo | no (0.1.2, registry 0.1.1) | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-cli` | cargo | yes | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps · ahead |  |  |  |  |  |  |
| `rightkit-agent-engine-core` | cargo | yes | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-history` | cargo | no (0.1.2, registry 0.1.1) | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-mobile-runtime` | cargo | yes | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-protocol` | cargo | no (0.1.2, registry 0.1.1) | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-runtime-middleware` | cargo | no (0.1.2, registry 0.1.1) | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-runtime-resources` | cargo | yes | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps · ahead |  |  |  |  |  |  |
| `rightkit-agent-engine-storage` | cargo | yes | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-engine-tasks` | cargo | yes | wrong (family) | family verdict of rightkit-agent-engine |  |  | gaps | gaps |  |  |  |  |  |  |
| `rightkit-agent-gateway` | cargo | yes | wrong | rightkit-agent-gateway is MailRight's mail gateway (drafts, forward, send-with-approval, Gmail seams); Pulse's gateway is a separate crate, rightkit-remote-gateway |  |  | wrong · 20758L 1% |  |  | fits · ahead · 14620L 34% |  |  |  |  |
| `rightkit-agent-host-traits` | cargo | no (0.1.3, registry 0.1.2) | best | CodeRight host seam traits (not fed by a plan target) |  |  |  |  |  |  |  |  |  |  |
| `rightkit-agent-orchestration` | cargo | yes | gaps | CodeRight's orchestration crate; -handoff also matches some Pulse transcript lines |  |  |  | fits · 37870L 80% |  |  |  |  |  |  |
| `rightkit-agent-orchestration-handoff` | cargo | yes | gaps (family) | family verdict of rightkit-agent-orchestration |  |  |  | fits |  |  |  |  |  | ahead |
| `rightkit-agent-plugins` | cargo | no (0.1.2, registry 0.1.1) | best | CodeRight plugin discovery |  |  |  |  |  |  |  |  |  |  |
| `rightkit-agent-policy` | cargo | no (0.1.2, registry 0.1.1) | gaps | CodeRight's policy crate |  |  |  | fits · 16018L 85% |  |  |  |  |  |  |
| `rightkit-agent-recipes` | cargo | yes | best | CodeRight recipes |  |  |  |  |  |  |  |  |  |  |
| `rightkit-agent-server` | cargo | yes | wrong | CodeRight's coderight binary and HTTP API, 164k lines, plus code planned for nine other packages |  |  |  | gaps · 91539L 88% |  |  |  |  |  |  |
| `rightkit-agent-skills` | cargo | yes | best | CodeRight skills |  |  |  |  |  |  |  |  |  |  |
| `rightkit-agent-tools` | cargo | no (0.3.4, registry 0.3.3) | gaps | RightKit's right-ctl tool interface extended with CodeRight's coding tools (94% of its lines) | n/a | n/a | adopted | fits · n/a · 86283L 94% | gaps · adopted · 302L 8% | n/a | n/a | adopted | n/a | adopted |
| `rightkit-agent-transcript` | cargo | no (0.1.2, registry 0.1.1) | gaps | Pulse's membrane-transcript crate (planned under rightkit-agents) |  |  | fits · ahead | gaps |  |  |  |  |  |  |
| `rightkit-agents` | cargo | yes | gaps | Two layers: RightKit's original sans-IO Claude Code and Codex clients with a blocking Session, plus CodeRight's driver_host (async executor, six provider profiles, ACP) behind features. Pulse's planned code is in rightkit-agent-transcript |  |  | fits · 2835L 6% | gaps · adopted · 44458L 66% |  |  |  |  | 339L 10% | adopted |
| `rightkit-ai-accounts` | cargo | yes | gaps | Part of Pulse's Claude account switching (29% of planned lines) |  |  | gaps · ahead · 17592L 29% |  |  |  |  |  |  |  |
| `rightkit-approvals` | cargo | yes | wrong | rightkit-approvals is GenRight's paid-draft approval (launch-secret HMAC token, 'eyes' release, blender_finish). Pulse's signed owner approvals are in rightkit-owner-approvals and its pending queue in rightkit-remote-gateway/approvals.rs. MailRight's are in rightkit-agent-gateway/approval*.rs |  |  | wrong · 13104L 1% |  | fits · 1485L 48% | wrong · 2083L 0% |  |  |  |  |
| `rightkit-bot-messaging` | cargo | yes | gaps | Under half of CodeRight's bot messaging; the rest and the Telegram client are in agent-server |  |  |  | gaps · 6489L 47% |  |  |  |  |  |  |
| `rightkit-chat-bridge` | cargo | yes | gaps | Pulse's chat-between-chats code |  |  | fits · 26856L 75% |  |  |  |  |  |  |  |
| `rightkit-evidence` | cargo | yes | gaps | rightkit-evidence holds a quarter of CodeRight's planned evidence code; most (1.8k of 2.2k lines) is in agent-server. The seven -* crates (27k lines) are not fed by any planned app file in this family |  |  |  | gaps · ahead · 5738L 26% |  |  |  |  |  |  |
| `rightkit-evidence-application` | cargo | yes | gaps (family) | family verdict of rightkit-evidence |  |  |  | gaps |  |  |  |  |  |  |
| `rightkit-evidence-commands` | cargo | yes | gaps (family) | family verdict of rightkit-evidence |  |  |  | gaps |  |  |  |  |  |  |
| `rightkit-evidence-contracts` | cargo | yes | gaps (family) | family verdict of rightkit-evidence |  |  |  | gaps |  |  |  |  |  | ahead |
| `rightkit-evidence-decisions` | cargo | yes | gaps (family) | family verdict of rightkit-evidence |  |  |  | gaps |  |  |  |  |  | ahead |
| `rightkit-evidence-effects` | cargo | yes | gaps (family) | family verdict of rightkit-evidence |  |  |  | gaps |  |  |  |  |  | ahead |
| `rightkit-evidence-governance` | cargo | yes | gaps (family) | family verdict of rightkit-evidence |  |  |  | gaps |  |  |  |  |  | ahead |
| `rightkit-evidence-report` | cargo | yes | gaps (family) | family verdict of rightkit-evidence |  |  |  | gaps |  |  |  |  |  | ahead |
| `rightkit-github` | cargo | yes | gaps | CodeRight's GitHub App module |  |  |  | gaps · 8586L 50% |  |  |  |  |  |  |
| `rightkit-hooks` | cargo | no (0.2.4, registry 0.2.3) | gaps | Claude Code/Codex hook payloads; CodeRight 66% in; also used by Pulse's chat-bridge |  |  | ahead | fits · exception · 5423L 66% |  |  |  |  |  |  |
| `rightkit-llm` | cargo | no (0.3.5, registry 0.3.4) | gaps | RightKit's original BYOK clients (anthropic.rs, openai.rs, failover) plus CodeRight's agent_providers and agent_router (15.7k lines) behind features | ahead |  | ahead | gaps · ahead · 33990L 67% |  | gaps · ahead · 941L 5% |  |  |  |  |
| `rightkit-mcp` | cargo | no (0.7.5, registry 0.7.4) | gaps | RightKit MCP client/server with CodeRight's (74%) | local |  | ahead | fits · ahead · 10970L 74% |  |  |  |  |  |  |
| `rightkit-pty` | cargo | no (0.1.1, registry 0.1.0) | gaps | RightKit's own |  |  |  |  |  |  |  |  |  |  |
| `rightkit-remote-gateway` | cargo | yes | gaps | Pulse core/src/remote, 75% of its planned gateway lines |  |  | fits |  |  |  |  |  |  |  |
| `rightkit-sandbox` | cargo | no (0.6.5, registry 0.6.4) | gaps | RightKit sandbox; a quarter of CodeRight's sandbox code |  |  |  | gaps · ahead · 7177L 26% |  |  |  |  |  |  |
| `rightkit-scheduler` | cargo | yes | gaps | A model-only subset (816 lines) of CodeRight's scheduler |  |  |  | gaps · 9315L 11% |  |  |  |  |  |  |
| `rightkit-terminal-ui` | cargo | yes | gaps | CodeRight's ratatui TUI |  |  |  | gaps · 94376L 73% |  |  |  |  |  |  |
| `rightkit-workflow` | cargo | yes | gaps | RightKit scenario runtime; under half of CodeRight's workflow code |  |  |  | gaps · ahead · 26076L 46% |  |  |  |  |  |  |
| `RightKitAgent` | swift | n/a | missing | none |  |  | 4258L 0% | 30723L 0% |  |  |  |  |  |  |

## Content pipeline & social (6)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/social-ui` | npm | yes | gaps | PostRight front end, near-complete copy |  |  |  |  |  |  |  |  | fits · 17034L 98% |  |
| `rightkit-content-plan` | cargo | yes | gaps | PostRight calendar, variants, hook tests, drafts over store traits |  |  |  |  |  |  |  |  | gaps · 4599L 57% |  |
| `rightkit-content-proof` | cargo | yes | gaps | only PostRight's claim-number canonicaliser (172 lines) |  |  |  |  |  |  |  |  | gaps · 3820L 3% |  |
| `rightkit-social-analytics` | cargo | yes | wrong | PostRight links/analytics/metric_import/optimise plus CutRight retention command under a feature |  |  |  |  |  |  |  | wrong · 1300L 89% | gaps · 5186L 58% |  |
| `rightkit-social-inbox` | cargo | yes | gaps | PostRight webhook inbox and keyword automation over store traits |  |  |  |  |  |  |  |  | gaps · 3070L 48% |  |
| `rightkit-social-publish` | cargo | yes | gaps | PostRight publish adapters, phase machine, quota, accounts over store traits |  |  |  |  |  |  |  |  | gaps · ahead · 16930L 62% |  |

## Context, memory & code intelligence (24)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `rightkit-adapt` | cargo | no (0.1.2, registry 0.1.1) | wrong | rightkit-adapt is CodeRight's coderight-learning crate; rightkit-adapt-taste is Pulse's membrane-adapt |  |  | fits · 15823L 96% | gaps · 8073L 76% |  |  |  |  |  |  |
| `rightkit-adapt-taste` | cargo | no (0.1.2, registry 0.1.1) | wrong (family) | family verdict of rightkit-adapt |  |  | fits · ahead | gaps |  |  |  |  |  |  |
| `rightkit-channel-memory` | cargo | yes | best | pre-existing RightKit contract crate (0.2.3) |  |  |  |  | fits |  |  |  | fits · ahead · 1618L 2% |  |
| `rightkit-code-audit` | cargo | n/a | gaps | Legion legion-audit, CLI commands and legion-dev modules, moved 1 Nov |  |  |  |  |  |  |  |  |  | gaps |
| `rightkit-code-audit-commands` | cargo | yes | gaps (family) | family verdict of rightkit-code-audit |  |  |  |  |  |  |  |  |  | gaps |
| `rightkit-code-audit-engine` | cargo | yes | gaps (family) | family verdict of rightkit-code-audit |  |  |  |  |  |  |  |  |  | gaps |
| `rightkit-code-audit-provider-sdk` | cargo | yes | gaps (family) | family verdict of rightkit-code-audit |  |  |  |  |  |  |  |  |  | gaps · ahead |
| `rightkit-code-audit-qualification` | cargo | yes | gaps (family) | family verdict of rightkit-code-audit |  |  |  |  |  |  |  |  |  | gaps |
| `rightkit-context` | cargo | yes | gaps | Pulse membrane engine crates, moved crate for crate |  |  | fits · 98511L 89% | gaps |  |  |  |  |  |  |
| `rightkit-context-client` | cargo | yes | gaps (family) | family verdict of rightkit-context |  |  | fits · ahead | gaps |  |  |  |  |  |  |
| `rightkit-context-core` | cargo | yes | gaps (family) | family verdict of rightkit-context |  |  | fits · ahead | gaps |  |  |  |  |  |  |
| `rightkit-context-federation` | cargo | yes | gaps (family) | family verdict of rightkit-context |  |  | fits | gaps |  |  |  |  |  |  |
| `rightkit-context-protocol` | cargo | yes | gaps (family) | family verdict of rightkit-context |  |  | fits · ahead | gaps |  |  |  |  |  |  |
| `rightkit-context-provider-sdk` | cargo | yes | gaps (family) | family verdict of rightkit-context |  |  | fits · ahead | gaps |  |  |  |  |  |  |
| `rightkit-context-runtime` | cargo | yes | gaps (family) | family verdict of rightkit-context |  |  | fits | gaps |  |  |  |  |  |  |
| `rightkit-embed` | cargo | no (0.6.5, registry 0.6.4) | best | pre-existing RightKit crate (0.6.5) |  |  | fits · ahead |  |  | fits · ahead · 398L 0% |  |  |  |  |
| `rightkit-memory` | cargo | yes | wrong | memory-core and memory-store are Pulse cortex-core and cortex-store; rightkit-memory is CodeRight's client of Pulse's membrane CLI plus its mobile in-memory store |  |  | fits · 14843L 99% | gaps · ahead · 9759L 62% |  |  |  |  |  |  |
| `rightkit-memory-core` | cargo | yes | wrong (family) | family verdict of rightkit-memory |  |  | fits | gaps |  |  |  |  |  |  |
| `rightkit-memory-store` | cargo | yes | wrong (family) | family verdict of rightkit-memory |  |  | fits | gaps |  |  |  |  |  |  |
| `rightkit-repo-graph` | cargo | yes | gaps | Pulse membrane-blueprint plus CodeRight skeletonization as one added module |  |  | fits · 37279L 96% | fits · 600L 95% |  |  |  |  |  |  |
| `rightkit-repo-graph-grammars` | cargo | yes | gaps (family) | family verdict of rightkit-repo-graph |  |  | fits | fits |  |  |  |  |  |  |
| `rightkit-research` | cargo | yes | gaps | Legion legion-research; no PostRight code |  |  |  |  |  |  |  |  | wrong · 1572L 2% | gaps · ahead |
| `rightkit-search` | cargo | yes | best | pre-existing RightKit crate (0.4.5); holds MailRight's html-to-text |  |  | exception | fits · ahead | ahead | fits · ahead · 944L 13% | ahead | ahead |  |  |
| `rightkit-vault` | cargo | yes | gaps | fresh implementation of split-key secret sync; README says written without a build or test run |  |  | 1984L 0% |  |  |  |  |  |  |  |

## Documents (12)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/doc-render` | npm | no (0.2.2, registry 0.2.1) | best | ViewRight's Markdown reader pieces behind a written API |  | fits · adopted |  | fits · adopted |  |  | fits · adopted |  |  |  |
| `@rightkit/document-core` | npm | no (0.1.2, registry 0.1.1) | wrong | Surveyed ViewRight (0.98 similar): bridge, settings, themes, chrome, printing and the whole PDF editor; not in the master plan |  | gaps |  |  |  |  |  |  |  |  |
| `@rightkit/document-workspace` | npm | no (0.1.2, registry 0.1.1) | wrong | Surveyed ViewRight 0.1.165 src/app, settings, state copied at the same paths (0.97 similar; 0.75 to live 0.1.123) |  | gaps · 31171L 73% |  |  |  |  |  |  |  |  |
| `@rightkit/docx-editor` | npm | yes | gaps | Surveyed ViewRight src/documents/docx, re-pointed from the vendored eigenpal source to published @eigenpal 1.9.0 |  | gaps · 1834L 96% |  |  |  |  |  |  |  |  |
| `@rightkit/html-editor` | npm | yes | gaps | Surveyed ViewRight src/documents/html (0.99 similar; 0.95 to live) |  | gaps · 15898L 91% |  |  |  |  |  |  |  |  |
| `@rightkit/markdown-editor` | npm | yes | gaps | Surveyed ViewRight src/documents/markdown and mdx (0.98 similar; 0.90 to live); reader pieces now imported from doc-render |  | gaps · 27905L 94% |  |  |  |  |  |  |  |  |
| `@rightkit/pdf-ui` | npm | no (0.3.0, registry 0.2.0) | gaps | Written read-only pdf.js viewer; 0.3.0 adds props so ViewRight's editor can sit on it |  | gaps · adopted · 10411L 3% |  |  |  | fits · 144L 5% |  |  |  |  |
| `rightkit-docs` | cargo | yes | gaps | Written DOCX/XLSX/PDF read and write library; feature 'files' is ViewRight's documents.rs |  | gaps · ahead · 695L 88% | fits · ahead |  |  |  |  |  |  |  |
| `rightkit-document-workspace` | cargo | yes | gaps | ViewRight src-tauri persistence.rs and command bodies, reshaped behind PathGrants, DocumentBackend and WorkspaceWatcher traits |  | gaps · ahead · 3147L 51% |  |  |  |  |  |  |  |  |
| `rightkit-ocr` | cargo | yes | best | Written crate: native Vision and Windows OCR, isolated worker, verified ONNX fallback |  | fits · adopted · 401L 4% |  | fits · adopted |  |  | fits · adopted · 586L 4% | fits · adopted |  |  |
| `rightkit-pdf` | cargo | yes | gaps | ViewRight's PDF editor core moved earlier, plus Legion merge and ScrapeRight text-extraction donors |  | fits · ahead · 503L 5% |  |  |  |  | gaps · exception · 129L 0% |  |  |  |
| `rightkit-typeset` | cargo | yes | best | Fresh small wrapper over Typst and mitex; CutRight's planned lines are the vendored citationberg fork only |  |  |  |  |  |  |  | fits |  |  |

## Foundation & tooling (8)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/config` | npm | yes | best | fresh presets (tsconfig, eslint, vite, Rust toml files) | fits · 159L 29% | gaps · 304L 8% | fits · 58L 50% | fits · 4L 100% | fits · 96L 27% | fits · 114L 38% | fits · ahead · 171L 33% | fits · ahead · 57L 57% | fits · 101L 30% |  |
| `@rightkit/waitlist-worker` | npm | yes | best | config-driven factory generalising the ViewRight and MailRight workers |  | fits · ahead · 218L 35% |  |  |  | fits · 311L 20% |  |  |  |  |
| `rightkit-apple-ops` | cargo | yes | gaps | Legion legion-apple, moved 1 Nov |  |  |  |  |  |  |  |  |  | gaps |
| `rightkit-bench` | cargo | no (0.2.1, registry 0.2.0) | gaps | MailRight timing helpers and ScrapeRight article scorer, plus fresh load driver and real-database probes |  | gaps · 2725L 0% |  | 5329L 1% |  | wrong · 4191L 4% | gaps · ahead · 15276L 1% |  |  |  |
| `rightkit-connector-gen` | cargo | yes | best | fresh, std-only generator from an engine contract |  |  |  |  |  |  |  |  |  |  |
| `rightkit-doctor` | cargo | yes | gaps | fresh framework shaped on the engine doctor rows the apps share |  |  |  | gaps · ahead · 1652L 10% |  | fits · ahead · 95L 8% |  | fits · ahead · 177L 10% | gaps · 338L 6% |  |
| `rightkit-ml-lab` | cargo | n/a | wrong | HeardRight training and evaluation scripts copied with their directory layout | wrong · 53827L 0% |  |  |  |  |  |  |  |  |  |
| `rightkit-ownership` | cargo | yes | best | RightKit's own scanner, Rust port of the Node scanner in packages/release |  |  |  |  |  |  |  |  |  | ahead |

## Foundation: runtime (29)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/ax` | npm | yes | best | RightKit implementation, 4.4k lines |  |  | fits · 243L 0% |  | fits · 246L 3% |  |  |  |  | adopted |
| `@rightkit/ci` | npm | yes | best | RightKit implementation, 4.9k lines |  |  |  |  |  |  |  |  |  | ahead |
| `@rightkit/logs` | npm | yes | best | RightKit implementation, 808 lines | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted |  |
| `@rightkit/qa` | npm | no (0.7.0, registry 0.6.2) | best | RightKit implementation, 7.2k lines (CDP page, control client, a11y, visual, evidence, journeys) | gaps · adopted · 14575L 1% | fits · adopted · 678L 0% | fits · adopted · 3170L 1% | fits · adopted · 2987L 1% | fits · adopted · 4923L 0% | fits · adopted · 1769L 0% | fits · missing · 2361L 1% | fits · adopted | fits · adopted |  |
| `@rightkit/qa-chrome` | npm | yes | best | RightKit implementation, 1.9k lines | adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · missing | fits · adopted | fits · adopted | adopted |
| `@rightkit/release` | npm | no (0.6.53, registry 0.6.52) | gaps | RightKit implementation grown from HeardRight's release scripts; 104k lines in about 380 flat modules | gaps · ahead · 20331L 4% | fits · ahead | fits · ahead · 609L 3% | fits · ahead · 1497L 1% | fits · ahead | fits · ahead | fits · ahead · 676L 10% | fits · ahead · 320L 6% | fits · ahead · 594L 2% | ahead |
| `rightkit-cache` | cargo | yes | gaps | RightKit implementation |  |  | 89L 0% | fits · adopted |  | fits · adopted |  | fits · adopted | fits | adopted |
| `rightkit-child-process` | cargo | no (0.5.10, registry 0.5.9) | best | RightKit implementation; HeardRight main fully on it | fits · adopted | fits · adopted | fits · adopted · 124L 15% | fits · adopted | fits · adopted | fits · adopted | fits · adopted · 278L 3% | fits · adopted | fits · adopted | adopted |
| `rightkit-contracts` | cargo | no (0.1.11, registry 0.1.10) | gaps | RightKit implementation | local |  |  |  | ahead |  |  | fits · ahead | fits · ahead · 1218L 4% |  |
| `rightkit-crash` | cargo | yes | gaps | built from HeardRight's crash capture | gaps · adopted · 3616L 5% | fits · adopted | fits · adopted · 227L 6% | fits · adopted | fits · adopted | fits · adopted · 68L 3% | fits · adopted · 21L 0% | fits · adopted · 35L 14% | fits · adopted | missing |
| `rightkit-credentials` | cargo | no (0.5.1, registry 0.5.0) | best | RightKit implementation | fits · ahead · 752L 2% | fits · ahead | fits · ahead · 114L 76% | fits · ahead | fits · ahead · 379L 4% | fits · ahead · 274L 4% | fits · ahead | fits · ahead | fits · ahead | missing |
| `rightkit-crypto` | cargo | no (0.1.3, registry 0.1.2) | gaps | RightKit implementation | fits · adopted | fits | fits · adopted | fits · adopted | fits · adopted | fits · adopted |  |  | fits · adopted |  |
| `rightkit-db` | cargo | no (0.5.6, registry 0.5.5) | gaps | RightKit implementation; encrypted lifecycle built for HeardRight | gaps · local | fits · missing | fits · ahead | fits · ahead | fits · ahead | gaps · ahead · 2552L 2% | fits · ahead | n/a | fits · ahead · 1211L 2% | missing |
| `rightkit-decimal` | cargo | yes | gaps | RightKit implementation, 242 lines |  |  |  |  |  |  |  |  |  |  |
| `rightkit-device` | cargo | yes | best | RightKit implementation, split out of rightkit-tauri | fits · ahead | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted |  |
| `rightkit-file-watch` | cargo | yes | best | RightKit implementation with three front ends |  | fits · ahead · 205L 11% | fits · ahead · 328L 8% | fits · ahead · 460L 8% |  |  |  |  |  |  |
| `rightkit-fs` | cargo | no (0.4.13, registry 0.4.12) | gaps | RightKit implementation | gaps · local | fits · ahead | fits · ahead · 1653L 11% | fits · ahead | fits · ahead · 128L 8% | fits · ahead · 94L 11% | fits · ahead · 1015L 4% | fits · ahead · 137L 1% | fits · ahead · 436L 2% | ahead |
| `rightkit-http` | cargo | no (0.3.12, registry 0.3.11) | gaps | RightKit implementation | gaps · local | fits · n/a | fits · ahead | fits · ahead · 186L 86% | fits · ahead · 176L 8% | fits · ahead | fits · ahead · 409L 63% | n/a | fits · ahead · 597L 4% | ahead |
| `rightkit-identity` | cargo | no (0.1.2, registry 0.1.1) | gaps | RightKit implementation; no surveyed app imports it |  |  | gaps · 1360L 1% |  |  |  |  |  |  |  |
| `rightkit-job-queue` | cargo | no (0.5.4, registry 0.5.3) | gaps | fresh RightKit implementation; three layers behind features (store, contract, supervisor) | gaps · adopted | n/a | fits · adopted · 63L 37% | fits · adopted | fits · adopted · 501L 4% | fits · adopted | gaps · adopted · 5177L 3% | fits · adopted | fits · adopted · 141L 3% |  |
| `rightkit-oauth` | cargo | no (0.10.5, registry 0.10.4) | best | RightKit implementation | local |  |  | fits · adopted · 809L 4% |  | fits · adopted |  |  |  |  |
| `rightkit-onnx` | cargo | yes | gaps | RightKit implementation | gaps · ahead |  | fits · ahead | fits · adopted · 205L  | fits · adopted | fits · ahead | fits · adopted · 304L 12% | fits · adopted |  |  |
| `rightkit-passkeys` | cargo | yes | gaps | RightKit adapter over passkey-rs, 289 lines |  |  |  |  |  |  |  |  |  |  |
| `rightkit-qa` | cargo | no (0.10.6, registry 0.10.5) | gaps | RightKit implementation, 52k lines | gaps · local · 4649L 5% | fits · ahead | fits · ahead | gaps · ahead · 6854L 0% | fits · ahead | fits · ahead · 488L 7% | fits · ahead | fits · ahead | fits · ahead · 907L 6% | missing |
| `rightkit-qa-driver` | cargo | yes | wrong | deprecated shim (its own Cargo description) over rightkit-qa control and rightkit-automation |  |  |  |  |  |  |  |  |  |  |
| `rightkit-sidecar` | cargo | no (0.3.2, registry 0.3.1) | best | RightKit implementation of bounded JSON-lines framing | fits · adopted | n/a | fits · adopted | fits · adopted · 3L  | fits · adopted | fits · adopted · 3109L 0% | fits · adopted | adopted | adopted |  |
| `rightkit-sync` | cargo | yes | gaps | RightKit engine; HeardRight main runs on it | gaps · adopted · 2081L 5% |  | fits · adopted |  |  |  | gaps · adopted · 6854L 4% |  |  |  |
| `rightkit-sync-keys` | cargo | private | gaps | RightKit implementation |  |  |  |  |  |  |  |  |  |  |
| `rightkit-time` | cargo | no (0.1.4, registry 0.1.3) | best | RightKit implementation; PostRight calendar_time.rs is 84% in it | ahead |  | fits · ahead | fits · ahead | fits · ahead | fits · ahead · 69L 49% | fits · ahead |  | fits · ahead · 233L 84% |  |

## Foundation: shell and UI (22)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/app-update` | npm | no (0.4.3, registry 0.4.2) | gaps | fresh update-policy helper and UI pieces | adopted | fits · adopted | gaps · adopted | fits · adopted | fits · adopted | adopted | adopted · 218L 0% | adopted | adopted |  |
| `@rightkit/i18n` | npm | no (0.2.1, registry 0.2.0) | gaps | fresh React i18n runtime |  |  | gaps · adopted · 7215L 0% | gaps · adopted |  |  |  |  | 150L 0% |  |
| `@rightkit/legal` | npm | yes | best | fresh manifest and receipt primitives | fits · adopted | fits · adopted · 312L 4% | fits · adopted | fits · adopted · 111L 0% | fits · adopted | fits · adopted · 564L 3% | adopted · 313L 13% | adopted · 395L 2% | adopted |  |
| `@rightkit/legal-ui` | npm | no (0.4.2, registry 0.4.1) | best | fresh LegalGate screen | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | adopted · 201L 1% | adopted · 237L 1% | adopted | adopted |  |
| `@rightkit/license` | npm | no (0.3.1, registry 0.3.0) | gaps | fresh renderer client over native commands | gaps · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | fits · adopted | adopted · 192L 0% | adopted | adopted |  |
| `@rightkit/onboarding` | npm | no (0.6.4, registry 0.6.3) | gaps | fresh frame with legal gate and built-in steps | gaps · adopted · 4328L 0% | fits · adopted | fits · adopted · 920L 1% | gaps · adopted · 789L 1% | fits · adopted · 662L 2% | fits · adopted | adopted · 384L 1% | adopted | adopted |  |
| `@rightkit/platform-ui` | npm | no (0.4.1, registry 0.4.0) | best | fresh headless helpers | adopted | fits · adopted · 160L 1% | fits · adopted | fits · adopted | fits · adopted | fits · adopted | adopted | adopted | adopted |  |
| `@rightkit/react-shell` | npm | no (0.7.3, registry 0.7.2) | gaps | fresh component set | gaps · adopted | fits · adopted | fits · adopted | gaps · adopted · 4054L 1% | fits · adopted · 788L 0% | fits · adopted · 2464L 2% | adopted · 4155L 0% | adopted | adopted · 350L 2% |  |
| `@rightkit/settings-ui` | npm | no (0.7.1, registry 0.7.0) | gaps | fresh frame, primitives, standard sections, backend port | gaps · adopted · 6175L 2% | gaps · adopted · 2684L 2% | fits · adopted · 4666L 3% | fits · adopted | fits · adopted · 3617L 1% | fits · adopted · 3465L 2% | adopted · 2072L 2% | adopted | adopted · 522L 0% |  |
| `@rightkit/tauri` | npm | no (0.3.1, registry 0.3.0) | wrong | fresh typed adapters for device, logs, diagnostics commands | adopted | adopted | fits · adopted | adopted | adopted · 192L 0% | gaps · adopted · 559L 0% | adopted · 248L 0% | adopted | adopted · 133L 0% |  |
| `@rightkit/tauri-shell` | npm | no (0.1.4, registry 0.1.3) | best | fresh typed renderer bridge for the crate | exception | fits · adopted | fits · adopted | fits · adopted | fits · adopted | exception | adopted | adopted | adopted |  |
| `@rightkit/theme` | npm | no (0.3.2, registry 0.3.1) | gaps | fresh brand-to-token generator | gaps · adopted · 271L 0% | fits · adopted · 290L 1% | fits · adopted · 445L 0% | gaps · adopted · 4133L 1% | adopted · 199L 0% | fits · adopted · 74L 2% | adopted · 166L 0% | adopted | adopted · 10L 0% |  |
| `rightkit-app-host` | cargo | no (0.1.1, registry 0.1.0) | wrong | seam for companion crates moved out of Pulse |  |  | fits · ahead |  |  |  |  |  |  |  |
| `rightkit-app-update` | cargo | yes | gaps | fresh: boot probation and fetch bridge only |  |  | gaps |  |  |  |  |  |  |  |
| `rightkit-deep-link` | cargo | no (0.1.1, registry 0.1.0) | best | split from the shell, unpublished |  |  |  | fits |  |  |  |  |  |  |
| `rightkit-engine-host` | cargo | yes | wrong | GenRight's engine framework, moved 2026-11-01 |  |  |  |  | fits | gaps | fits | gaps · ahead | gaps |  |
| `rightkit-i18n` | cargo | yes | gaps | fresh Rust runtime for the same JSON catalogs, unpublished |  |  | gaps |  |  |  |  |  |  |  |
| `rightkit-instance` | cargo | no (0.1.1, registry 0.1.0) | best | split from the shell, unpublished; carries HeardRight's legacy-plugin handoff | fits |  | fits |  |  | fits |  |  |  |  |
| `rightkit-license` | cargo | yes | gaps | built from HeardRight's licence code | gaps · local · 1840L 5% | fits · ahead · 320L 2% | fits · ahead | gaps · ahead · 3623L 3% | fits · ahead · 301L 3% | gaps · ahead · 858L 2% | ahead · 1468L 2% | ahead · 126L 0% | ahead |  |
| `rightkit-tauri-shell` | cargo | yes | gaps | fresh; absorbed each app's host needs as modules | gaps · ahead · 5658L 5% | fits · ahead · 2641L 7% | fits · ahead · 2033L 7% | gaps · ahead · 4608L 6% | fits · ahead · 280L 9% | fits · ahead · 902L 7% | ahead · 901L 5% | ahead · 276L 7% | ahead |  |
| `rightkit-theme` | cargo | yes | best | fresh Rust port of the npm resolver, unpublished |  |  |  |  |  |  |  |  |  |  |
| `RightKitUI` | swift | n/a | wrong | fresh, two files (glass surface, contrast) | 1563L 0% |  | wrong · 2745L 0% | wrong · 835L 0% |  |  | 422L 0% |  |  |  |

## Generative media (13)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/media-gen-ui` | npm | yes | wrong | GenRight src/ moved whole (generated index) |  |  |  |  | fits · 21626L 79% |  |  | wrong |  |  |
| `rightkit-blender-worker` | cargo | n/a | gaps | GenRight workers/blender moved 2026-11-01 (npm wrapper around Python) |  |  |  |  | fits · 4386L 0% |  |  |  |  |  |
| `rightkit-local-gen` | cargo | yes | gaps | GenRight genright-local-image moved (profiles, runtime, store, machine fit, memory, artifacts) + service_host/http |  |  |  |  | fits · ahead · 12312L 68% |  |  |  |  |  |
| `rightkit-media-gen` | cargo | yes | wrong | GenRight genright-gen moved whole with a `host` seam, plus studio voices, avatars, balances, stock, search |  |  |  |  | fits · 34258L 62% |  |  | wrong | wrong |  |
| `rightkit-media-pipeline` | cargo | yes | gaps | GenRight pipeline_workflows (+compose, translate, dubbing, fanout, media, finish, asr), production stack, brief run, motion service |  |  |  |  | fits · 9149L 92% |  |  |  |  |  |
| `rightkit-motion` | cargo | yes | gaps | Fresh neutral design (beats, sheet, FrameSource render driver, encoder) that GenRight already builds on; GenRight project_server/composition doc donated; CutRight contactsheet and thumbnail engine commands added under feature engine-commands |  |  |  |  | fits · adopted · 2619L 28% |  |  | wrong · ahead · 626L 95% |  |  |
| `rightkit-pipeline` | cargo | yes | wrong | CutRight cutright-genright moved whole (GenRightClient, Orchestrator, Journal, ModelCatalog, generated_source) + channel_recipe |  |  |  |  | wrong · 1701L 1% |  |  | fits · ahead · 13753L 28% | wrong · 2521L 3% |  |
| `rightkit-prompt-recipes` | cargo | yes | wrong | GenRight genright-studio recipes, video_templates, contracts, characters, swap_templates, kdp, brands, catalog moved with their JSON data |  |  |  |  | fits · 12135L 95% |  |  | wrong |  |  |
| `rightkit-remote-compute` | cargo | yes | wrong | GenRight model_management/remote.rs + rental.rs, plus models/ (local model catalogue, installs, governor, hardware, blender/sdcpp/wan2gp) |  |  |  |  | fits · 2681L 82% |  |  |  |  |  |
| `rightkit-spend` | cargo | yes | gaps | Fresh generic ledger (0.2.3); PostRight shims absorbed in 0.1.1, GenRight ledger semantics in 0.2.0, Pulse approved budgets in 0.2.2 |  |  | gaps · adopted · 1655L 4% |  | fits · adopted · 2755L 2% |  |  | 4345L 0% | fits · adopted · 2272L 6% |  |
| `rightkit-tts` | cargo | yes | best | RightKit runtime (system voices, QTS worker client, model manifest, chunking) that GenRight's genright-qts already wraps (=0.2.3) |  |  |  |  | fits · adopted · 4338L 4% |  |  |  |  |  |
| `rightkit-tts-qwen` | cargo | yes | best | Vendored Qwen3-TTS native inference (GGUF/GGML); no app feeds it directly |  |  |  |  | fits |  |  |  |  |  |
| `rightkit-tts-qwen-sys` | cargo | no (0.1.5, registry 0.1.4) | best | Generated bindings (macOS arm64, Windows x64) + build script |  |  |  |  | fits |  |  |  |  |  |

## HeardRight review (40)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `rightkit-asr` | cargo | yes | gaps | Rust port of the Sherpa-ONNX keyword spotter (fresh) plus decode core and ONNX execution moved from HeardRight crates | gaps · ahead · 33877L 14% |  |  |  | 314L 3% |  | 2428L 4% |  |  |  |
| `rightkit-audio` | cargo | yes | best | faithful port of live HeardRight code (heardright_capture) | fits · adopted |  |  | adopted | adopted |  |  | adopted |  |  |
| `rightkit-automation` | cargo | no (0.6.8, registry 0.6.7) | gaps | desktop module is a near line-for-line copy of HeardRight main's heardright_platform (moved in 0.6.4); the Automation engine is a fresh implementation | gaps · local · 21307L 39% |  | ahead · 1069L 2% |  |  |  |  | ahead |  |  |
| `rightkit-dictation-app-runtime` | cargo | yes | best | fork of HeardRight main (heardright-app-runtime, renamed plus host configuration) | fits |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-command-host` | cargo | yes | gaps | fork of HeardRight main (heardright-command-host, renamed, test_mode removed, platform calls now rightkit_automation::desktop) | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-core` | cargo | no (0.2.4, registry 0.2.3) | gaps | port of live heardright_core engine and state code (single copy); settings and app_paths are a diverged fork of HeardRight main | gaps · ahead |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-engine` | cargo | yes | gaps | fork of HeardRight main (heardright-engine, renamed only) | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-ffi` | cargo | yes | gaps | fork of HeardRight main heardright_core ffi.rs and kws_ffi.rs with renamed C symbols, diverged | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-inference` | cargo | yes | gaps | diverged fork of HeardRight main (heardright-inference) | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-models` | cargo | yes | gaps | fork of HeardRight main (heardright-model-store, renamed plus small divergence) | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-runtime` | cargo | yes | gaps | fork of HeardRight main (heardright-runtime, renamed plus lever removal) | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-tauri` | cargo | yes | gaps | fork of HeardRight main src-tauri runtime, diverged by host-trait extraction | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-telemetry` | cargo | yes | best | fork of HeardRight main (heardright-telemetry, renamed plus host configuration) | fits |  |  |  |  |  |  |  |  |  |
| `rightkit-dictation-text-pipeline` | cargo | yes | gaps | fork of HeardRight main (heardright-text-pipeline, renamed plus lever removal) | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-hotkeys` | cargo | yes | gaps | faithful port of live HeardRight code (ptt_policy, fn-hotkey tap, middle mouse) plus Carbon/RegisterHotKey chord backends | gaps · adopted · 580L 3% |  | adopted · 1798L 1% |  |  |  |  | adopted |  |  |
| `rightkit-media-control` | cargo | n/a | missing | does not exist; HeardRight main src-tauri/src/media_control/* (about 700 lines) is the only implementation | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-overlay` | cargo | yes | gaps | faithful port of live HeardRight Windows no-activate, topmost, clamp and hover-gate code, plus a fresh overlay model HeardRight does not use | gaps · adopted · 19095L 1% |  | adopted |  |  |  |  | adopted |  |  |
| `rightkit-overlay-win` | cargo | yes | gaps | fresh implementation written against HeardRight main's native Windows pill; no counterpart in the live build | gaps · adopted |  | exception · 20798L 2% |  |  |  |  |  |  |  |
| `rightkit-speech-client` | cargo | yes | best | fresh implementation; no file matches a HeardRight source file | fits · local |  |  | ahead |  |  | ahead | ahead |  |  |
| `rightkit-text-polish` | cargo | yes | best | faithful port of live HeardRight code (formatting-only diffs, brand table parameterised) | fits · ahead · 18564L 18% |  |  |  |  |  |  |  |  |  |
| `rightkit-transcript` | cargo | yes | best | fresh implementation; no file matches a HeardRight source file | fits · local |  |  |  | adopted |  | adopted · 1519L 1% | adopted |  |  |
| `rightkit-vad` | cargo | yes | best | fresh implementation with a HeardRight-compatible option (Warmup::KeepState "as HeardRight does") | fits · local |  |  |  |  |  |  | adopted |  |  |
| `rightkit-voice-commands` | cargo | yes | best | faithful port of live HeardRight code (renamed, tests removed) | fits · ahead · 9784L 13% |  |  |  |  |  |  |  |  |  |
| `RightKitAppleCredentials` | swift | n/a | gaps | rewritten store; AppleCredentialHelperHost was moved out of HeardRight's HeardRightAppleCredentials CLI | gaps · 300L 8% |  |  | 321L 3% |  |  | 615L 6% |  |  |  |
| `RightKitAudio` | swift | n/a | gaps | rewritten code, not copied; WarmAudioCapture has the same name and role as HeardRight's | gaps |  |  |  |  |  |  |  |  |  |
| `RightKitCloudSync` | swift | n/a | gaps | rewritten engine; CloudKitWakeHost was moved out of HeardRight's macOS pill helper | gaps |  |  |  |  |  | 1212L 5% |  |  |  |
| `RightKitCredentials` | swift | n/a | best | rewritten code, not copied | fits |  |  |  |  |  |  |  |  |  |
| `RightKitDiagnostics` | swift | n/a | gaps | JSONLDiagnosticStore is an extraction of HeardRight's DogfoodTraceStore; the rest is rewritten | gaps · 463L 3% |  |  | 886L 2% |  |  | 18L 8% |  |  |  |
| `RightKitDictation` | swift | n/a | missing | does not exist; HeardRight iOS code (30 files / 10,514 lines on main) is the only implementation | gaps · 10514L 0% |  |  |  |  |  |  |  |  |  |
| `RightKitHubQA` | swift | n/a | wrong | fresh implementation of desktop Hub fakes; holds no HeardRight code | wrong · 16048L 0% |  | 3531L 1% |  |  |  |  |  |  |  |
| `RightKitImportQueue` | swift | n/a | best | rewritten code, not copied | fits |  |  |  |  |  | 680L 2% |  |  |  |
| `RightKitIntents` | swift | n/a | gaps | rewritten thin seam (1 file, 139 lines) | gaps · 1224L 2% |  |  |  |  |  |  |  |  |  |
| `RightKitLicense` | swift | n/a | gaps | rewritten code, not copied; verification and HTTP moved out of HeardRight | gaps · 1456L 6% |  |  | 1759L 6% |  |  | 753L 8% |  |  |  |
| `RightKitLiveActivity` | swift | n/a | best | rewritten lifecycle code; replaced HeardRight's RecordingActivityStateMachine | fits · 934L 0% |  |  | 173L 2% |  |  |  |  |  |  |
| `RightKitOverlay` | swift | n/a | gaps | fresh implementation (one file, 804 lines); neither live nor main's HeardRightPillMac depends on it | gaps |  |  |  |  |  |  |  |  |  |
| `RightKitPurchases` | swift | n/a | best | rewritten code, not copied | fits |  |  |  |  |  |  |  |  |  |
| `RightKitSpeech` | swift | n/a | gaps | rewritten code, not copied; replaced HeardRight's own Parakeet and Whisper runtime (14 files, 2,322 lines) | gaps · 5217L 8% |  |  | 121L  |  |  | 327L 5% |  |  |  |
| `RightKitStore` | swift | n/a | best | rewritten code, not copied; AppGroupFileLock is the same flock loop as live HeardRight | fits |  |  |  |  |  |  |  |  |  |
| `RightKitWatch` | swift | n/a | wrong | fresh implementation of a macOS FSEvents file watcher; holds no HeardRight code | wrong · 3476L 1% |  |  |  |  |  |  |  |  |  |
| `RightKitWatchBridge` | swift | n/a | gaps | rewritten code, not copied | gaps |  |  |  |  |  |  |  |  |  |

## Ingest & library (11)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/library-ui` | npm | yes | wrong | ScrapeRight desktop front end (src/queue) and HeardRight History/Transcribe pages (src/history) side by side | wrong · 2870L 83% |  |  |  |  |  | fits · ahead · 8194L 79% |  |  |  |
| `rightkit-browser` | cargo | no (0.3.10, registry 0.3.9) | gaps | own implementation, used by ScrapeRight, GenRight, Pulse | local |  | adopted | adopted · 3460L 3% | adopted |  | gaps · adopted · 932L 3% |  |  | adopted |
| `rightkit-catalog` | cargo | yes | best | fresh implementation (localisation catalog generator) |  |  |  |  |  |  |  |  |  |  |
| `rightkit-feed` | cargo | yes | best | fresh implementation (RSS/Atom parser) |  |  |  |  |  |  |  |  |  |  |
| `rightkit-fsindex` | cargo | yes | best | own implementation; Pulse already depends on 0.5.0 |  |  | fits · ahead · 2980L 5% |  |  |  |  |  |  |  |
| `rightkit-library` | cargo | yes | wrong | ScrapeRight store, history reader and enrichment only | wrong · 6049L 1% |  |  |  | wrong · 4596L 23% |  | fits · 4280L 75% |  | wrong · 1836L 4% |  |
| `rightkit-media-fetch` | cargo | yes | gaps | ScrapeRight audio/detect crates and extraction modules behind host traits |  |  |  |  | 1362L 2% |  | fits · 8515L 55% |  |  |  |
| `rightkit-transcript-library` | cargo | yes | gaps | HeardRight heardright_core delivery/history/net (near-identical to live build) | gaps · ahead |  |  |  |  |  |  |  |  |  |
| `rightkit-web-extract` | cargo | yes | best | ScrapeRight article extraction ladder over ArticleNet/BrowserHost traits |  |  |  |  |  |  | fits · ahead · 2734L 40% |  |  |  |
| `rightkit-web-session` | cargo | yes | gaps | Pulse core/src/browser only |  |  | gaps · 2734L 67% |  |  |  | wrong · 1434L 1% |  |  |  |
| `RightKitLibrary` | swift | n/a | missing | none | gaps · 4984L 0% |  |  |  |  |  | gaps · 1072L 0% |  |  |  |

## Mail & workspace (6)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/mail-ui` | npm | yes | gaps | MailRight front end with app store and 158 Tauri command names |  |  |  |  |  | fits · ahead · 17055L 89% |  |  |  |  |
| `@rightkit/workspace-ui` | npm | yes | wrong | MailRight Calendar/Drive/Contacts/Tasks/Meet views |  |  |  |  |  | fits · ahead · 2722L 79% |  |  |  |  |
| `rightkit-google-apis` | cargo | yes | gaps | MailRight Google REST clients and desktop OAuth |  |  |  |  |  | fits · ahead · 8778L 68% |  |  |  |  |
| `rightkit-google-identity` | cargo | yes | wrong | fresh protocol-only Sign in with Google |  |  |  |  |  |  |  |  |  |  |
| `rightkit-mail` | cargo | yes | gaps | MailRight search compiler, fusion, trust, image checks (pure rules) |  |  |  |  |  | gaps · ahead · 27603L 34% |  |  |  |  |
| `rightkit-mail-store` | cargo | yes | gaps | MailRight db/*, schema, sync apply, semantic index |  |  |  |  |  | fits |  |  |  |  |

## Redraws on the branch, 2026-11-11 (8)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `rightkit-agent-config` | cargo | n/a | gaps | CodeRight's coderight-config, moved out of rightkit-app-settings |  |  |  |  |  |  |  |  |  |  |
| `rightkit-agent-daemon` | cargo | n/a | gaps | CodeRight's agent daemon core and lifecycle, moved out of rightkit-daemon |  |  |  |  |  |  |  |  |  |  |
| `rightkit-agent-local-models` | cargo | n/a | gaps | CodeRight's local-model routing, moved out of rightkit-models |  |  |  |  |  |  |  |  |  |  |
| `rightkit-agent-telemetry` | cargo | n/a | gaps | CodeRight's agent telemetry, moved out of rightkit-logs |  |  |  |  |  |  |  |  |  |  |
| `rightkit-app-settings` | cargo | no (0.3.5, registry 0.3.4) | gaps | store core serving every app; CodeRight's agent configuration moved out in 0.4.0 | fits · ahead · 1470L 7% | fits · ahead | gaps · ahead · 7270L 1% | wrong · ahead · 10622L 47% | fits · ahead · 222L 4% | fits · ahead · 1032L 1% | fits · ahead · 163L 2% | ahead | ahead · 66L 2% | missing |
| `rightkit-daemon` | cargo | no (0.5.4, registry 0.5.3) | wrong | local service plus several other products; CodeRight's agent daemon code moved out in 0.6.0 | gaps · ahead | fits · ahead | fits · ahead | gaps · ahead · 13142L 15% | gaps · ahead · 384L 17% | fits · ahead · 5589L 3% | fits · ahead | gaps · ahead · 567L 4% | fits · ahead · 2628L 3% |  |
| `rightkit-logs` | cargo | no (0.3.10, registry 0.3.9) | gaps | logging core serving all nine apps; CodeRight's agent telemetry moved out in 0.4.0 | fits · local | fits · ahead · 166L 3% | fits · ahead · 845L 6% | gaps · ahead · 4075L 95% | fits · ahead · 62L 5% | fits · ahead · 314L 0% | fits · ahead · 686L 2% | fits · ahead · 105L 8% | fits · ahead · 166L 1% | missing |
| `rightkit-models` | cargo | no (0.5.9, registry 0.5.8) | wrong | two independent stores; CodeRight's agent local-model code moved out in 0.6.0 | gaps · ahead · 6182L 10% |  |  | fits · ahead · 1853L 47% | gaps · ahead · 3008L 2% | gaps · ahead · 340L 0% | fits · ahead · 1838L 6% | fits · ahead · 908L 1% |  |  |

## Remaining packages (24)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/blender-worker` | npm | yes | gaps | Not re-reviewed. Verdict copied from the row `rightkit-blender-worker` in matrix-rows-video-generative.json (Generative media family). That row's basis: GenRight workers/blender moved 2026-11-01 (npm wrapper around Python) |  |  |  |  | fits · ahead |  |  |  |  |  |
| `@rightkit/build-control-policy` | npm | private | gaps | RightKit's own agent and build admission tooling (ownership write hooks, session hook, heavy-write guard, host gate); no app code | fits |  | fits | fits |  |  |  |  |  |  |
| `@rightkit/dictation-ui` | npm | yes | wrong | HeardRight main's webview front end copied file for file (83 files have a same-path twin in heardright-main/tauri-app-next/src; 542 of 10,555 lines differ, mostly `.js` import suffixes and renamed helpers), plus CodeRight's voice-conversation loop under src/voice (3 files) | wrong · 8675L 96% |  |  | wrong · 1090L  |  |  | gaps |  |  |  |
| `@rightkit/hooks` | npm | yes | best | Fresh RightKit implementation: normalise a hook payload, run modules in order under a deadline |  |  |  | exception |  |  |  |  |  | adopted |
| `@rightkit/issues-tracker` | npm | yes | best | Fresh RightKit tool (`right-issues` writes issues.html from GitHub issues) | fits · adopted |  | fits · adopted | gaps |  | fits · adopted · 88L 93% | fits · adopted |  | fits · adopted · 88L 93% |  |
| `@rightkit/ml-lab` | npm | private | wrong | Not re-reviewed. Verdict copied from the row `rightkit-ml-lab` in matrix-rows-context-tooling.json (Foundation & tooling family). That row's basis: HeardRight training and evaluation scripts copied with their directory layout | wrong |  |  |  |  |  |  |  |  |  |
| `@rightkit/outdated` | npm | no (0.3.1, registry 0.3.0) | gaps | Fresh RightKit tool (`right-update`: check, apply, gate, land for cargo, pnpm, swiftpm, RightKit and external-tool pins) | fits |  | gaps · 324L 1% |  |  |  | fits |  |  |  |
| `@rightkit/send-ui` | npm | no (0.1.0, registry none) | gaps | Fresh RightKit implementation over rightkit-send's single JSON command (`createSendClient`, `useSend`, `SendPanel`) |  |  | gaps |  |  |  |  |  |  |  |
| `@rightkit/sync-worker` | npm | private | gaps | Fresh RightKit server for rightkit-sync-http/1 (D1 + R2), written for HeardRight's non-Apple sync | gaps |  |  |  |  |  |  |  |  |  |
| `rightkit-align` | cargo | private | gaps | Fresh RightKit implementation written to CutRight's contract request (docs/plans/2026-10-11-cutright-contract-asks...); no app code in it |  |  |  |  | gaps |  |  | gaps · 2477L 0% |  |  |
| `rightkit-android` | cargo | yes | gaps | Fresh RightKit implementation from HeardRight's Android plan; README: written without compiling or running anything | gaps · exception |  | gaps |  |  |  |  |  |  |  |
| `rightkit-contrast` | cargo | yes | best | Fresh RightKit implementation of the WCAG maths already in @rightkit/theme color.ts and RightKitUI.RightKitContrast |  |  | fits |  |  |  |  |  |  |  |
| `rightkit-dictation` | cargo | n/a | gaps | Summary row. Forks of HeardRight main's engine crates, renamed with host configuration added (hr-review-dictation-chain.md, section 7; per-crate rows in matrix-rows-aa-heardright.json) | gaps · 63039L 53% |  |  |  |  |  |  |  |  |  |
| `rightkit-owner-approvals` | cargo | yes | wrong | Pulse's signed owner approvals (approval_sig.rs, admin_sig.rs, protected_guard/*), per family-agent-platform.md |  |  | fits |  | wrong | wrong |  |  |  |  |
| `rightkit-platform-rules` | cargo | yes | gaps | Fresh RightKit implementation; PostRight is the consumer it was written for |  |  |  |  |  |  |  | gaps | fits · adopted · 107L 3% |  |
| `rightkit-policy-rules` | cargo | yes | gaps | Legion's legion-policy-model and legion-policy, moved 2026-11-01 (README). Legion source is not available, so this was not compared |  |  | gaps | gaps |  |  |  |  |  | fits · ahead |
| `rightkit-signal` | cargo | yes | best | Pulse hub's notch wake-up signalling (watch_notch, win_bridge) moved in shape and generalised (CHANGELOG) |  |  | fits |  |  |  |  |  |  |  |
| `rightkit-sysmem` | cargo | yes | gaps | Ember's `lightcraft-sysmem` crate (CHANGELOG names Ember as donor; Ember is not one of the surveyed apps) | gaps |  | gaps |  | gaps |  |  |  |  |  |
| `RightKitFS` | swift | n/a | best | Fresh RightKit implementation mirroring rightkit-fs and rightkit-daemon naming rules (app root, containment, atomic write, verified copy, append record) |  |  | fits | fits · 846L 4% |  |  | fits |  |  |  |
| `RightKitHotkeys` | swift | n/a | best | Fresh RightKit implementation shaped on Pulse's Mac notch (Carbon hot-key registrar, event-tap hub with handler chain, keystroke planner) |  |  | fits · 151L 4% |  |  |  |  |  |  |  |
| `RightKitHTTP` | swift | n/a | gaps | Fresh RightKit implementation mirroring rightkit-http (client, retry policy, address policy, redirect guard, streamed download) |  |  |  | gaps · 644L 3% |  |  |  |  |  |  |
| `RightKitHub` | swift | n/a | gaps | Pulse's SwiftUI hub base moved unchanged: 29 files, 27 identical, HubSettingsView.swift and HubExports.swift differ by 19 lines; RightKitHubQA's 3 files identical |  |  | gaps · 8117L 47% |  |  |  |  |  |  |  |
| `RightKitSwift` | swift | n/a | gaps | Mostly rewritten RightKit code (line matching finds little app code in it), except RightKitHub, HubQA, Engine and EngineClient which are Pulse's files | gaps · ahead |  | gaps · adopted | gaps · adopted |  |  | fits · adopted | gaps |  |  |
| `RightKitUpdate` | swift | n/a | gaps | RightKit rewrite shaped on Pulse's Mac updater (the Developer ID requirement text and relauncher match Pulse's lines) with feeds, channels, tiers and boot probation added |  |  | gaps · 703L 13% | fits · 127L 4% |  |  |  |  |  |  |

## System utilities (26)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/system-ui` | npm | yes | wrong | Pulse hub/src verbatim (api.ts differs by 18 lines) |  |  | fits · 7889L 75% |  |  |  |  |  |  |  |
| `rightkit-app-lock` | cargo | yes | gaps | MailRight state/preference/startup/flow/verify; Pulse hello_key and pin_window |  |  | gaps · 502L 47% |  |  | gaps · 1244L 29% |  |  |  |  |
| `rightkit-app-manager` | cargo | yes | gaps | Pulse core/src app_manager, apps, process_control, apps_windows |  |  | gaps · 13783L 45% |  |  |  |  |  |  |  |
| `rightkit-autostart` | cargo | yes | best | Fresh, split from the Tauri shell | fits |  | fits |  |  |  |  |  |  |  |
| `rightkit-clipboard` | cargo | yes | best | Fresh, split out of rightkit-tauri-shell |  |  | gaps |  |  |  |  |  |  |  |
| `rightkit-clipboard-history` | cargo | yes | gaps | Pulse Windows clip*.rs, near verbatim; Mac Swift twin absent |  |  | gaps · 12760L 88% |  |  |  |  |  |  |  |
| `rightkit-desktop-tweaks` | cargo | yes | wrong | Pulse windows/src/keys.rs (planned for rightkit-hotkeys); none of the planned Mac Swift |  |  | wrong · 2032L 0% |  |  |  |  |  |  |  |
| `rightkit-device-link` | cargo | yes | gaps | Fresh sans-IO reliable delivery layer (rightkit#1244); unrelated to Pulse's device link |  |  |  |  |  |  |  |  |  |  |
| `rightkit-device-link-types` | cargo | yes | best | Pulse registry and circle record types plus agent DeviceRef/EffectClass/Principal |  |  | fits |  |  |  |  |  |  |  |
| `rightkit-disk-cleanup` | cargo | yes | gaps | Pulse core/src scan, cleanup, rules, store, duplicates |  |  | gaps · ahead · 25046L 47% |  |  |  |  |  |  |  |
| `rightkit-launcher` | cargo | yes | wrong | Pulse's Windows launcher (panel.rs = windows/src/launcher.rs); Mac Swift twin absent |  |  | wrong · 22237L 36% |  |  |  |  |  |  |  |
| `rightkit-mesh` | cargo | yes | gaps | Pulse's in-tree rightkit-mesh plus core/src/devicelink, near verbatim | 873L 0% |  | gaps · 33474L 49% | 605L 0% |  |  |  |  |  |  |
| `rightkit-mesh-ffi` | cargo | private | gaps | Pulse's rightkit-mesh-ffi (lib, circle, store identical; pairing 57 lines changed) |  |  | fits |  |  |  |  |  |  |  |
| `rightkit-notch-ui` | cargo | yes | wrong | Pulse windows/src render, layout, motion, card (planned under overlay-win) |  |  | fits |  |  |  |  |  |  |  |
| `rightkit-notifications` | cargo | yes | gaps | Fresh |  |  | gaps |  |  |  |  |  |  |  |
| `rightkit-now-playing` | cargo | yes | best | Fresh |  |  |  |  |  |  |  |  |  |  |
| `rightkit-power` | cargo | yes | best | Fresh |  |  |  |  |  |  |  |  |  |  |
| `rightkit-presence` | cargo | no (0.1.1, registry 0.1.0) | best | Fresh, split from the Tauri shell |  |  | gaps |  |  |  |  |  |  |  |
| `rightkit-privileged-helper` | cargo | yes | gaps | Pulse engine/src/elevate.rs |  |  | gaps · 673L 22% |  |  |  |  |  |  |  |
| `rightkit-record` | cargo | yes | gaps | RightKit's own per-window recorder; no Pulse or HeardRight code | 439L 1% |  | gaps · 4656L 2% |  |  |  |  | adopted |  |  |
| `rightkit-send` | cargo | yes | gaps | Pulse's LocalSend implementation, already extracted and used by Pulse |  |  | fits · ahead · 9261L 2% |  |  |  | gaps · 891L 0% |  |  |  |
| `rightkit-system-monitor` | cargo | yes | gaps | Pulse core/src lib, monitor, process_inspect, drive_health, gpu_temp |  |  | gaps · ahead · 13212L 28% |  |  |  |  |  |  |  |
| `rightkit-win-ui` | cargo | yes | gaps | Pulse windows/src canvas, glyphs, surface, text_layout, raii, fileicon, verbatim |  |  | fits |  |  |  |  |  |  |  |
| `RightKitMeshLink` | swift | n/a | gaps | Pulse swift/MeshLink, identical file |  |  | fits |  |  |  |  |  |  |  |
| `RightKitNotch` | swift | n/a | missing | none |  |  | gaps · 17668L 0% |  |  |  |  |  |  |  |
| `RightKitSystemUI` | swift | n/a | missing | none |  |  | gaps · 21612L 0% |  |  |  |  |  |  |  |

## Video editing (16)

| Package | Kind | Published | Shape | Basis | heardright | viewright | pulse | coderight | genright | mailright | scraperight | cutright | postright | legion |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| `@rightkit/media-ui` | npm | no (0.3.5, registry 0.3.4) | gaps | Brand-neutral MediaPlayer, image viewer/editor, MaskPainter, VoiceCallBar (RightKit implementation) |  | adopted |  |  | gaps · adopted · 282L 2% |  | adopted · 698L 0% | adopted |  |  |
| `@rightkit/video-ui` | npm | no (0.1.1, registry 0.1.0) | wrong | CutRight src/ moved whole (generated index exporting every module) |  |  |  |  | wrong |  |  | fits · ahead · 16336L 96% |  |  |
| `rightkit-autoedit` | cargo | yes | gaps | CutRight cutright-cuts, -editorial, -variants, -rules merged, plus cmd/ (autoedit, rules, autonomy, mic, style) |  |  |  |  |  |  |  | fits · ahead · 14279L 65% |  |  |
| `rightkit-captions` | cargo | yes | gaps | CutRight cutright-captions moved; words/align/export are re-exports of rightkit-transcript |  |  |  |  |  |  |  | gaps · ahead · 3177L 48% |  |  |
| `rightkit-color` | cargo | yes | best | CutRight cutright-color moved |  |  |  |  |  |  |  | fits · ahead · 4108L 50% |  |  |
| `rightkit-compose` | cargo | yes | gaps | Fresh composition model (ticks, layers, eval; ScreenRight camera curve) + CutRight cutright-compose as feature `native` + GenRight still_finish as feature `still-finish` |  |  |  |  | gaps · 614L 73% |  |  | fits · ahead · 5679L 66% |  |  |
| `rightkit-compositor` | cargo | yes | gaps | CutRight cutright-compositor, -gpu, -effects, -masks merged (adapted from OpenCut Classic, MIT) |  |  |  |  |  |  |  | fits · 3088L 98% |  |  |
| `rightkit-ffmpeg` | cargo | no (0.3.1, registry 0.3.0) | gaps | Fresh RightKit core (resolve, verify, runner, probe, loudnorm); plus GenRight media probe and H.264 rule, CutRight delivery/intermediate policy, CodeRight agent media tools | ahead |  | ahead | gaps · adopted · 997L 95% | fits · ahead · 971L 54% |  | ahead | fits · ahead · 1338L 0% |  |  |
| `rightkit-gpu` | cargo | yes | gaps | Adapted from Ember crates/gpu; no surveyed app feeds it |  |  |  |  |  |  |  | gaps |  |  |
| `rightkit-playback` | cargo | yes | gaps | ScrapeRight compatibility-remux playback (source registry, cancel registry, fingerprinted cache, Tauri commands) |  |  |  |  |  |  | fits · adopted · 276L 6% | wrong · adopted · 1819L 2% |  |  |
| `rightkit-reframe` | cargo | yes | gaps | CutRight cutright-reframe moved (YuNet face detection via ONNX Runtime, tracker, plan, approval, render) |  |  |  |  |  |  |  | fits · ahead · 4222L 66% |  |  |
| `rightkit-review` | cargo | yes | gaps | Fresh shared core (Subject, RuleSet, Verdict, disposition, repair loop, ladder, qa, delivery) that CutRight and PostRight already call; CutRight cutright-qa moved as feature render-qa; PostRight phone surface as feature phone; Legion code review as features code/minimize |  |  |  |  | 683L 4% |  |  | fits · ahead · 5035L 66% | fits · ahead · 2409L 9% | ahead |
| `rightkit-shorts` | cargo | yes | gaps | CutRight cutright-shorts moved, plus cmd/moments (OCR index, search) |  |  |  |  |  |  |  | fits · ahead · 4992L 66% |  |  |
| `rightkit-timeline` | cargo | yes | gaps | CutRight cutright-timeline + cutright-fcpxml + cutright-otio merged |  |  |  |  |  |  |  | fits · ahead · 8011L 77% |  |  |
| `rightkit-video-project` | cargo | yes | gaps | CutRight cutright-project moved (project, ingest, render, source, probe, worker, loudness, lock, audio_sync, still) |  |  |  |  |  |  |  | gaps · ahead · 14524L 35% |  |  |
| `RightKitMedia` | swift | n/a | missing | not in RightKit |  |  |  |  |  |  |  | gaps · 1859L 0% |  |  |

## Wrong shape (36 packages; sub-crates that inherit a family verdict are not repeated)

| Package | Why | Reference app |
|---|---|---|
| `@rightkit/dictation-ui` | It is one app's whole front end (sidebar, title bar, licence, auto-update, theme, legal, settings registry, pill, pages) with a second app's unrelated voice loop beside it. No app depends on it; HeardRight main keeps the same files, and CodeRight's current tree no longer has the voice files. | heardright |
| `@rightkit/document-core` | A catch-all created to break an import cycle. Adopting any editor pulls in ViewRight's concrete Tauri bridge (57 command names), settings schema, theme list and PDF editor. | viewright |
| `@rightkit/document-workspace` | It is the whole ViewRight application: hard-coded legal gate, licence and update settings, format list, and a generated barrel exporting every module. A second app would inherit ViewRight's product decisions. | viewright |
| `@rightkit/library-ui` | Two apps' screens in two folders with no shared component, literal Tauri command names and one app's theme, licence and title bar. | heardright |
| `@rightkit/media-gen-ui` | The GenRight application frontend including a 4,003-line browser mock of the engine, legal documents, onboarding/license/app-update wiring and brand data. | genright |
| `@rightkit/ml-lab` | A private, unbuilt file dump of one product's experiments: no entry points, no dependency manifest, machine paths, dated experiment folders. Not a shared package and not publishable. | heardright |
| `@rightkit/system-ui` | Views call Tauri invoke directly with Pulse command names, carry Pulse strings and bundle chat, nearby, launcher settings, onboarding and legal; a second app cannot adopt it. | pulse |
| `@rightkit/tauri` | Exports one constructor per product (createViewRightTauriClient, createHeardRightTauriClient, ...), so the shared package names its consumers. The generic constructor is the right API. | pulse |
| `@rightkit/video-ui` | It is the CutRight application frontend, not a component library: Home, Settings, legal config, demo mode, app name, GenRight client, and Tauri invokes of commands that exist only in the CutRight shell. | cutright |
| `@rightkit/workspace-ui` | Every view imports mail-ui internals (store, hooks, ipc, icons), so the package cannot be used without the mail app's state. | mailright |
| `rightkit-adapt` | Two unrelated subsystems under one name with no shared type and no dependency between them; the Pulse one (the reference) has the suffixed name. | pulse |
| `rightkit-agent-engine` | The base crate and the -* family share a name but no code or dependency; nothing depends on the base crate. Task, approval and effect types exist twice with different meanings, and Principal/DeviceRef/EffectClass are copied in rightkit-device-link-types. | coderight for the turn loop; pulse for the task/authority contract |
| `rightkit-agent-gateway` | The package named 'agent gateway' is a mail product: it depends on rightkit-mail and its methods are mail verbs. Pulse's gateway shares none of its code. The common part (pairing, gate pipeline, audit, pending approvals, untrusted-content wrapping, redaction) exists twice and is not factored out. | pulse for gate order, approval binding, redaction and refusal codes; mailright for the paired-agent registry and SQL audit (the more general design: several agents, each with a profile and account scope) |
| `rightkit-agent-server` | It is CodeRight's application binary under a library name: 532 'coderight' strings, benchmarks, harness-radar data, and its own copies of the scheduler (3.0k), GitHub App (3.5k), workflow (7.2k) and Telegram (0.7k) modules. It does not depend on rightkit-scheduler or rightkit-github. | coderight |
| `rightkit-app-host` | A process-global host registry created by moving Pulse code; not a capability every desktop app adopts, so it should not sit in the mandatory set. Low confidence: README only. | pulse |
| `rightkit-approvals` | Three apps, three unconnected approval implementations in four crates; the crate with the shared name depends on rightkit-media-gen and has GenRight draft kinds built in, so no other app can use it. | pulse |
| `rightkit-daemon` | Agent daemon code moved to rightkit-agent-daemon (0.6.0). Still wrong: run mode and app roots, the remote HTTPS host with link and relay, and an approval workflow share the crate with the local service. | heardright |
| `rightkit-desktop-tweaks` | The crate holds the Windows keyboard hook wired to screenshots, snippets, wheel and strip, not the planned Mac Finder and window tweaks. | pulse |
| `rightkit-engine-host` | Built from a tier-4 app and not compared with HeardRight's engine supervisor or CodeRight's daemon; should not be mandatory until it is. Low confidence: README only. | heardright |
| `rightkit-google-identity` | Duplicates PKCE and ID-token verification already in rightkit-oauth (which google-apis uses); no dependant; endpoints marked unverified. | none |
| `rightkit-launcher` | The crate is Pulse's Windows launcher product (Pulse pages, strings, data folder) rather than a launcher core; the portable logic should be split from the Win32 panel. | pulse |
| `rightkit-library` | Plan merged four different capabilities under one name; the crate holds only ScrapeRight's artifact index, with a fixed social Kind enum and tier rules. | heardright |
| `rightkit-media-gen` | A 34,600-line crate that is the GenRight engine: 18 provider adapters, jobs, catalogue, routing, library, credentials, spend layer, voices, and private copies of code other RightKit crates own. Its API is JSON Value in and out, so a second app adopts it only as "GenRight embedded". | genright |
| `rightkit-memory` | Not two competing engines: Pulse's engine and CodeRight's client of that engine. But the crate named rightkit-memory is the client, it does not use the engine crates' types, and it carries a dead second retrieval router. | pulse |
| `rightkit-ml-lab` | A private, unbuilt file dump of one product's experiments: no entry points, no dependency manifest, machine paths, dated experiment folders. Not a shared package and not publishable. | heardright |
| `rightkit-models` | Agent local-model code moved to rightkit-agent-local-models (0.6.0). Still wrong: two independent on-disk stores and APIs (Store for MailRight and GenRight, suite::* for HeardRight, ScrapeRight and CutRight). | heardright |
| `rightkit-notch-ui` | The cell and mark set is Pulse's AI-usage product (Cell::Claude, Cell::Codex, accounts); a second app would inherit it. | pulse |
| `rightkit-owner-approvals` | Conclusion taken from family-agent-platform.md (section `rightkit-approvals (with rightkit-owner-approvals)`, lines 269-293): approvals exist as three unconnected implementations in four crates; this crate should become the `signed` proof of a redrawn rightkit-approvals core, with its redact.rs and audit.rs moved out. | pulse |
| `rightkit-pipeline` | Not in the assigned list but flagged in the task: it holds one app's client for one other app's CLI. The three feeders implement three sides of engine-to-engine calls and only CutRight's is here. | postright |
| `rightkit-prompt-recipes` | Prompt contracts per model and the recipe composer are reusable. The crate also ships the owner's brand list and a Kindle colouring-page workflow, and every module is a `studio.<area>.*` command handler bound to rightkit_media_gen::Context. | genright |
| `rightkit-qa-driver` | Superseded by rightkit-qa; no surveyed app imports it. It should not be in the set apps adopt. | none |
| `rightkit-remote-compute` | Rented GPU worker lifecycle (SSH, Vast.ai) with a time-billed spend bracket is a clear package. Half the crate is local model management, and worker records live in media-gen's settings table. | genright |
| `rightkit-social-analytics` | CutRight's retention-to-timeline-cuts command is video editing and drags video-project, timeline, autoedit and engine-host into a social crate. | postright |
| `RightKitHubQA` | Wrong shape: it is three files of fakes for the desktop Hub and depends on RightKitHub. The 48 iOS test files mapped to it stay with the app, as the mapping document's own APPTEST rows say. | heardright |
| `RightKitUI` | The module is fine for what it holds, but about 5.5k planned lines mapped to it are settings models, app entry points, File Provider and widget code, none of which is UI kit. | heardright |
| `RightKitWatch` | Wrong shape: it is an FSEvents file watcher (the target is empty on iOS), not Apple Watch code. The 19 Apple Watch files mapped to it belong with RightKitWatchBridge, RightKitAudio and a new watch-recording home. | heardright |

## Missing (7)

| Package | Note |
|---|---|
| `rightkit-media-control` | Planned in the inventory as NEW and not in RightKit. The delivery design recommends a small separate crate, or rightkit-now-playing gaining a control-other-players half, rather than rightkit-automation. |
| `RightKitAgent` | No Swift module exists. CodeRight's 30.7k lines are its whole iOS app; Pulse's are iOS screens. The reusable part is small: provider protocol, budgeted loop step, approval gate with device-lock confirmation, pairing. |
| `RightKitDictation` | Planned NEW and not in RightKitSwift. One of only two missing Swift packages HeardRight needs; it is the iOS dictation runtime built on RightKitAudio, RightKitSpeech, RightKitStore and RightKitLiveActivity. |
| `RightKitLibrary` | No Swift module exists; HeardRight's transcript store with tombstones and CloudKit sync and ScrapeRight's small artifact store are different things. |
| `RightKitMedia` | No RightKitMedia module in swift/RightKitSwift/Sources or Package.swift. CutRight's macOS media worker (AVFoundation timeline render, Vision, audio finish) has 46 matching lines anywhere. |
| `RightKitNotch` | No module in RightKitSwift; should be a generic notch surface, not a lift of Pulse's Features and Model. |
| `RightKitSystemUI` | No module in RightKitSwift; it is the SwiftUI twin of @rightkit/system-ui and should share that package's contract. |

## Actions before publication

- `@rightkit/agent-ui` (gaps): Define one ApprovalItem model (id, kind, scope, origin, summary, detail, digest, expiry) matching the Rust approvals core and render both MailRight cards and Pulse lists from it; Move mail-specific card content behind a render prop; drop the mail-ui dependency; Move Pulse hub pages that are not approvals to a remote-gateway UI package; Replace product strings with the appName prop already present (appName.ts)
- `@rightkit/app-update` (gaps): see rightkit-app-update
- `@rightkit/blender-worker` (gaps): Split generic worker (contract, worker, scene, pngcheck, install, qualify, schemas) from product look-dev (fenrir, stonewash, product_* modules) which becomes a host-supplied assets/recipes pack via RIGHTKIT_BLENDER_ASSETS_DIR; Fix the plan name to the npm package
- `@rightkit/build-control-policy` (gaps): Do not publish; keep private and say so in the matrix; Record that app scripts depend on a host-installed bin, so a fresh machine needs the install step before any app script runs; Version string 0.1.5-phase1: settle a normal version before anything else pins it
- `@rightkit/dictation-ui` (wrong): Redraw as three units: (a) dictation pill overlay (pill/*, hint catalog) with an event/command interface; (b) dictation pages (Home, Dictation, Vocabulary, Polish, replacements, snippets) over a typed `DictationClient` the host supplies; (c) voice conversation (src/voice) moved to the agent UI family or its own package; Delete the licence, auto-update, theme, legal, sidebar, title-bar and window-control copies; use the owning packages; Storage keys, event names and theme code from host configuration, not `heardright.*` literals; Move history helpers next to the History page in @rightkit/library-ui and drop that package's dependency on this one; Decide what happens to the published 0.1.0 (deprecate once the split packages exist); Then HeardRight deletes its 83 twins; until then this package is a duplicate of HeardRight's src
- `@rightkit/doc-render` (best): Settle the Mermaid policy with markdown-editor; Publish 0.2.2
- `@rightkit/document-core` (wrong): Move documents/pdf out; Bridge becomes an interface plus a default Tauri implementation configured with command and scheme names; Move themes, settings sections and file associations to the workspace package or ViewRight; Remove benchmark harness and QA globals; Generate the TS format table from the rightkit-docs file or add a gate check; Add the package to the plan
- `@rightkit/document-workspace` (wrong): Move legal gate, licence and update sections to the ViewRight shell or props; Take format-to-pane table as a registry argument; Written entry point instead of the generated barrel; One setHostAppName in document-core, no module-level reads; Rename viewright:* events to a neutral prefix across the five packages
- `@rightkit/docx-editor` (gaps): Make the host toolbar work on published 1.9.0: external toolbar via DocxEditorRef and selection events, or a dist-level pnpm patch registered in root and apps, or upstream PR then pin; Keep the source patch only as upstream PR text; ViewRight deletes vendor/docx-editor after cutover; Plan the move to the @docx-editor.dev 2.x line
- `@rightkit/hooks` (best): Resolve or delete the stale P1 note
- `@rightkit/html-editor` (gaps): Scheme names and CSP sources from host configuration; Event wiring as props; Written entry point
- `@rightkit/i18n` (gaps): add xcstrings import or generate xcstrings from JSON; re-map Pulse table to rightkit-i18n
- `@rightkit/issues-tracker` (best): Correct the README status line; Drop the two generated-file rows from the plan
- `@rightkit/legal` (best): remove the deprecated write port once HeardRight and ViewRight move
- `@rightkit/library-ui` (wrong): define LibrarySource/row/detail contract with no IPC; list from HeardRight History, reader from ScrapeRight Detail; move queue components to a queue package; remove theme/licence/update/title bar/settings/mocks; drop dictation-ui dependency
- `@rightkit/license` (gaps): publish 0.3.1; Publish 0.3.1 (createAccountClient, requestLink, redeemLink, retryPendingReleases) and bump from 0.3.0
- `@rightkit/mail-ui` (gaps): MailClient interface with Tauri binding as subpath; move workspace/agent IPC out; mocks to dev entry; hostAppName
- `@rightkit/markdown-editor` (gaps): Link, undo and asset ports as props; One Mermaid sanitizer with ViewRight's policy, in doc-render; Neutral scheme and event names; keep '<!-- viewright: -->' hints accepted; Written entry point
- `@rightkit/media-gen-ui` (wrong): Split into components a second app can mount (Generate form from gen-params, JobCard/Jobs, Gallery, Voices, ModelsPanel) over an injected API client; shell stays in GenRight; Move the mock to a dev-only entry or the app; Use MaskPainter; delete MaskBrush; One generation client shared with video-ui
- `@rightkit/media-ui` (gaps): Add invert to MaskPainter/exportMaskPng; Remove MaskBrush from media-gen-ui
- `@rightkit/ml-lab` (wrong): do not publish; keep experiments (bakeoff/*, tools/zephyr_*, local-model-training) in a HeardRight research repository; extract only reusable pieces: WER scoring and ASR adapter harness to the bench package, CoreML/ONNX conversion to a model-tools package with a pinned Python manifest
- `@rightkit/onboarding` (gaps): continueLabel and blockedHint accept a function of step context; publish 0.6.4 card frame; add a native completion-flag port; Let continueLabel and blockedHint accept a function of the step context (O2); Publish 0.6.4 (frame="card") and bump the pin from 0.6.3 so about 20 CSS overrides can go (O3, O4); Step count should exclude the hidden Agreement step (O3); Model row: elapsed-seconds while preparing is lost (O7, minor); Adopt the native legal receipt create port instead of deprecated write (O11)
- `@rightkit/outdated` (gaps): Decide whether a `donor` source kind (repository + recorded commit) belongs here; if yes add it so Pulse's poller can go, if no remap those files to stay with the app; Publish 0.3.1 and fix the README version
- `@rightkit/pdf-ui` (gaps): Build and run 0.3.0 against ViewRight's four cases in its changelog; Move the PDF editor from document-core onto PdfViewer with pageLayers; Delete the duplicate transport and prompt; Publish 0.3.0
- `@rightkit/qa` (best): Re-map the mock files to their UI packages in the plan
- `@rightkit/react-shell` (gaps): add maximizable option to CaptionControls; document one theme path and drop the others; consider a palette-recents hook; Add a maximizable option to CaptionControls (R3); --ink means text in RightKit and chrome in HeardRight; product surfaces renamed to --hr-ink, missed uses not audited (R5); Title-bar theme toggle is gone; whether AppShell draws its own was not read (R2); Bump pin 0.7.2 to 0.7.3
- `@rightkit/release` (gaps): Move Legion and ASR rule files behind a per-app rule plug-in; Split ownership governance and RightKit's own publishing from the app release CLI; Add one verified asset-fetch step to replace the per-app fetch scripts
- `@rightkit/send-ui` (gaps): Build and test it against rightkit-send commands; Check parity with Pulse's view (devices, incoming requests, per-device trust), then delete NearbySettings from system-ui; the chat rows go to the chat settings package; Publish after that
- `@rightkit/settings-ui` (gaps): section factories accept a key map or direct props; revision as string or opaque token; theme vocabulary option on appearance section; Type revision so a 64-bit store revision fits (U3); main works around it with a 64-entry handle table; Standard license/updates/models sections do not match HeardRight screens (U2, U6); Appearance section theme vocabulary (system/light/dark) needs an app adapter to app_theme (U4)
- `@rightkit/social-ui` (gaps): mocks to dev entry; product name via hostAppName
- `@rightkit/sync-worker` (gaps): Owner decision: parked or supported. Make the workspace file, this README and the rightkit-sync README say the same thing; Keep private; never publish to npm. If supported, put it back in the workspace so it is built and tested
- `@rightkit/system-ui` (wrong): Inject the transport; Remove product strings; Move non-system views to their packages; Add or drop Monitor/Overview
- `@rightkit/tauri` (wrong): remove per-app constructors; merge into @rightkit/tauri-shell or reduce to defineTauriCommands and createNativeFetch
- `@rightkit/theme` (gaps): add Swift token output or generate from rightkit-theme; re-map scripts; Fix the mapping of the three iOS Swift files (T4); App: default font "system" in web vs "hanken" in Rust, and the patch allowlist rejects "system" (T3); App: confirm the palette change from live is intended (T1); Bump pin 0.3.1 to 0.3.2
- `@rightkit/video-ui` (wrong): Split: (1) editor components (Timeline, Inspector, ColourInspector, CaptionInspector, TranscriptPanel, Player, RangePreview) taking an injected engine adapter with no Tauri import; (2) the CutRight shell (views, settings, legal, demo) stays in the app; Replace the generated export-everything index with a deliberate API; Move genright.ts to a generation client package shared with media-gen-ui
- `@rightkit/workspace-ui` (wrong): own client interface and state slices; move Drive/Calendar hooks, nav and IPC from mail-ui; shared icons/account switcher in a lower package
- `rightkit-adapt` (wrong): rename rightkit-adapt-taste to rightkit-adapt (Pulse governed learning is the reference); move CodeRight learning to its own crate (for example rightkit-agent-learning) or into rightkit-agent-skills; move harness_radar back to CodeRight or to a neutral watch-list crate; collapse the two proposal stores
- `rightkit-agent-config` (gaps): compare with Pulse's agent configuration before publishing as the shared shape; publish 0.1.0
- `rightkit-agent-daemon` (gaps): decide whether it folds into rightkit-agent-server when that crate is split; publish 0.1.0
- `rightkit-agent-demo` (gaps): Remove product names; Not read for behaviour
- `rightkit-agent-engine` (wrong): Rename the base crate rightkit-agent-task-contract (or fold into engine-protocol) and make rightkit-agent-engine a facade over the -* crates; Delete the duplicated Principal/DeviceRef/EffectClass; one home (device-link-types or the contract crate), re-exported; Move voice.rs to the dictation/voice family or make chat label and policy ref host parameters; Move task_cli.rs out to a CLI crate; Adopt Pulse's rule in the CodeRight loop: approval always bound to a digest of the exact arguments, destination, expiry and revision; map ExternalEffectSettlement::InterruptedUnknown to TaskStatus::Uncertain; Rename CodeRight TaskNode/TaskState to PlanTask* so the two task concepts cannot be confused
- `rightkit-agent-gateway` (wrong): Extract a generic core as rightkit-agent-gateway: agent registry (hashed key, profile, scopes, revoke), gate pipeline (key, lock, profile, scope, rate limit, handler, one audit row), audit store trait (SQL and JSONL), Origin/door, Refusal with stable codes, untrusted-content wrapper; Rename today's crate rightkit-mail-agent-gateway (mail family) on top of the core; Rebase rightkit-remote-gateway on the same core; move ChatGPT and media-app specifics behind host-registered tools; Move all three approval stores to rightkit-approvals; One redaction module (Pulse's masking) plus MailRight's leak assertion as its test helper
- `rightkit-agent-local-models` (gaps): publish 0.1.0
- `rightkit-agent-orchestration` (gaps): Put repo-graph and code-audit coupling behind features; Not read for behaviour
- `rightkit-agent-policy` (gaps): Have remote-gateway's terminal command policy call this crate's classifier, or state why not; Check overlap with rightkit-policy-rules
- `rightkit-agent-server` (wrong): Split: a small generic rightkit-agent-server (session hosting, daemon, API router, permission and session routes) and leave CodeRight's product routes in the app; Delete src/github_app and src/scheduler in favour of the crates; move execution/delivery/persistence into rightkit-scheduler; Move each misplaced module to the package the plan named, or change the plan
- `rightkit-agent-telemetry` (gaps): publish 0.1.0
- `rightkit-agent-tools` (gaps): Split a thin rightkit-agent-tool-api (trait, schema, right-ctl) from rightkit-agent-coding-tools; Use rightkit-scheduler timezone
- `rightkit-agent-transcript` (gaps): Point CodeRight's exporter at TranscriptEventV1; Fix plan target
- `rightkit-agents` (gaps): Split: rightkit-agents (sans-IO protocol codecs + AgentEvent + discovery) and rightkit-agent-drivers (driver_host, providers, ACP); One event model: map NormalizedEvent onto AgentEvent or the reverse, delete the other; One binary lookup; Remove 'coderight' strings; re-target Pulse transcript files to rightkit-agent-transcript and PostRight drafting to an LLM package
- `rightkit-ai-accounts` (gaps): Port the remaining Pulse modules or re-map them to SHELL; Not read for behaviour
- `rightkit-align` (gaps): Remap cutright-speech to the ASR family; Add fixtures on real narration audio (README: pending on CutRight audio); Set publish = true and publish so GenRight's dubbing can use it
- `rightkit-android` (gaps): Build it once for arm64-v8a and x86_64 and run the bridge in an emulator before the next publish; Decide whether the mesh Kotlin wrapper lives here or beside RightKitMeshLink
- `rightkit-app-host` (wrong): decide whether it is mandatory; reconcile with agent_config state root
- `rightkit-app-lock` (gaps): Move hello_key and pin_window; Add generation-checked transition core; Add Swift AppLock with grace period parameter
- `rightkit-app-manager` (gaps): Add Windows inventory/leftovers and startup items
- `rightkit-app-settings` (gaps): extend the manifest for per-platform defaults and ranges, or retire one generator; document Document vs Store<T>; publish 0.4.0; move agent_config to its own crate; publish 0.3.3+; Publish and pin >= 0.3.3 so Store::snapshot() gives per-field repair on the UI read (S11); main pins 0.3.1; Windows replace is rename, not ReplaceFileW/write-through (S10); power-loss behaviour not verified; Check stat-per-snapshot cost on hot paths such as the hotkey hook (S8, suspicion)
- `rightkit-app-update` (gaps): move the runtime-neutral update core out of rightkit-tauri-shell into this crate; use Pulse windows/src/update.rs as the Windows reference
- `rightkit-apple-ops` (gaps): confirm no overlap with packages/release upload code; split App Store Connect client from build analysis and profiling if a second consumer needs only one
- `rightkit-approvals` (wrong): Redraw rightkit-approvals as the core: Request {id, kind, scope, origin, client, bind digest of exact arguments, summary, detail, created, expires}; states pending/approved/denied/consumed/expired; single-use claim; caps and decline cooldown; ApprovalStore trait (memory, file, SQL); Proof trait (owner click with request digest, launch-secret HMAC, signed approver key); Keep Pulse's rules as the reference: only the owner's app decides, the click is bound to a digest of what was shown, an approval is spent by one act; Move GenRight kinds (eyes, blender_finish) to a media-gen approvals crate that registers kinds; drop the media-gen dependency; Fold rightkit-owner-approvals in as the 'signed' Proof (or keep as -signed sub-crate); move its redact.rs and audit.rs out; Port agent-gateway/approval_store.rs and remote-gateway/approvals.rs onto the core
- `rightkit-asr` (gaps): Take main's tdt_beam rank scratch buffer (A8); Put the KWS parity tests and fixtures the changelog cites into the crate (S4); parakeet_coreml: port command-locator alignment, window health, hypothesis batching, fused graph (S5); whisper: decide on the Windows CLI fallback (S8); Reconcile onnx_tdt with heardright-onnx-asr, then HeardRight deletes its crate and three parakeet-core files (S10); Pin: 0.4.2 changes Windows duration cap 2 to 4; main already overrides to 4 (S6)
- `rightkit-audio` (best): Route helper matches by exact platform id; HeardRight must convert its saved setting first, then drop the wrapper; Bump pin 0.3.1 to 0.3.4; whether route.rs exists in 0.3.1 not verified
- `rightkit-autoedit` (gaps): Compare rules/ladder.rs with rightkit-review ladder and keep one; Move or justify cmd/mic.rs; Rename schema ids; Port edit-plan commands once the decision ledger has a home
- `rightkit-automation` (gaps): Add desktop::delivery (DeliveryRules, capture_destination, choose_destination, deliver, DeliveryObserver): paste-first, receipt, no activation, no Policy::check; HeardRight: enable desktop-context, call set_own_window_owner_prefix("heardright") at startup, delete heardright_platform; Move in the persistent UIA worker, WezTerm pane channel and macOS paste probe; Make proof.rs the single home of the pure rules; fold the in-RightKit duplicates; Rewrite deliver_text tier 4 on the same transaction core; split policy lists (never_typed_into vs never_driven_by_agent); Owner decisions first: success rule, Enter without read-back on macOS, allowlist; then rewrite LOCKED_RULES.md and MOVED_FROM.md
- `rightkit-autostart` (best): publish; check against pulse/windows/src/autostart.rs so the notch can adopt it
- `rightkit-bench` (gaps): add the shared shape: Manifest{cases}, CaseResult, Receipt (JSON + Markdown), validate(receipt), Gate(thresholds); ship the same receipt schema as an npm module (@rightkit/qa/bench or @rightkit/bench) for the .mjs runners; remove readable_text and depend on rightkit-web-extract; allow a seeded temporary database path alongside owner probes; remap CodeRight api_benchmarks to rightkit-agent-server
- `rightkit-blender-worker` (gaps): Split generic worker (contract, worker, scene, pngcheck, install, qualify, schemas) from product look-dev (fenrir, stonewash, product_* modules) which becomes a host-supplied assets/recipes pack via RIGHTKIT_BLENDER_ASSETS_DIR; Fix the plan name to the npm package
- `rightkit-bot-messaging` (gaps): Move telegram and remaining bridge code here
- `rightkit-browser` (gaps): add blocking adapter; host cookie sign-in flow
- `rightkit-cache` (gaps): Move prefix to rightkit-llm
- `rightkit-captions` (gaps): Safe zones keyed by platform-rules ids or moved there; Port caption commands under the chosen command rule; Rename cutright.caption-cue.v1
- `rightkit-catalog` (best): list under i18n family
- `rightkit-chat-bridge` (gaps): Remove remaining Pulse strings; Make ai-accounts an optional feature; Check discovery overlap with agent-transcript
- `rightkit-child-process` (best): Run kill_on_parent_exit on a signed macOS build: it double-forks the multi-threaded shell without exec (P3); Bump pin 0.5.7 to 0.5.10
- `rightkit-clipboard` (best): Add change listening and sequence number if clipboard-history is to use it
- `rightkit-clipboard-history` (gaps): Split strip UI from the store; Use rightkit-clipboard for raw access or state why not; Single json module; Decide the Mac twin's home
- `rightkit-code-audit` (gaps): delete or wire orphan native_providers/security/history_qualification_record.rs (408 lines, no mod declaration); replace Legion names in paths, ids and messages or document them as fixed schema ids; explain the 100k-line difference from the plan (tests, fixtures or code not moved); split 5,399-line commands/audit.rs and 6,423-line reasoning/ingest.rs
- `rightkit-color` (best): none for the library; port colour_cmd under the chosen command rule
- `rightkit-compose` (gaps): Move still_finish out (media-pipeline or media-gen finish); One Ticks/Fps/Rational definition used by compose and timeline; Split `native` into a crate (e.g. rightkit-video-preview) that also receives CutRight playback.rs session logic
- `rightkit-compositor` (gaps): GpuContext::from(rightkit_gpu::GpuContext) so device creation, backend policy and the crash sentinel come from rightkit-gpu; keep pipelines/samplers here
- `rightkit-content-plan` (gaps): add content records and digests; README; tests
- `rightkit-content-proof` (gaps): move claim review over a store trait; remap acquisition.rs
- `rightkit-context` (gaps): move the membrane CLI client from rightkit-memory into rightkit-context-client; move context_telemetry out of memory-store into context-runtime or its own module crate; Pulse host files (engine/src/membrane_child.rs, membrane_enroll.rs, membrane_cmds.rs, 2,151 lines) need a supervisor API in the package
- `rightkit-contracts` (gaps): Take it out of the mandatory set or split daemon-api from the content contracts
- `rightkit-crash` (gaps): Keep one install entry point and deprecate the other; HeardRight capture gaps; Add an option for macOS sample duration; it is 1 s for every kind, live used 2 s (C3); Hang native capture repeats at most once per 10 minutes; main does not override (C7); Publish 0.3.8 (admit_crash_artifacts) and bump the pin from 0.3.6; Pre-upgrade crash_*.log reports are dropped from exports (C1); UI-stall sampling option lost: main passes capture None (C8)
- `rightkit-credentials` (best): macOS reads only the default keychain; an item elsewhere reads as absent (C3); Verify keyring-core target and blob layout equals KeyringCompat (C2, source not in tree); Windows persist type may change on rewrite (C9, suspicion); Bump pin 0.5.0 to 0.5.1
- `rightkit-crypto` (gaps): Single hashing owner; Drop unused base64/getrandom deps noted in the HeardRight review; Bump pin 0.1.0 to 0.1.3
- `rightkit-daemon` (wrong): move mode/Suite/AppRoot/root_cutover to one roots crate or rightkit-fs; split remote/link/relay into their own crate, leaving local service and loopback; GenRight and CutRight use discover_apps_in_with instead of their own discovery; Move mode/Suite/AppRoot/root_cutover to one roots crate (or rightkit-fs) and make daemon, models and qa depend on that; Move agent_daemon_core/agent_daemon_runtime to rightkit-agent-server; Split remote/link/relay into their own crate, leaving local service + loopback; Make GenRight and CutRight delete their discovery.rs in favour of discover_apps_in_with; Carry the HeardRight gaps (verify_peer pin, SID query export); Bump pin from 0.5.1 to >= 0.5.3, then HeardRight deletes peer_trust.rs; Export current_user_sid_string (pub(crate) today); HeardRight: remove the four unused dependency declarations; Owner note about a suite-root guard in the two main.rs files is stale (D2)
- `rightkit-db` (gaps): Add verified in-place compaction for keyed databases, from MailRight db_compaction; Decide whether a connection gate (writer, read pool, app lock) is a package type or stays MailRight's; HeardRight gaps from hr-review-storage-network; Check the lifecycle can keep the quarantined file until a CloudKit replay receipt arrives, then HeardRight adopts encrypted-lifecycle and quarantine helpers; Verify the Windows OpenSSL-to-LibTomCrypt open of a real database (D3, claimed in rightkit#1447); Symlinked database file is now refused (D5); RIGHTKIT_SQLITE_JOURNAL_MODE overrides WalStrict in release builds (D6); Interrupted rotation leaves the key only in the staging account 0.1.213 never reads (D11); Bump pin 0.5.5 to 0.5.6
- `rightkit-decimal` (gaps): First consumer before mandatory; not verified
- `rightkit-deep-link` (best): publish
- `rightkit-desktop-tweaks` (wrong): Move the hook hub to rightkit-hotkeys with a generic handler chain; Keep only shortcut mapping here; Create the Swift side
- `rightkit-device` (best): Rename or move the log and diagnostic host adapters; Empty stored value is now an error, never replaced (V4, fail-closed edge)
- `rightkit-device-link` (gaps): Rename or fold in; confirm a consumer before publishing
- `rightkit-dictation` (gaps): Carry out actions A1 to A11 of hr-review-dictation-chain.md (per-crate lists are in matrix-rows-aa-heardright.json); Replace the plan's single `rightkit-dictation` row with the eleven crate names
- `rightkit-dictation-app-runtime` (best): Publish the crate (A11); HeardRight: switch after telemetry and models, configure EngineHost
- `rightkit-dictation-command-host` (gaps): Reduce delivery/insert.rs to a call into the new rightkit-automation deliver entry point, or the duplicate just moves; Publish the crate (A11); HeardRight: switch after the automation desktop step; configure HostIdentity
- `rightkit-dictation-core` (gaps): Settle the app-data root rule (injected root vs configured name) and expose the winner (A6); Keep configure_legal_contract; HeardRight adapts the two Option call sites; HeardRight: use rightkit_dictation_core::{settings, app_paths} and delete its copies; Publish 0.2.4; main pins 0.2.3
- `rightkit-dictation-engine` (gaps): Drop the [[bin]] and keep the library facade, or give run a config argument (A9); Publish the crate (A11); HeardRight: engine shrinks to main.rs with configure calls, build.rs and entitlements
- `rightkit-dictation-ffi` (gaps): Polish entry point takes or looks up the brand table (A5, P3); Decide whether C symbols stay heardright_core_* for existing Swift callers or Swift renames to rightkit_dictation_*; Publish the crate (A11)
- `rightkit-dictation-inference` (gaps): Reconcile kws.rs with main's kws_cascade.rs into one file (A7); Remove the uncalled SpeechVad::calibrate (A10); Depends on rightkit-asr taking the tdt_beam scratch buffer (A8); Publish the crate (A11); HeardRight: configure display name
- `rightkit-dictation-models` (gaps): Restore DEFAULT_WORKER_HOST to the real host or derive it from the archive URL (A4); Env name RIGHTKIT_MODEL_WORKER_BASE replaced HR_MODEL_WORKER_BASE; check the feature gate; Publish the crate (A11); HeardRight: configure bundle id and pass app version to legacy_compatibility_required
- `rightkit-dictation-runtime` (gaps): Port cli.rs and its dispatch in entry::run, with the launcher host-supplied (A1); crash_capture: dump helper name and engine version prefix from configuration (A2); Settle the app-data root rule with rightkit-dictation-core (A6); Remove spawn_dummy_worker (A10); Publish the crate (A11)
- `rightkit-dictation-tauri` (gaps): Publish the crate (A11); HeardRight: add impl DictationHost over its delivery module and call install_host at startup; Decide whether destination policy stays host code or moves to rightkit-automation
- `rightkit-dictation-telemetry` (best): Publish the crate (A11); HeardRight: re-export it and call configure_dump_helper_name and configure_feedback_endpoint
- `rightkit-dictation-text-pipeline` (gaps): Make User-Agent, prompt-version header name and diagnostic prefix host-configurable or restore the HeardRight values (A3); Publish the crate (A11); HeardRight: call configure_brand_homophones and configure_product_terms in the engine main
- `rightkit-disk-cleanup` (gaps): One owner for snapshot.rs with host-supplied schema name; Add the scan runner; Re-map the journal to job-queue/agent engine
- `rightkit-docs` (gaps): Move 'files' into rightkit-document-workspace or make that crate depend on it and delete its copies; Keep the format table in one place
- `rightkit-doctor` (gaps): single report builder; decide where CodeRight's doctor dimensions live; move clipboard copy to rightkit-clipboard
- `rightkit-document-workspace` (gaps): Use or absorb rightkit-docs 'files'; delete duplicate types and the DocumentBackend seam; Optional tauri feature or companion crate registering the command set; Publish 0.1.4
- `rightkit-engine-host` (wrong): compare with HeardRight and CodeRight engine hosting before publishing as mandatory; move project.rs to rightkit-video-project; keep one doctor report (in rightkit-doctor) and delete engine-host/src/doctor.rs; produce per-app inventory rows so adoption can be measured; check HeardRight and Pulse engine entry points against the registry before publication
- `rightkit-evidence` (gaps): Move CodeRight evidence modules from agent-server; Establish which app owns the -* crates before publishing
- `rightkit-ffmpeg` (gaps): Move agent_media to rightkit-agent-tools; drop the agent-policy/tokio optional deps; One encoder decision API: keep pick_h264 + qualify_encoder as the mechanism and one policy function with options {qualify, allow_software, allow_mpeg4}; h264_rule keeps only the error code/details/is_init_failure; delete select_delivery vs select_qualified_delivery split; Delete rightkit-media-gen media.rs and host/h264.rs in favour of this crate; rightkit-timeline uses encoders::Encoder; Confirm the bundled ffmpeg is the catalogued build or file transcription fails (F2); Decide the re-pin from 0.2.13 to 0.3.1; 0.2.13 API not verified; HeardRight: collapse the three path lists into one passed to RightKit; No decode timeout; cancel token never triggered (F5)
- `rightkit-file-watch` (best): Confirm one backend under the three front ends and document which to pick
- `rightkit-fs` (gaps): One root resolver (see rightkit-daemon action); One hashing owner: fs::digest re-exports rightkit-crypto or the reverse; HeardRight F2/F3/F6 from hr-review-storage-network; Decide whether any shipped install relied on HR_APP_DATA_DIR or a redirected $HOME (F2, F3); Windows replace is rename, not ReplaceFileW: different ACL inheritance, not write-through (F6); HeardRight: replace the remaining hand-written atomic and hash helpers and drop the dirs dependency; Check the mobile entry injects the data root (F4, suspicion); Bump pin 0.4.12 to 0.4.13
- `rightkit-github` (gaps): Delete the agent-server copy; Cut the orchestration and review dependencies down to shared contract types
- `rightkit-google-apis` (gaps): scopes as parameter with per-API constants; cargo features per API; port tests
- `rightkit-google-identity` (wrong): merge into or re-base on rightkit-oauth; verify endpoints
- `rightkit-gpu` (gaps): Wire into rightkit-compositor before publishing as the adopted shape
- `rightkit-hooks` (gaps): Drop the llm dependency or gate it; Remove product names
- `rightkit-hotkeys` (gaps): Add hooks for the lost diagnostics: fn+Escape accepted, tap re-enable count, ptt_hold_suppressed (H2 to H4); Move diag off the event-tap callback thread (H5); Derive Serialize on HotkeyConflict (H7); Add a Windows push-to-talk tap and Windows middle-mouse type, or move HeardRight's hooks onto LowLevelHooks; Carry hold duration, stop reason and key in FnHotkeyCallbacks (H1); Publish 0.1.3; main pins 0.1.2
- `rightkit-http` (gaps): Remove direct ureq use from rightkit-models; HeardRight gaps; Add a parity preset that takes a single timeout, or HeardRight shares one constructor; New public-web address policy refuses private and loopback hosts (H2); confirm no split-DNS user is affected; HeardRight: delete the ureq error-text shim once logs stop depending on ureq wording; Bump pin 0.3.9 to 0.3.12
- `rightkit-i18n` (gaps): publish; provide catalog conversion from Pulse's table
- `rightkit-identity` (gaps): Read the four pairing implementations side by side and keep one before publishing
- `rightkit-instance` (best): publish
- `rightkit-job-queue` (gaps): Decide one engine supervisor (job-queue vs daemon) and re-export from the other; Add a registry/read-model hook so ScrapeRight can drop its parallel in-memory job authority, or record it as an app need; Carry the HeardRight gaps from hr-review-process-runtime; Worst-case quit can take about 6 s instead of 3 s (J4, suspicion); Write ack 2 s became send timeout 5 s (J5); Bump pin 0.5.1 to 0.5.4
- `rightkit-launcher` (wrong): Split portable core (calc, config, notes, rates, files, commands) from the Win32 panel; Product pages and strings via host; Single json module; Move response_layout; Decide Mac: FFI to core or Swift module
- `rightkit-library` (wrong): remove or properly home artifacts.rs/path_policy.rs; host-supplied kind; delete duplicate Kind; drop media-fetch dependency; single publish_replacing in rightkit-fs; move tier rules to host; port ScrapeRight history tests; correct plan mapping for HeardRight, GenRight, PostRight
- `rightkit-license` (gaps): publish 0.2.9; publish shell link and account commands; add the Apple credential helper driver; resolve lease expiry in view vs native gate; Publish 0.2.9 and bump main from =0.2.8 (K10, K11: Pro refused during a launch-time renew); Decide K8: native gate also enforces lease expiry plus grace, or the status view stops doing so; Replace license/trial.rs with request_trial_for and license/removal.rs with the release-queue API; Publish the shell account and link commands, then drop account_session.rs and license/link.rs; Check publication state: INDEX.md lists 0.2.5
- `rightkit-llm` (gaps): Move agent_providers and agent_router to rightkit-agent-llm; keep rightkit-llm as the wire clients both use; Compare MailRight's four files with the plain client and adopt or drop
- `rightkit-local-gen` (gaps): Either rename to rightkit-local-image or receive the local providers from media-gen; One local model catalogue (with remote-compute/models and rightkit-models)
- `rightkit-logs` (gaps): move random_uuid out (apps import it from logs); measure JsonlStore's open per append; publish 0.4.0; Move agent_telemetry to a rightkit-agent-* crate; Move random_uuid out (apps import it from logs); Check that 0.3.7 (main's pin) is published; INDEX.md records 0.3.5; JsonlStore opens the file per append (L1); cost not measured
- `rightkit-mail` (gaps): create rightkit-mail-engine for method table and sync/outbox/backfill over MailRemote; port tests; update plan: target is three crates
- `rightkit-mail-store` (gaps): host passed at open; store-owned input types; reverse dependency on google-apis; semantic feature; port db tests
- `rightkit-mcp` (gaps): Not read for behaviour
- `rightkit-media-control` (missing): Owner decision: new crate or extend rightkit-now-playing (design question 4); Move media_control/{backend,mac,win,worker}.rs as they are; HeardRight keeps the call at recording start and stop
- `rightkit-media-fetch` (gaps): move tier and entitlement types to host; one Kind; remove stock.rs from plan mapping; port tests
- `rightkit-media-gen` (wrong): Split by owner: provider trait + remote adapters + routing + catalogue (media-gen core); jobs on rightkit-job-queue; spend layer thinned onto rightkit-spend; voices to rightkit-tts; local providers to rightkit-local-gen; library onto rightkit-library; delete media.rs and host/h264.rs; Add a typed request/result API beside the JSON command surface; Remove organisation and app names from paths, table names and context strings (host supplies them)
- `rightkit-media-pipeline` (gaps): Move brief.rs to the shared brief-run skeleton (see rightkit-pipeline); Decide owner of production.rs against rightkit-content-plan; Rename so it is distinguishable from rightkit-pipeline
- `rightkit-memory` (wrong): delete retriever.rs; move membrane.rs to rightkit-context-client; keep MemoryBackend trait and native backend as rightkit-memory-client or a feature of it; re-export MemoryTier/MemoryEntry from memory-core instead of redeclaring; make the membrane binary locations a host-supplied list; move agent_host modules (memory_save_tool, blueprint_tools, memory_provenance, api_memright) to rightkit-agent-server; decide whether rightkit-memory becomes a facade over core+store or is renamed
- `rightkit-mesh` (gaps): Add a desktop host layer: controller, registry-backed PeerTrust, pairing session runner, ALPN dispatcher with app handlers; Make vault service and relay caps host-supplied; Move or generalise policy.rs; Decide and document the pulse-* wire names; Fix stale crate header
- `rightkit-mesh-ffi` (gaps): Take the link ALPN from the caller or the host-layer crate
- `rightkit-ml-lab` (wrong): do not publish; keep experiments (bakeoff/*, tools/zephyr_*, local-model-training) in a HeardRight research repository; extract only reusable pieces: WER scoring and ASR adapter harness to the bench package, CoreML/ONNX conversion to a model-tools package with a pinned Python manifest
- `rightkit-models` (wrong): pick the suite layout (HeardRight's live layout) as the one store and migrate Store users, or give Store a suite backend; move the trusted catalog entries to data each app or a catalog crate supplies; resolve rightkit-dictation-models vs heardright-model-store; Pick the suite layout as the one store and migrate Store users (MailRight, GenRight) or give Store a suite backend; Move the trusted catalog entries to data each app or a catalog crate supplies; Move agent_local_models to an agent crate; Resolve rightkit-dictation-models vs heardright-model-store; HeardRight M7 full-hash-on-launch; Add HeardRight's legacy_dirs and retired_legacy_paths to parakeet_ultra_spec before HeardRight drops ultra_model.rs; Decide the per-launch full hash of 363 to 433 MB (M7); stat-receipt fix is switched off; suite::whisper must install the macOS CoreML bundle; HeardRight: use suite::grant (it also parses Retry-After); Verify the download engine keeps live retry and resume rules; Bump pin 0.5.3 to 0.5.9
- `rightkit-motion` (gaps): Move cmd/ to the video-project command layer; have it lay out tiles with sheet::plan_sheets; Remove the engine-commands feature and its four optional deps; pick_encoder delegates to the single rightkit-ffmpeg policy; Decide owner of HyperFrames templates/plan (media-pipeline motion)
- `rightkit-notch-ui` (wrong): Make cells and marks host-defined; Keep AI rings in Pulse or an AI-accounts UI crate
- `rightkit-notifications` (gaps): Confirm a sound-only alert call exists
- `rightkit-onnx` (gaps): Fold the two ort_common crates into the package; HeardRight O5; Settle CoreML error_on_failure (O5) and take or drop the DirectML bisect switches so HeardRight can use SessionOptions; HeardRight: delete the thread-budget copy and call RightKit; HeardRight: use installed_runtime_candidate for both candidate lists; Check the ort rc.13 to rc.15 binding bump against the bundled runtime (O1); Bump pin 0.2.9 to 0.2.10
- `rightkit-overlay` (gaps): Add Voice{level} and Success to OverlayEvent (O2); foreground_fullscreen_on must require a borderless window or it hides the pill under a maximised window on a taskbar-less monitor (O5); Borderless check: 2 px tolerance, DWM frame bounds, shell-class exclusion, same-monitor rule (O6); align shell class list (O7); Visibility must allow the always-show setting and full-screen rule (O4); Starting mode mapping (O3); Decide an owner for dispatch queue, visibility guardian and temporary-event timer (O8, O9); Publish 0.2.2; main pins 0.2.0
- `rightkit-overlay-win` (gaps): Publish 0.1.3+ (set_focusable, key events) and bump the pin from 0.1.0 to restore clip-card editing (W2); Show visual and interaction parity of the native pill by a run; Work-area clamp default would move the pill; main turns it off (W4); No drag support (W3, probably no loss)
- `rightkit-owner-approvals` (wrong): Follow the rightkit-approvals actions in matrix-rows-agent-platform.json: one core (request, states, single-use claim, store trait, proof trait); this crate becomes the signed proof or a `-signed` sub-crate; Move redact.rs and audit.rs to their owners; Do not publish further versions under this name until that decision is made
- `rightkit-passkeys` (gaps): Find a first consumer before it is called mandatory; not verified
- `rightkit-pdf` (gaps): Add an optional PDFium render feature with a bundle-restricted loader, or record that page rendering stays in ScrapeRight; ViewRight moves its pin to the current version
- `rightkit-pipeline` (wrong): Redraw as engine-to-engine calling: (a) owned engine call with PostRight's attempt/sweep discipline and CutRight's contract/capability handshake, (b) generate client (submit, wait, import, journal) named for the contract, (c) brief dispatcher, (d) shared `brief run` skeleton (CLI, envelope validation and writing) with per-engine deliverable producers; Move channel_recipe to the video side; add brief-run caller runner and executor harness; rename or add genright module; move channel_recipe out; port orchestration tests and fake engine
- `rightkit-platform-rules` (gaps): Add video rules (aspect, duration, title/description/tag lengths, safe zones) and TikTok so the video crates can drop their compiled limits; State which file is the source for a limit: the bundled seed or social-publish's overrides; Fix the README status; publish the version PostRight is waiting for
- `rightkit-playback` (gaps): Do not extend this crate with timeline preview; create the preview-session crate (see rightkit-compose) with the Tauri commands behind a feature, as this crate does with tauri_commands; Publish as is for ScrapeRight
- `rightkit-policy-rules` (gaps): Decide the relation to rightkit-agent-policy: one evaluator, or a stated split (rule packs here, command classification there); Rename the port folders by what they do and remove comments about Legion crates; Resolve the invocation-proof duplicate the comment describes
- `rightkit-privileged-helper` (gaps): Add Swift helper module with parameterised identifiers and TrashPolicy
- `rightkit-prompt-recipes` (wrong): Keep contracts + validator + recipe composer + video templates as a pure library over data; Brands become host-supplied data; KDP moves to the GenRight app (or a recipes data pack); Command handlers move to the engine layer
- `rightkit-pty` (gaps): Port remote-gateway terminals to rightkit-pty or state why not
- `rightkit-qa` (gaps): Add a PTY/terminal driver or rule that TUI acceptance stays CodeRight's; Consider splitting media assertions from the control harness
- `rightkit-qa-driver` (wrong): Remove from the mandatory set; publish no new versions
- `rightkit-record` (gaps): Add still-frame window capture; Re-map the rest
- `rightkit-reframe` (gaps): Single copy of track.rs (here); Rename schema ids; Decide whether approval is mandatory at library level
- `rightkit-remote-compute` (wrong): Move models/ to rightkit-local-gen (or rightkit-models for catalogue/install); Worker registry behind a small store trait; Spend bracket on rightkit-spend
- `rightkit-remote-gateway` (gaps): Rebase on the gateway core and approvals core; Split agent_secrets into its own crate; Remove Pulse and ChatGPT names from library text
- `rightkit-repo-graph` (gaps): drive skeletonization from the graph's language registry and grammars crate; put embed/onnx behind a feature; state in README whether repo-graph is usable without the membrane resident
- `rightkit-research` (gaps): remap PostRight digest to rightkit-content-plan; decide whether reddit_reader becomes a source in rightkit-research::source or stays in media-fetch; remove the code-audit-provider-sdk dependency; remap PostRight files; move to rightkit-http
- `rightkit-review` (gaps): render_qa takes a file + a spec, running ffmpeg through rightkit-ffmpeg directly; drop the video-project/timeline deps; Move package.rs to the publishing side (social-publish or video-project exports) and read limits from rightkit-platform-rules; Rename cutright.* schema ids (no live users); Consider splitting code/minimize (Legion) into rightkit-code-review; they share nothing with media review
- `rightkit-sandbox` (gaps): Move it
- `rightkit-scheduler` (gaps): Move execution, delivery, lifecycle, persistence here behind a runner trait; delete the copies
- `rightkit-search` (best): lift resumable rebuild-and-swap into rightkit-search::fts
- `rightkit-send` (gaps): Build the Swift engine or FFI; Extract the generic part of the mesh Send route
- `rightkit-shorts` (gaps): Model id from caller or rightkit-llm default; Move moments search to the project command layer; Rename cutright.shorts.proposals.v1
- `rightkit-sidecar` (best): A 0.1.213 engine and a main shell cannot talk; check the updater never leaves a stale engine binary (P5); Bump pin 0.3.0 to 0.3.2
- `rightkit-social-analytics` (wrong): move cmd/retention.rs to video/autoedit family; shared social core types crate; README; tests
- `rightkit-social-inbox` (gaps): shared types crate; README; tests
- `rightkit-social-publish` (gaps): reference SQLite store or documented schema; opaque or generic scope key; move calendar_time to rightkit-time; README; port tests
- `rightkit-spend` (gaps): Add non-final (partial) spend reports and a per-reservation expiry to the approved-budget API (Pulse report final_report, issue ttl_seconds); Re-map cutright-ledger out of spend (decision ledger/edit-plan to video-project or a new rightkit-decisions; palette to a UI/command package); Reduce rightkit-media-gen ledger.rs to a thin layer over reserve_multi / set_device_allowance / reconcile_invoice / Budget; Apps pin =0.2.0; publish 0.2.3 and move the three adopters; Add an optional currency to Allowance; Make Pulse use approved.rs seals or remove them from the crate; Re-map CutRight's ledger to a review/evidence package
- `rightkit-sync` (gaps): Add the shared scheduler both apps hand-roll; Consider owning the engine-to-shell CloudKit transport seam; HeardRight: move the scheduler onto rightkit_sync::schedule (next_delay, sync_once) or state why wake coalescing is app-specific; Compare rightkit-cloudkit-helper save policy and system fields with the live helper (S1); Ledger migration is one-way; downgrade resumes stale legacy ledgers (S3); 0.2.3 removed MemoryBackend and the fake helper; main pins 0.2.2 (S8); Google provider declared in settings but not implemented (rightkit#162)
- `rightkit-sync-keys` (gaps): First consumer before mandatory; not verified
- `rightkit-sysmem` (gaps): Make the six RightKit crates call it, or delete it and let rightkit-system-monitor own memory reads; Move window_monitor_profile to a display or colour crate, or rename the crate to what it is (platform FFI leaf); Add available memory alongside total, since every other copy reads both [suspicion from the governor and sensors call sites]
- `rightkit-system-monitor` (gaps): Single snapshot owner; Add Windows sensors, volumes, health; Re-map usage.rs to ai-accounts/spend
- `rightkit-tauri-shell` (gaps): add a custom tray left-click action; state core vs optional host modules; gate or split viewer and overlay hosts; settle one IPC allowlist model against CodeRight ipc_policy.rs; publish 0.7.x; Add a custom tray left-click action so the app stops reaching into the raw icon (H5); Publish 0.7.3 (main pins 0.7.0); publish the account and link license host commands; Run the single-instance and autostart migrations against a real 0.1.213 install (H1, H3 read, not run); Verify close-handler ordering between app handler and plugin (H8); Window builder is Wry-only so main downcasts the app handle (H11); Tray icon field named icon_png receives .ico bytes (H7)
- `rightkit-terminal-ui` (gaps): Decide whether the session-facing TUI host belongs here behind a trait or stays in agent-server; document the seam
- `rightkit-text-polish` (best): Bump pin 0.1.0 to 0.1.1 (test removal)
- `rightkit-theme` (best): publish with a parity test against @rightkit/theme
- `rightkit-timeline` (gaps): Share Ticks/Fps with compose (one small time module, either crate re-exports); Use rightkit-ffmpeg Encoder; Presets as data (name, size, safe zone id) resolved against rightkit-platform-rules ids
- `rightkit-transcript` (best): Bump pin 0.4.0 to 0.4.2
- `rightkit-transcript-library` (gaps): add feature-gated SQLCipher store, recovery, export with key source and paths as parameters; move non-record delivery logic to rightkit-dictation-*; port history tests; fix plan mapping; Fold the duplicated pure delivery rules with rightkit-automation
- `rightkit-tts` (best): Decide owner of the voice library (recommended: a `voices` module here, since it needs no provider code) and drop the qts facade from media-gen
- `rightkit-typeset` (best): Give apps one source for the fork so CutRight deletes its copy
- `rightkit-vad` (best): Library default is Warmup::ResetAfter; HeardRight must keep KeepState set (V1)
- `rightkit-vault` (gaps): remap Pulse agent_secrets to rightkit-remote-gateway; build and test the crate before publication (README states it never compiled); depends on entitlement endpoints that must exist first
- `rightkit-video-project` (gaps): Delete reframe_track.rs here; Pick one rule for engine commands (recommended: a `cmd` module per domain crate behind `engine-commands`, engine-host stays free of video deps) and port media/bundle/project/outputs/storage/slot commands under it; Rename ids and env prefixes to an app-supplied prefix; Give decision ledger + edit plan + relink a home (here as modules, or rightkit-decisions)
- `rightkit-voice-commands` (best): Owner decision on V1: non-desktop platforms now report commands supported; Bump pin 0.1.0 to 0.1.1
- `rightkit-web-extract` (best): port tests; decide home for iOS extractors
- `rightkit-web-session` (gaps): remove Pulse names and paths; remap ScrapeRight sign-in to rightkit-browser/rightkit-tauri-shell; tests
- `rightkit-win-ui` (gaps): Choose one canvas (overlay-win is the reference by app priority) and port notch-ui and the clipboard strip to it
- `rightkit-workflow` (gaps): Decide whether CodeRight workflows run on this state machine; move or delete the agent-server copy
- `RightKitAgent` (missing): Decide first: Swift module wrapping rightkit-agent-engine-mobile-runtime over FFI (CodeRight) with Pulse's approval gate and budget ledger as the Swift-side policy; then write it; Re-map screens to SHELL
- `RightKitAppleCredentials` (gaps): HeardRight: shrink the Mac helper to service name, access group, account allowlist and a call to AppleCredentialHelperHost; Check how much of the iOS facade is thin
- `RightKitAudio` (gaps): Add in-memory sample accumulation, speech-activity snapshot and a WAV writer to WarmAudioCapture; Add a session policy layer if RecordingAudioPolicy is to move (mix policies, retry ladder, Bluetooth, warm hold); Move the iOS pin to a published version containing 0.9.0 modules; HeardRight: use AudioFileRecorder on the watch
- `RightKitCloudSync` (gaps): HeardRight: replace CloudSyncNotifications.swift with CloudKitWakeHost, keeping identifiers and FFI entry points
- `RightKitDiagnostics` (gaps): DiagnosticEvent must accept non-string field values; Cover the DEBUG campaign file kept outside Caches; Add a MetricKit subscriber; HeardRight: replace DogfoodTraceStore with JSONLDiagnosticStore
- `RightKitDictation` (missing): Create the module: command plane and snapshot first, then coordinator and transcriber, then audio session and policy; Decide whether a RightKitDictationUI view target is wanted; Decide whether a watchOS slice here is the watch-recording home
- `RightKitHTTP` (gaps): Add an SSE frame parser over the existing `stream` transport (HTTPTransport.swift:39), taking CodeRight's parser as the reference; Remap DaemonClient and CallbackURLHandler in the plan
- `RightKitHub` (gaps): Add RightKitPresentation to RightKitSwift (or decide it belongs to the agent UI family) so Pulse can drop the local package; Reconcile AttachedEngine.swift with Pulse's copy and say which side is current; Pulse then replaces `path: ../../swift/RightKitHub` with the RightKitSwift products; Split the plan row into Hub, EngineClient, Engine and Presentation
- `RightKitHubQA` (wrong): Correct the mapping so the iOS tests stay with the app
- `RightKitIntents` (gaps): HeardRight: use RightKitRecordingBridge.dispatch; Share the recording intent bodies only if the owner wants; AppIntent declarations stay in the app
- `RightKitLibrary` (missing): create transcript library module from HeardRight on RightKitStore/RightKitCloudSync; remap settings views; Create the module: store and library first, then the cloud adapter, then the history view (RightKitLibraryUI); Verify the shared data model with ScrapeRight
- `RightKitLicense` (gaps): Tag a version containing ContinuityBridgeClient and move the iOS pin from 0.8.1; HeardRight: delete its controller, removal queue, link-code call and bridge client for the RightKit owners
- `RightKitLiveActivity` (best): Owner decision whether Live Activity views move to a view target or stay app screens
- `RightKitMedia` (missing): Decide whether the macOS worker is still CutRight's render path; if superseded mark DROP, otherwise create the module from AssetService/AudioService/VisionService with the wire Protocol.swift
- `RightKitMeshLink` (gaps): State why it is standalone (binary dependency); Add the Kotlin wrapper to rightkit-android
- `RightKitNotch` (missing): Create with generic window, geometry, card container and motion only
- `RightKitOverlay` (gaps): Add an audio level path and a smoothing driver; Add a done outcome or rule that done is status("Done"); Add hover enter/leave state and a SwiftUI hosting view; Add anchored resize and generation-guarded frame animation; Add a logging seam; decide the C interface and whether pill content moves in; Decide the owner of splash and transitions
- `RightKitPurchases` (best): HeardRight: replace ProAccessStore with AccessCache
- `RightKitSpeech` (gaps): Add ANE cache maintenance and SharedModelPublisher; Add the Files-import install path (4 GB inflate cap) and a download telemetry hook; Decide a home for keyword spotting over the Rust core (RustKWSBridge, KeywordCommandDetector); HeardRight: delete ModelGrantClient and both CoreMLModelValidator copies, then the download and install copies
- `RightKitStore` (best): iOS pin is 0.8.1 while the Mac pill pins 0.9.0; move to one published pin
- `RightKitSwift` (gaps): Release 0.9.1 (tree is ahead of every app's pin); Create or reassign the six missing modules before any app is told to move code: Agent and Dictation first, they carry the most lines; Remove app tests from the RightKitHubQA and RightKitWatch targets in the plan; app tests stay with the app; Add RightKitPresentation; bring RightKitMeshLink under the same version; One repository URL in every app manifest
- `RightKitSystemUI` (missing): Create section by section after the React package's client contract is settled
- `RightKitUI` (wrong): re-map; generate Swift tokens from rightkit-theme instead of hand-written contrast; Add onboarding and settings scaffolds only if a second app will use them; otherwise they are app screens; Make the mapping document and inventory JSON agree
- `RightKitUpdate` (gaps): Port Pulse's journalled swap, install lock and finishPendingSwap into MacAppInstaller before Pulse adopts it; Keep verifier identifiers as parameters (already so); Check the manifest and tier names against rightkit-app-update
- `RightKitWatch` (wrong): Correct the mapping so no file is planned into RightKitWatch; Choose a watch-recording home that is not this module
- `RightKitWatchBridge` (gaps): Decide the watch-recording home: extend this module with the two WCSessionDelegate halves or add a watchOS slice to RightKitDictation; Add a recovery store
