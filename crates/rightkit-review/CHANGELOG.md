# Changelog

## 0.1.0

- First published release. Shared review capability for the content pipeline: typed deterministic checks over a media or brief description (`RuleSet::standard`), declared holds and windows, disposition rules, a hard-capped repair loop (`run_repair_loop`, cap never above 5), an advisory `Judge` interface that cannot override deterministic failures, and versioned `VerdictRecord`s.
