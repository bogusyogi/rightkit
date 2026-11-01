# Changelog

## 0.1.1

- Synced with CodeRight: a damaged state or notification file is set aside as `<name>.damaged-<unix seconds>` before the default is used, so a default is never saved over the only copy; a trusted-source refusal returns the authority's own status instead of a blanket 401.

## 0.1.0 (unreleased)

- First RightKit release, moved from `coderight-attention` (engine).
