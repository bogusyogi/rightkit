# Changelog

## 0.2.0

- New `cli` feature (default off) and `provider::cli`: `CliProvider` runs an already-installed, already-signed-in assistant CLI (Claude Code `claude -p`, Codex `codex exec`) as a bounded child process through `rightkit-process`, so a daemon can draft text with no API key stored by the app. `CliSpec` describes the tool as data (absolute program path, fixed argument template, stdin or `--`-terminated prompt, text or JSON output with JSON-pointer usage extraction, environment allow-list, not-signed-in patterns); `CliSpec::claude_code` and `CliSpec::codex_exec` are presets. Bounded by wall-clock timeout, idle timeout and stdout/stderr byte caps; any bound kills the process tree. Typed `CliError` surfaces as `LlmError::Cli`.
- `provider` is now a directory module (`provider/mod.rs`); the public path `rightkit_llm::provider::Provider` is unchanged.
- `ChatResponse` gains `usage_known: bool` and `metering: Metering` (`PerToken` or `Subscription`). The HTTP providers set `PerToken`; `usage_known` is true when the response carried usage. This is a breaking change for code that builds `ChatResponse` by struct literal.
- `ToolSpec` gains `strict: bool` (default false) with `ToolSpec::new(..)` and `.strict(true)`. When set, the Anthropic builder sends `"strict": true` on the tool and the OpenAI-compatible builder on the function; when unset the request body is byte-identical to before. Breaking for code that builds `ToolSpec` by struct literal.
- Re-exports `rightkit_http` as `rightkit_llm::http`, plus `Client`, `ClientConfig`, `ProxyMode`, `RetryPolicy`, `AddressPolicy` and `HttpError` at the crate root, so hosts configure the transport without a direct `rightkit-http` dependency.
- `PriceTable::cost_of(&ChatResponse)` returns `None` for subscription-metered or unknown-usage responses.

## 0.1.0

- First release. Brand-neutral BYOK LLM clients: Anthropic Messages and OpenAI-compatible chat providers, streaming, tool use and forced-tool structured output, retryable-only route failover with a first-token timeout and circuit breaker, bundled model price table, and prompt-cache breakpoint discipline.
