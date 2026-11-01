"""Local authoring artifacts, deliberately separate from production asset adoption."""
import json
import os
import time

import bpy
import numpy as np

from .contract import ContractError, atomic_json, canonical_hash, sha256_file


def _projection(scene, meshes):
    render = scene.render
    projection = scene.camera.calc_matrix_camera(bpy.context.evaluated_depsgraph_get(),
        x=render.resolution_x, y=render.resolution_y,
        scale_x=render.pixel_aspect_x, scale_y=render.pixel_aspect_y)
    camera_matrix = projection @ scene.camera.matrix_world.inverted()
    arrays, outside, count = [], 0, 0
    for obj in meshes:
        vertices = np.empty((len(obj.data.vertices), 3), dtype=np.float32)
        obj.data.vertices.foreach_get('co', vertices.ravel())
        homogeneous = np.column_stack((vertices, np.ones(len(vertices))))
        clip = homogeneous @ np.asarray(camera_matrix @ obj.matrix_world).T
        uv = (clip[:, :2] / clip[:, 3:4] + 1) * .5
        arrays.append(uv)
        outside += int(np.count_nonzero(np.any((uv < 0) | (uv > 1), axis=1) | (clip[:, 3] <= 0)))
        count += len(vertices)
    all_uv = np.concatenate(arrays)
    return {"bounds_uv": [[float(all_uv[:, i].min()), float(all_uv[:, i].max())] for i in (0, 1)],
            "vertices": count, "outside_frame": outside}


def run_authoring(job, root):
    from .product_geometry import import_and_rest
    from .premium_look import configure_render
    from .editorial_look import settings, draft_materials, build_set, camera, grade
    from .product_review import load_product_spec, review_summary
    from .worker import _configure_device, _render_with_fallback, _worker_code_hash

    started = time.monotonic()
    out, params = job["output_dir"], job["params"]
    spec = load_product_spec(job)
    source_hash = sha256_file(job["source"])
    if params.get("source_sha256") not in (None, source_hash):
        raise ContractError("source_sha256 does not match authoring input")
    if os.path.exists(out):
        raise ContractError("authoring requires a fresh output directory")
    os.makedirs(out)
    identity = {"source_sha256": source_hash, "worker_code_sha256": _worker_code_hash(root),
                "recipe_sha256": sha256_file(os.path.join(root, "recipes", "manifest.json")),
                "product_spec_sha256": sha256_file(params["product_spec"]) if params.get("product_spec") else None,
                "blender_version": bpy.app.version_string,
                "job": {k: v for k, v in job.items() if k != "output_dir"}}
    fingerprint = canonical_hash(identity)
    progress = os.path.join(out, "authoring-progress.json")
    atomic_json(progress, {"state": "running", "job_id": job["job_id"], "fingerprint": fingerprint})
    try:
        bpy.ops.object.select_all(action='SELECT')
        bpy.ops.object.delete(use_global=False)
        # Incomplete draft specs cannot silently change physical registration.
        meshes, blade, transform, geometry = import_and_rest(job["source"], spec)
        atomic_json(os.path.join(out, "geometry.json"), geometry)
        artifacts = ["geometry.json"]
        review = review_summary(spec, source_hash, overridden=True)
        diagnostics = {"recipe": job["recipe"], "product_review": review}
        if job["recipe"] == "lookdev":
            cfg = settings(params)
            from .finish_profiles import resolve as resolve_finish
            finish = resolve_finish(cfg["finish_profile"], spec, params.get("lookdev_settings", {}))
            cfg["finish_detail_parts"] = finish["detail_parts"] if finish else []
            cfg["finish_pivot_object"] = finish["pivot_object"] if finish else None
            finish_table = draft_materials(meshes, blade, transform, cfg, finish)
            scene = bpy.context.scene
            from .editorial_look import pose_meshes, prop_block, foreground, backdrop, link_set_receivers
            transform = pose_meshes(meshes, blade, transform, cfg)
            configure_render(scene, params["width"], params["height"], params["samples"], denoise=False)
            device = _configure_device(params)
            cam, target = camera(scene, meshes, cfg)
            projection = _projection(scene, meshes)
            if projection["outside_frame"] and not cfg["frame_part"]:
                raise ContractError("whole-product framing clips geometry")
            build_set(scene, cfg, target, meshes, blade, transform)
            prop_block(scene, meshes, cfg)
            foreground(scene, cfg)
            backdrop(scene, cfg, target)
            link_set_receivers(scene, meshes)
            grade(scene, cfg)
            scene.render.image_settings.media_type = 'IMAGE'
            scene.render.image_settings.file_format = 'PNG'
            scene.render.image_settings.color_mode = 'RGB'
            scene.render.image_settings.color_depth = '8'
            scene.render.filepath = os.path.join(out, "lookdev.png")
            render_start = time.monotonic()
            fallback = _render_with_fallback(params)
            render_seconds = time.monotonic() - render_start
            bpy.ops.wm.save_as_mainfile(filepath=os.path.join(out, "scene.blend"))
            diagnostics.update({"settings": cfg, "camera_authorship": "editorial_draft_from_current_mesh_bounds",
                                "projection": projection, "finishes": finish_table,
                                "depth_of_field": True, "samples": scene.cycles.samples,
                                "denoise": False, "adaptive_sampling": False,
                                "device": fallback or device, "render_seconds": render_seconds,
                                "width": params["width"], "height": params["height"],
                                "diagnostics_are_aesthetic_acceptance": False})
            atomic_json(os.path.join(out, "lookdev.json"), diagnostics)
            artifacts += ["lookdev.png", "scene.blend", "lookdev.json"]
        if sha256_file(job["source"]) != source_hash:
            raise RuntimeError("Authoring source changed during run; use a fresh experiment identity")
        result = {"schema": 1, "kind": "authoring", "job_id": job["job_id"], "fingerprint": fingerprint,
                  "identity": identity, "product_review": review, "elapsed_seconds": time.monotonic() - started,
                  "artifacts": [{"path": name, "sha256": sha256_file(os.path.join(out, name)),
                                  "bytes": os.path.getsize(os.path.join(out, name))} for name in artifacts]}
        atomic_json(os.path.join(out, "authoring-manifest.json"), result)
        atomic_json(progress, {"state": "complete", "job_id": job["job_id"], "fingerprint": fingerprint})
        print(json.dumps({"event": "AUTHORING_COMPLETE", "job_id": job["job_id"],
                          "manifest": "authoring-manifest.json", "fingerprint": fingerprint}), flush=True)
        return result
    except Exception as exc:
        atomic_json(progress, {"state": "failed", "job_id": job["job_id"], "fingerprint": fingerprint,
                               "message": str(exc)})
        raise
