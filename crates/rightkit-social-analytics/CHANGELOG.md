# Changelog

## 0.1.1 (unreleased)

- New, additive (feature `engine-commands`, off by default): `cmd::retention`, the `retention.import`, `retention.list`, `retention.proposals` and `retention.apply` engine handlers, moved from CutRight's `cutright-engine`: an audience-retention CSV is bound to one exact render, drop-offs become reviewable edit proposals, and apply writes one canonical timeline revision. On-disk schema ids (`cutright.retention.v1`, `cutright.render-request.v1`) are kept.

## 0.1.0 (unreleased)

- First RightKit release, moved from PostRight (postright-engine links.rs pure link rules; postright-protocol UtmPolicy).
