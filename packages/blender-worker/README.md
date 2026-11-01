# @rightkit/blender-worker

The Blender render worker as Python source under `worker/`: `render_job.py` (entry, run as `blender -b --python render_job.py -- job.json`), the `renderright` package, install and qualification tools, and unit tests. `workerDir` returns the absolute path of that folder so a host can install it with `worker/tools/install_worker.py --destination <dir>`.

Licensed GPL-3.0-or-later because the worker imports Blender's `bpy`. Set `RIGHTKIT_BLENDER_ASSETS_DIR` to point the look-development code at an assets folder.

Moved into RightKit from GenRight (workers/blender) on 2026-11-01.
