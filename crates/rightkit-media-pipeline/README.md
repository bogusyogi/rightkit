# rightkit-media-pipeline

Named production pipelines on top of `rightkit-media-gen`: editable, persistable step DAGs (explainer,
product video, dubbing with retiming, translation with terminology, narration-clock presenter, batch
variations) with fan-out multi-job steps, native subtitle and compose steps, an optional finishing
hand-off to an external timeline engine, and the deterministic campaign production stack.

Moved into RightKit from `genright-studio` (`pipeline_workflows.rs`, `pipeline_workflows/*`,
`production.rs`, `data/production/templates.json`) on 2026-11-01.

`handle(ctx, name, input)` dispatches the `studio.pipelines.*` and `studio.production.*` command
names. The finishing engine is found through `pipeline_workflows::finish::set_engine_resolver`
(or an explicit `engine_path` input). Wire and on-disk ids are kept unchanged for compatibility:
the `cutright_finish` step and command names, `cutright_*` error codes and the
`genright_campaign_stack` pack source id.
