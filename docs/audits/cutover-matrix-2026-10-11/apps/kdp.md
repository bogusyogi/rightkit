# KDP formatter RightKit cutover matrix

Reviewed source HEAD `0cc72e7ae028bdb4cf53690814ed1e033a1b96e6` at `/Volumes/D/claude/workflows/kdp`; targets: Node & Python command workflows.

Counts: **Y 0 · N 0 · M 0 · NA 97** across all 97 catalog components.

KDP’s active formatter & cover CLIs create print artifacts through Playwright `page.pdf`, Pillow, ReportLab & pypdf. Its coloring path imports workspace Node image-provider adapters; cover review imports workspace `runAutoJury`. These routes do not invoke RightKit.

No Cargo, SwiftPM, Xcode, React-reader, Tauri, desktop-shell, release, settings, license, updater, QA-harness or Rust/Swift bridge target exists. `rightkit-pdf` & `@rightkit/pdf-ui` remain NA: print-PDF creation is neither RightKit Rust PDF editing nor React PDF reading. No cutover task follows from this source scope.

Full 97-row source evidence, source census & adversarial review are in [kdp.json](./kdp.json).
