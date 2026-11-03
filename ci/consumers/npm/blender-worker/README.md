# @rightkit/blender-worker npm consumer (npm.@rightkit/blender-worker)

Packed-archive consumer for `@rightkit/blender-worker` 0.1.0. Recipe ID: `blender-worker-export-surface-consumer`.
The unit is installed only from the sealed `file:` archive. It declares no dependencies and imports no
Node built-in beyond the file and crypto helpers in `consumer.mjs`.

Surface (exact `exports` keys): `.` (`index.js`, which exports `workerDir`) and `./package.json`.

Scenarios: 2. The installed manifest and export surface are checked against the archive, and the shipped
worker source (`render_job.py`, `worker-manifest.json`) is checked present under `workerDir`. No Blender,
Python render job or worker process is run.
