# rightkit-social-analytics

Social campaign analytics without a store: tracked short links (public-https destination checks, UTM tagging, deterministic link ids, redirect-route rules), platform insight pulls and the metric registry, manual metric CSV import and calendar change proposals, over host-implemented stores.

| Module | Contents |
|---|---|
| `links` | Tracked short links: public-https destination checks, UTM tagging with defaults, deterministic link ids, the redirect route's id, target and referrer rules, and the campaign and link commands over a host-implemented `LinksStore`. |
| `analytics` | Platform insights pull, observation reads, lineage, and the metric registry and gates over a host-implemented `AnalyticsStore` (which also makes the one HTTP GET). |
| `metric_import` | Manual CSV metric import (preview, commit, list) over a host-implemented `MetricImportStore`. |
| `optimise` | Calendar change proposals (the `remake` rule) over a host-implemented `OptimiseStore`. |
| `protocol` | The links, analytics, metric-import and optimise payload shapes. |

The tables and the pull schedule belong to the host app. The crate has no video dependency: turning
an audience-retention export into timeline cuts is video editing and lives in `rightkit-autoedit`
(`cmd::retention`, feature `engine-commands`).

Moved into RightKit from PostRight (`postright-engine` links, analytics, metric import and optimise;
`postright-protocol` `UtmPolicy`) on 2026-11-01.
