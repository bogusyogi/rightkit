# Changelog

## 0.1.2 (unpublished)

- Unit tests and debug-assertion switches removed (Adrian, 2026-11-03: end-to-end tests only; no debug assertions). Release behaviour unchanged.

## 0.1.1 (unreleased)

- Added `analytics`: `analytics.pull` (and `pull_with`), `list`, `latest`, `summary`, `lineage_put`, `registry`, `gate` and the metric registry over a host-implemented `AnalyticsStore`, whose `http_get` is the HTTP seam. Additive.
- Added `protocol` (analytics, metric-import and optimise payload shapes) `metric_import` over a host-implemented `MetricImportStore` and `optimise` over an `OptimiseStore`. Additive.
- Added `links::LinksStore` and the campaign and link commands (`campaign.put`, `campaign.list`, `link.create`, `link.get`, `link.list`, `link.record_visit`, `link.attribution`); `UtmPolicy::validate`; link payload shapes in `protocol`. Additive.

## 0.1.0 (unreleased)

- First RightKit release, moved from PostRight (postright-engine links.rs pure link rules; postright-protocol UtmPolicy).
