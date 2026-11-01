import type { ConductorModelOption } from "./conductorOptions";

/**
 * Claude's model catalog for the composer's per-conductor model submenu.
 *
 * This list is static because Claude Code has no local model-list command —
 * unlike Codex, whose list is never hardcoded and instead arrives at runtime
 * via the daemon's app-server `model/list` call (see `modelsFor` in
 * conductorOptions.ts, which reads it from the caller-supplied
 * `modelsByKind.codex` rather than from a file like this one).
 *
 * Source: https://platform.claude.com/docs/en/about-claude/models/overview
 * (fetched 2026-08-03). Ids below are exact.
 *
 * Deliberately excluded:
 *   - claude-opus-4-1-20250805 (Claude Opus 4.1): deprecated, retires 2026-08-05.
 *   - Claude Mythos 5: invitation-only, not generally available.
 */
export const CLAUDE_CURRENT_MODELS: ConductorModelOption[] = [
  { id: "claude-fable-5", label: "Claude Fable 5" },
  { id: "claude-opus-5", label: "Claude Opus 5", isDefault: true },
  { id: "claude-sonnet-5", label: "Claude Sonnet 5" },
  { id: "claude-haiku-4-5-20251001", label: "Claude Haiku 4.5" },
];

/** Legacy — still available, but superseded by the current generation above. */
export const CLAUDE_LEGACY_MODELS: ConductorModelOption[] = [
  { id: "claude-opus-4-8", label: "Claude Opus 4.8", description: "Legacy" },
  { id: "claude-opus-4-7", label: "Claude Opus 4.7", description: "Legacy" },
  { id: "claude-opus-4-6", label: "Claude Opus 4.6", description: "Legacy" },
  { id: "claude-sonnet-4-6", label: "Claude Sonnet 4.6", description: "Legacy" },
  { id: "claude-sonnet-4-5-20250929", label: "Claude Sonnet 4.5", description: "Legacy" },
  { id: "claude-opus-4-5-20251101", label: "Claude Opus 4.5", description: "Legacy" },
];

export const CLAUDE_MODELS: ConductorModelOption[] = [
  ...CLAUDE_CURRENT_MODELS,
  ...CLAUDE_LEGACY_MODELS,
];
