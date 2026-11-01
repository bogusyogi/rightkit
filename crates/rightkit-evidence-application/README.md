# rightkit-evidence-application

In-process evidence application: composes the audit engine, orchestration runtime and report renderers behind host-supplied catalog (`CapabilityCatalog`), report and run sources. The host declares its default agent with `set_application_identity`.

Moved into RightKit from `legion-application` (Legion) on 2026-11-01; the installed-product (release binding) composition stays in the host.
