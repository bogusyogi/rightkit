# Changelog

## Unreleased

- Fix: `wrap_untrusted_web_text` fences web text with the protocol's configured untrusted-context tag (`rightkit_agent_engine_protocol::untrusted_context_tag()`, default `rightkit-untrusted-context`); it was a hard-coded `coderight-untrusted-context`. Changes the fence element name for hosts that do not set a tag (from `coderight-untrusted-context` to `rightkit-untrusted-context`).

## 0.1.0 (unpublished)

- New crate. The coding agent's host tools moved here from `rightkit-agent-tools` 0.3.5, where they were the `coding_tools` module behind the `coding_tools` feature. They never referenced the rest of that crate, which was left with the tool contract, gate and backends every app uses, and no longer depends on `rightkit-agent-config` or `rightkit-agent-telemetry` (2026-11-12 shape review).
- Paths: `rightkit_agent_tools::coding_tools::X` is now `rightkit_agent_coding_tools::X`; the module tree under it is unchanged (for example `rightkit_agent_coding_tools::shell::NativeCommandSandboxAdapter`).
- Features: `coding_tools-host` is now `host`, `coding_tools-code-mode` is `code-mode`, `coding_tools-embeddings` is `embeddings`, `coding_tools-test-support` is `test-support`. There is no `coding_tools` feature; the whole crate is the tools. The browser, OCR, media, plugin and PTY dependencies stay behind `host`.
- No behaviour change.
