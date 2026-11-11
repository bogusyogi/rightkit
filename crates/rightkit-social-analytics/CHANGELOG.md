# Changelog

## 0.2.0 (unpublished)

Taken on the recommended option of the shape review (`docs/audits/2026-11-11-shape-matrix/reports/cut-post-redraw.md`), pending the owner's confirmation.

- Breaking: the `engine-commands` feature and its `cmd::retention` module (CutRight's retention-to-timeline-cuts commands `retention.import`, `retention.list`, `retention.proposals`, `retention.apply`) are gone. They are video editing and moved, unchanged, to `rightkit_autoedit::cmd::retention` (`rightkit-autoedit` 0.2.0, feature `engine-commands`). The crate no longer depends on `rightkit-engine-host`, `rightkit-video-project`, `rightkit-timeline`, `rightkit-transcript` or `rightkit-autoedit`.
- README and description name everything the crate holds.

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Added `analytics`: `analytics.pull` (and `pull_with`), `list`, `latest`, `summary`, `lineage_put`, `registry`, `gate` and the metric registry over a host-implemented `AnalyticsStore`, whose `http_get` is the HTTP seam. Additive.
- Added `protocol` (analytics, metric-import and optimise payload shapes) `metric_import` over a host-implemented `MetricImportStore` and `optimise` over an `OptimiseStore`. Additive.
- Added `links::LinksStore` and the campaign and link commands (`campaign.put`, `campaign.list`, `link.create`, `link.get`, `link.list`, `link.record_visit`, `link.attribution`); `UtmPolicy::validate`; link payload shapes in `protocol`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from PostRight (postright-engine links.rs pure link rules; postright-protocol UtmPolicy).
