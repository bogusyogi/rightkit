# rightkit-library

The owned media library's artifact index: classify files, index gallery roots with their sidecars
(human-eyes and jury verdicts, caller tags, asset facts), filter (project, brand, eyes, jury, caller)
and page the result, and infer brand codes from a catalogue the host registers with
`artifacts::set_brand_catalog`.

Moved into RightKit from GenRight (`genright-core` `artifacts.rs`, `path_policy.rs`) on 2026-11-01.
The rest of the library capability (import with provenance, asset facts, the job store, FTS search)
moved with `rightkit-media-gen`, which writes the sidecars this index reads. The brand catalogue and
GenRight's `/stunning` special case stay in the app.
