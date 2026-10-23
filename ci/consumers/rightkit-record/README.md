# rightkit-record 0.3.3 registry consumer (draft, PK17)

Standalone consumer, outside the workspace, pinning `rightkit-record =0.3.3` exactly. It exercises the
pure parts of the record core: `plan::valid_id`, the start-request refusals in `plan::validate` (fps,
cursor pixels, window id, empty surface list), the content hashes in `hash` (`blake3_text`,
`is_blake3_text`, `file_hash`, `keyed_digest`), the stable `error::Code` strings, and `bundle_verify`
on a directory with no manifest and on a path that is not a directory.

State: files only under a fresh temp directory, removed at the end. No capture, no helper binary, no
Screen Recording permission, no network, no real user data. Run only by the CI consumer lane.

Not covered: a complete bundle that verifies, the `Recorder` take lifecycle, any `Backend`, the
ScreenCaptureKit and Windows helpers, redaction keys, cursor assets, media recovery, `BundleReader`,
and the success path of `plan::validate` (it needs a constructed `Surface` from `rightkit-contracts`,
which this recipe does not depend on). This recipe must be revisited if the family list changes.
