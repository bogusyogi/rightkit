# Changelog

## 0.1.6

Additive: no public item is removed and no signature changes. Stage 3 of moving CodeRight's terminal UI crate onto this one (bogusyogi/coderight#567): the security and leaf modules. Several functions draw or decide differently; each is listed.

- **`render::sanitize::sanitize` strips invisible spoofing characters.** Besides control characters it now removes the bidi controls (U+061C, U+200E, U+200F, U+202A to U+202E, U+2066 to U+2069), zero-width space (U+200B), word joiner (U+2060), BOM (U+FEFF), soft hyphen (U+00AD) and the tag characters (U+E0000 to U+E007F). ZWNJ and ZWJ (U+200C, U+200D) stay only right beside a non-ASCII character that is not another joiner and not general punctuation (U+2000 to U+206F); between ASCII characters they are removed. Tag characters stay only after the subdivision-flag emoji U+1F3F4. Every constructor that routes text through `sanitize` (transcript messages, tool-call and hint text, exec ledger, identity strip, elicitation form, cell text) gets this.
- **`render::allow_rule` offers fewer "always allow" prefixes.** The never-prefix list gains `busybox`, `toybox`, `stdbuf`, `setsid`, `chroot`, `strace`, `ltrace`, `wsl`, `pkexec`, `runuser`, `unshare`, `nsenter`, `ionice` and `taskset`. The check also covers Windows spellings (`.exe`, `.cmd`, `.bat`, `.com`) and versioned names (`python3.12`, `node18`), and the subcommand tools (`git.exe status`) are matched without the extension. The host's own command (`app_slug()`) still takes `command subcommand` prefixes.
- **`render::exec`: `ExecCall.permission` and `ExecCell::set_permission`.** A call can carry how its permission settled; the ledger row shows it as a dim field before the result and time (red for `denied`, and a denied call no longer also says `failed`). `ExecCall` already had a private field, so adding a public one breaks no struct literal.
- **`render::diff::ledger_diff_row` draws `−` (U+2212) for a removal and a 3-wide line number**, so the row body starts at column 4 instead of 6.
- **`terminal::capability`:** new `clipboard_osc52_forced` (`<PREFIX>_CLIPBOARD_OSC52`, taken through the naming hook) and `osc52_screen_sequence`; `clipboard_strategy` treats the forced flag like an SSH session; `clipboard_osc52_for_env` wraps for GNU screen with its own DCS rather than tmux's; `detect_image_protocol` also accepts `TERM=*kitty*`; `rgb_to_ansi16` maps neutrals by lightness and other colours by nearest hue (the old bit-threshold mapping chose other colours on 16-colour terminals).
- **Synchronized output (DEC mode 2026).** `terminal::custom_terminal::Terminal::draw` holds each non-empty frame, and `terminal::history::insert` each scrollback insert, between `ESC [ ? 2026 h` and `ESC [ ? 2026 l`. A terminal that does not know the mode ignores both.
- **`pty_attach`:** Ctrl+5 detaches like Ctrl+], because a terminal without the keyboard-enhancement protocol reports the 0x1d byte as Ctrl+5.
- **`rightkit-theme` dependency** (0.1.1, serde and serde_json only, both already in this crate's tree): `transcript::color::perceptual_distance` and the contrast ratio in `transcript::style` use `rightkit_theme::color::{delta_e76, contrast_ratio}` instead of local copies. Results move by a small numeric margin (the local copies used rounded matrix constants and an f32 path); the white-point, transfer curve and weights are the same.
- Not in this version, left for the later stages: `terminal::session`, `alternate_screen`, `link_pointer` and `keyboard_modes` carry only unused CodeRight code, and `rightkit-http`'s `percent_decode_strict` is not adopted (the local strict decoder in `markdown_render::local_links` does the same and rightkit-http brings an HTTP and TLS client).

## 0.1.5

Additive; no public item is removed or changed. First two stages of moving CodeRight's terminal UI crate onto this one (bogusyogi/rightkit#1781).

- **Naming hook.** The environment variables and file names the crate used to hard-code now take the host's prefix: `terminal::notify::set_env_prefix` (call once at startup, before the first read), `env_prefix`, `env_name` and `app_slug`, next to `set_app_name`. Same shape as `rightkit_agent_config::env_name`, which this crate does not depend on (it would bring rightkit-agent-policy, rightkit-http, toml and serde into a UI crate); a host sets the same prefix on both. Routed through it: `<PREFIX>_NOTIFY`, `<PREFIX>_TERMINAL_TITLE`, `<PREFIX>_ASCII`, `<PREFIX>_UNICODE`, `<PREFIX>_TUI_DISABLE_KEYBOARD_ENHANCEMENT`, the external-editor temp file (`<slug>-prompt-<pid>-<ns>.md`) and its shell `$0` (`<slug>-editor`), and the host's own command in the allow-rule subcommand list (`<slug> <subcommand>`; `rightkit` stays in the list). With nothing set every name is what it was.
- **Added modules.** `render::cast` (the cast tree of the main agent and its sub-agents with their models), `transcript::pacer` (adaptive line pacing of a streamed answer), `terminal::kitty_image` (Kitty graphics output for a PNG), mermaid `pie` charts (a ` ```mermaid ` block starting `pie` now renders as bars, where it fell back to source), and the crate-private `composer::clipboard_copy` result types.
- `NOTICE` (Codex, Pi and Goose donor attribution) is in the published package.

## 0.1.4

- Publish the dependency requirements main already declares: `rightkit-fs` 0.5.0, where the published 0.1.3 requires 0.4.9. The 0.1.3 manifest on crates.io stays as published; this version carries the new requirements.

## 0.1.3 (2026-11-16)

- Fix: a long answer written into the terminal's scrollback now wraps every row to the terminal width. A link longer than the terminal (a bare URL that carries an OSC 8 link, or a linked label) was written as one over-wide row that the terminal soft-wrapped on its own, so the row count the scrollback insert had counted was wrong and lines were pushed out of place. A list nested so deep that its continuation indent took half the terminal or more left a column or two for text and put one character on each row; such a line now continues from the left margin. Plain bare URLs without a link still stay whole so the terminal can match them as one link. Ported from the product TUI (CodeRight, Wave 25) where the fix was made first.
- Removed dead source files that no module declared and nothing read: `src/composer/wrapping.rs`, `src/composer/line_truncation.rs` and `src/composer/line_utils.rs` (the composer re-exports the transcript's wrapping and truncation and the terminal's line helpers), and the 97 `.snap` snapshot files under `src/` (there is no snapshot test target). In the transcript wrapping helpers, `adaptive_wrap_line_to_width` replaces the now unused crate-internal `adaptive_wrap_line`. No other behaviour changes.

## 0.1.2 (unreleased)

- Removed test-only code: `ApprovalModal::arm_for_tests` (Adrian, 2026-11-03: "make sure no debug assertions in their respective apps/ repos"; "My decision was no unit tests. Only e2e."; "dev mode, is mine. im the fucking owner, my data. no dummy data ... real data. snapshot and restore."). Release behaviour unchanged.

## 0.1.1 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-tui` (engine).
- Synced with the product TUI: `ApprovalOverlay::pending_requests` lists the open and queued approvals; `auto_approval:` requests no longer offer an allow rule; the syntax-theme root is called `theme_home` (no `CODEX_HOME`); the keyboard-enhancement opt-out variable is `RIGHTKIT_TUI_DISABLE_KEYBOARD_ENHANCEMENT`; the composer reuses the transcript's wrapping and truncation helpers and the terminal's line helpers instead of private copies.
