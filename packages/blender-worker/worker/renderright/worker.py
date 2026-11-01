"""Render lifecycle, checkpoints, manifests, and media encoding."""
import json
import os
import re
import shutil
import sys
import time

import bpy

from . import __version__
from .contract import ContractError, atomic_json, canonical_hash, normalize_job, sha256_file, validate_output_path
from .pngcheck import anomaly, decode_png, frame_quality
from .scene import animate, import_glb, setup

_ACTIVE_FINGERPRINT = None


def _event(name, **fields):
    print(json.dumps(dict(event=name, **fields), sort_keys=True), flush=True)


def _manifest_recipe(root, recipe):
    path = os.path.join(root, "recipes", "manifest.json")
    return path, sha256_file(path)


def _configure_device(params):
    scene = bpy.context.scene
    if params.get("device") == "cpu":
        scene.cycles.device = "CPU"
        return "CPU"
    try:
        prefs = bpy.context.preferences.addons["cycles"].preferences
        backend_order = ("OPTIX", "CUDA", "HIP", "ONEAPI", "METAL")
        for backend in backend_order:
            try:
                prefs.compute_device_type = backend
                refresh = getattr(prefs, "refresh_devices", None)
                if refresh:
                    refresh()
                else:
                    prefs.get_devices()
            except Exception:
                continue
            detected = [d for d in prefs.devices if d.type == backend]
            if detected:
                for d in prefs.devices:
                    d.use = d in detected
                scene.cycles.device = "GPU"
                return ",".join(d.type + ":" + d.name for d in detected)
    except Exception as exc:
        print("GPU discovery unavailable: " + str(exc), file=sys.stderr, flush=True)
    scene.cycles.device = "CPU"
    return "CPU fallback"


def _render_with_fallback(params):
    try:
        bpy.ops.render.render(write_still=True)
        return None
    except Exception:
        if params.get("device") != "auto" or bpy.context.scene.cycles.device != "GPU":
            raise
        bpy.context.scene.cycles.device = "CPU"
        bpy.ops.render.render(write_still=True)
        return "CPU runtime fallback"


def _load_previous(path, fingerprint):
    try:
        with open(path, encoding="utf-8") as f:
            old = json.load(f)
        if old.get("job_fingerprint") == fingerprint and isinstance(old.get("frames"), dict):
            return old["frames"]
    except (OSError, ValueError):
        pass
    return {}


def _checkpoint(root, old, frame, fingerprint, width, height):
    rel = "frames/frame_{:06d}.png".format(frame)
    path = validate_output_path(root, rel)
    rec = old.get(str(frame))
    if not rec or rec.get("job_fingerprint") != fingerprint or rec.get("path") != rel or not os.path.isfile(path):
        return None
    try:
        digest = sha256_file(path)
        decoded = decode_png(path)
        quality = frame_quality(decoded)
        if (digest == rec.get("sha256") and decoded[0:2] == (width, height)
                and rec.get("decode_ok") is True and rec.get("quality") == quality):
            return rec
    except Exception:
        return None
    return None


def _write_progress(path, job, fingerprint, state, message, frames):
    atomic_json(path, {"schema": 1, "job_id": job["job_id"], "job_fingerprint": fingerprint,
                       "state": state, "message": message, "frames": frames})


def _analyze(frames, root, count, shots, only=None):
    candidates = range(1, count - 1) if only is None else [only]
    def shot_index(frame):
        return next((i for i, s in enumerate(shots) if s["start_frame"] <= frame < s["end_frame"]), None)
    for f in candidates:
        if f < 1 or f >= count - 1:
            continue
        if len({shot_index(x) for x in (f - 1, f, f + 1)}) != 1:
            if str(f) in frames:
                frames[str(f)]["anomaly"] = {"status": "shot_boundary"}
            continue
        records = [frames.get(str(i)) for i in (f - 1, f, f + 1)]
        if not all(records):
            continue
        try:
            decoded = [decode_png(validate_output_path(root, r["path"])) for r in records]
            result = anomaly(*decoded)
            frames[str(f)]["anomaly"] = result
        except Exception as exc:
            frames[str(f)]["anomaly"] = {"status": "scan_error", "message": str(exc)}
    if only is None:
        for f in range(count):
            if str(f) in frames and frames[str(f)].get("anomaly", {}).get("status") == "pending":
                frames[str(f)]["anomaly"] = {"status": "insufficient_neighbours"}


def _artifact(root, asset_id, rel, mime, role):
    path = validate_output_path(root, rel)
    if not os.path.isfile(path):
        raise RuntimeError("expected artifact missing: " + rel)
    return {"id": asset_id, "path": rel.replace(os.sep, "/"), "mime": mime,
            "sha256": sha256_file(path), "bytes": os.path.getsize(path), "role": role}


def _worker_code_hash(root):
    paths = ["render_job.py", "renderright/__init__.py", "renderright/contract.py", "renderright/pngcheck.py",
             "renderright/scene.py", "renderright/worker.py", "renderright/rest_pose.py"]
    paths.extend(p for p in ("renderright/authoring.py", "renderright/premium_look.py", "renderright/fenrir_geometry.py", "renderright/product_geometry.py", "renderright/product_registration.py",
                            "renderright/product_review.py", "renderright/approved_materials.py", "renderright/swatch.py", "renderright/editorial_look.py", "renderright/finish_profiles.py", "recipes/fenrir-editorial-draft.json",
                            "recipes/samples/fenrir-sample-product.json")
                 if os.path.isfile(os.path.join(root, *p.split("/"))))
    return canonical_hash({p: sha256_file(os.path.join(root, *p.split("/"))) for p in paths})


def _guard_output_directory(root, job, fingerprint, mechanism_path=None):
    """Reject collisions with user files; only matching worker runs may resume their outputs."""
    progress_path = os.path.join(root, "render-progress.json")
    result_path = os.path.join(root, "result-manifest.json")
    progress = None
    result = None
    try:
        with open(progress_path, encoding="utf-8") as f:
            progress = json.load(f)
    except (OSError, ValueError):
        pass
    try:
        with open(result_path, encoding="utf-8") as f:
            result = json.load(f)
    except (OSError, ValueError):
        pass
    if progress and progress.get("job_fingerprint") not in (None, fingerprint):
        raise ContractError("output directory contains a checkpoint for another job")
    if result and (result.get("job_id") != job["job_id"] or result.get("job_fingerprint") != fingerprint):
        raise ContractError("output directory contains a finalized result for another job")
    if result:
        for item in result.get("artifacts", []):
            path = validate_output_path(root, item.get("path"))
            if not os.path.isfile(path) or os.path.getsize(path) != item.get("bytes") or sha256_file(path) != item.get("sha256"):
                raise ContractError("existing finalized artifact failed integrity check")
    matching_progress = bool(progress and progress.get("job_fingerprint") == fingerprint)
    matching_result = bool(result and result.get("job_id") == job["job_id"] and result.get("job_fingerprint") == fingerprint)
    provider_owned = False
    owner_path = os.path.join(root, "provider-state.json")
    if os.path.isfile(owner_path):
        with open(owner_path, encoding="utf-8") as f:
            owner = json.load(f)
        if owner.get("schema") != 1 or owner.get("job_id") != job["job_id"]:
            raise ContractError("provider receipt belongs to another job")
        provider_owned = True
    generated = {"render-progress.json", "result-manifest.json", "shot-manifest.json", "hero.png", "probe.png", "probe.json", "movie.mp4", "movie.mov", "movie.renderright-tmp.mp4", "movie.renderright-tmp.mov", "job.json"}
    mechanism_rel = None
    if mechanism_path:
        try:
            mechanism_rel = os.path.relpath(os.path.realpath(mechanism_path), os.path.realpath(root)).replace(os.sep, "/")
            if mechanism_rel.startswith("../") or mechanism_rel == "..":
                mechanism_rel = None
        except ValueError:
            pass
    for current, dirs, files in os.walk(root, followlinks=False):
        rel_dir = os.path.relpath(current, root)
        for dirname in list(dirs):
            full = os.path.join(current, dirname)
            if os.path.islink(full) or (rel_dir == "." and dirname != "frames") or rel_dir != ".":
                raise ContractError("output directory contains an unowned subdirectory")
        for name in files:
            filepath = os.path.join(current, name)
            if os.path.islink(filepath):
                raise ContractError("output directory contains a symlink")
            rel = os.path.relpath(filepath, root).replace(os.sep, "/")
            if rel_dir == "." and (rel == "hero.png.tmp" or re.fullmatch(r"\.(?:render-progress|result-manifest|shot-manifest)\.json\.[A-Za-z0-9_-]{8}\.tmp", name)):
                if matching_progress or matching_result:
                    continue
                raise ContractError("worker temporary file has no matching checkpoint or result")
            if rel == "job.json" or rel == mechanism_rel:
                continue
            if provider_owned and rel_dir == ".":
                if rel == "provider-state.json" or re.fullmatch(r"blender-attempt-[1-9][0-9]*\.(stdout|stderr)\.log", rel):
                    continue
                if rel == "cancel.json":
                    with open(filepath, encoding="utf-8") as f:
                        cancellation = json.load(f)
                    if cancellation.get("job_id") != job["job_id"]:
                        raise ContractError("cancellation receipt belongs to another job")
                    continue
            if rel.startswith("frames/") and rel.count("/") == 1 and name.startswith("frame_") and name.endswith(".png"):
                if progress and progress.get("job_fingerprint") == fingerprint:
                    continue
                raise ContractError("unowned frame file already exists")
            if rel in generated:
                if progress and progress.get("job_fingerprint") == fingerprint:
                    continue
                if result and result.get("job_fingerprint") == fingerprint:
                    continue
                raise ContractError("worker output path already exists: " + rel)
            raise ContractError("output directory contains an unowned file: " + rel)
    return result


def _publish_hero(root, frame_rel, width, height):
    source = validate_output_path(root, frame_rel)
    decoded = decode_png(source)
    if decoded[0:2] != (width, height):
        raise RuntimeError("selected hero frame dimensions do not match job")
    target = validate_output_path(root, "hero.png")
    tmp = target + ".tmp"
    shutil.copyfile(source, tmp)
    if sha256_file(tmp) != sha256_file(source):
        os.unlink(tmp)
        raise RuntimeError("hero copy integrity mismatch")
    os.replace(tmp, target)
    return "hero.png"


def _encode_video(root, params):
    # The worker never launches ffmpeg. It hands the host a structured request (frame pattern, fps, codec choice)
    # and waits for the host's answer: the Rust host runs the owner-resolved ffmpeg through rightkit-child-process
    # and writes encode-result.json. No path, argv or environment variable from this process reaches ffmpeg.
    transparent = bool(params.get("transparent"))
    ext = "mov" if transparent else "mp4"
    rel = "movie." + ext
    target = validate_output_path(root, rel)
    fps = params["fps"]
    temp_rel = "movie.renderright-tmp." + ext
    temp_target = validate_output_path(root, temp_rel)
    request_path = validate_output_path(root, "encode-request.json")
    result_path = validate_output_path(root, "encode-result.json")
    for stale in (request_path, result_path):
        if os.path.exists(stale):
            os.unlink(stale)
    atomic_json(request_path, {"schema": 1, "fps": {"num": fps["num"], "den": fps["den"]},
                               "frame_count": params["frame_count"], "transparent": transparent})
    deadline = time.time() + 660
    try:
        while not os.path.isfile(result_path):
            if time.time() > deadline:
                raise RuntimeError("host ffmpeg encode did not answer in time")
            time.sleep(0.25)
        with open(result_path, "r", encoding="utf-8") as handle:
            answer = json.load(handle)
        if answer.get("cancelled"):
            raise RuntimeError("video encode cancelled")
        if not answer.get("ok"):
            raise RuntimeError("ffmpeg encoding failed: " + str(answer.get("message", ""))[-2000:])
        if not os.path.isfile(temp_target) or os.path.getsize(temp_target) == 0:
            raise RuntimeError("ffmpeg produced no movie output")
        os.replace(temp_target, target)
    finally:
        for leftover in (temp_target, request_path, result_path):
            if os.path.exists(leftover):
                os.unlink(leftover)
    return rel, "video/quicktime" if transparent else "video/mp4"


def _probe(job, fingerprint, source_hash, recipe_info, recipe_hash, blender_version, device):
    p = job["params"]
    bpy.ops.mesh.primitive_uv_sphere_add(segments=16, ring_count=8, radius=0.5)
    test_mesh = bpy.context.object
    test_mesh.name = "GPU probe sample"
    setup([test_mesh], p)
    scene = bpy.context.scene
    scene.render.resolution_x, scene.render.resolution_y = p["width"], p["height"]
    scene.cycles.samples = p["samples"]
    scene.render.image_settings.file_format = "PNG"
    scene.render.filepath = os.path.join(job["output_dir"], "probe.png")
    fallback = _render_with_fallback(p)
    if fallback:
        device = fallback
    w, h, _, _ = decode_png(scene.render.filepath)
    recipe_manifest, recipe_digest = recipe_info, recipe_hash
    return [{"id": "probe", "path": "probe.png", "mime": "image/png", "sha256": sha256_file(scene.render.filepath),
             "bytes": os.path.getsize(scene.render.filepath), "role": "probe"}], {
        "device": device, "render_ok": True, "width": w, "height": h, "gpu_requested": p.get("device") != "cpu",
        "gpu_used": device not in ("CPU", "CPU fallback", "CPU runtime fallback"),
        "recipe_manifest": recipe_manifest, "recipe_sha256": recipe_digest
    }


def run_job(job_path, root):
    global _ACTIVE_FINGERPRINT
    with open(job_path, encoding="utf-8") as f:
        raw = json.load(f)
    job, recipe_manifest = normalize_job(raw, root)
    from .product_review import load_product_spec, review_summary
    spec = load_product_spec(job)
    if job["recipe"] == "swatch":
        from .swatch import run_swatch
        return run_swatch(job, root, None)
    if job["kind"] == "authoring":
        from .authoring import run_authoring
        return run_authoring(job, root)
    out = job["output_dir"]
    os.makedirs(out, exist_ok=True)
    progress_path = os.path.join(out, "render-progress.json")
    recipe_path, recipe_sha = _manifest_recipe(root, job["recipe"])
    entry = recipe_manifest["recipes"][job["recipe"]]
    source_hash = sha256_file(job["source"]) if job.get("source") else None
    params = job["params"]
    product_spec_hash = sha256_file(params["product_spec"]) if params.get("product_spec") else None
    mech = params.get("mechanism_json")
    mechanism_hash = None
    mechanism_doc = None
    if mech:
        if not isinstance(mech, str) or not os.path.isabs(mech) or not os.path.isfile(mech):
            raise ContractError("mechanism_json must name an existing absolute JSON file")
        with open(mech, encoding="utf-8") as f:
            mechanism_doc = json.load(f)
        mechanism_hash = sha256_file(mech)
    for key, actual in (("source_sha256", source_hash), ("recipe_sha256", recipe_sha), ("mechanism_sha256", mechanism_hash)):
        requested = params.get(key)
        if requested is not None and requested != actual:
            raise ContractError(key + " does not match requested input")
    requested_recipe_version = params.get("recipe_version")
    if requested_recipe_version is not None and requested_recipe_version != entry["version"]:
        raise ContractError("recipe_version does not match installed recipe")
    fingerprint = canonical_hash({"job": {k: v for k, v in job.items() if k != "output_dir"}, "source_sha256": source_hash, "product_spec_sha256": product_spec_hash,
                                  "mechanism_sha256": mechanism_hash, "worker_version": __version__,
                                  "recipe_version": entry["version"], "recipe_sha256": recipe_sha,
                                  "blender_version": bpy.app.version_string, "worker_code_sha256": _worker_code_hash(root)})
    _ACTIVE_FINGERPRINT = fingerprint
    existing_result = _guard_output_directory(out, job, fingerprint, mech)
    if existing_result:
        _event("WORKER_COMPLETE", job_id=job["job_id"], result_manifest="result-manifest.json", job_fingerprint=fingerprint,
               reused_finalized_result=True)
        return
    os.makedirs(os.path.join(out, "frames"), exist_ok=True)
    frames = _load_previous(progress_path, fingerprint)
    _write_progress(progress_path, job, fingerprint, "running", "Preparing Blender scene", frames)
    device = _configure_device(params)
    artifacts = []
    review_assignments_match = False
    reviewed_shaders_applied = False
    material_override = params.get("material_recipe") not in (None, "")
    effective_finishes = {}
    if job["recipe"] == "probe":
        artifacts, probe_info = _probe(job, fingerprint, source_hash, recipe_path, recipe_sha, bpy.app.version_string, device)
        atomic_json(os.path.join(out, "probe.json"), probe_info)
        artifacts.append(_artifact(out, "probe_report", "probe.json", "application/json", "probe"))
    else:
        objects = import_glb(job["source"])
        if spec and not material_override:
            finishes = spec.get("finishes") if isinstance(spec.get("finishes"), dict) else {}
            parts = spec.get("parts") if isinstance(spec.get("parts"), dict) else {}
            used = {finish for part in parts.values() if isinstance(part, dict) and isinstance(part.get("slots"), dict)
                    for finish in part["slots"].values() if isinstance(finish, str)}
            # Applying an explicitly selected shader is useful in draft previews too.
            # Confirmation and owner approval affect the review summary, not whether
            # a requested appearance can be inspected.
            eligible = spec.get("schema") == 1 and bool(parts) and bool(used)
            for finish_id in used:
                finish = finishes.get(finish_id)
                shader = finish.get("shader") if isinstance(finish, dict) else None
                if (not isinstance(finish, dict) or not isinstance(shader, dict) or
                        not isinstance(shader.get("path"), str) or not shader.get("path") or
                        not isinstance(shader.get("material"), str) or not shader.get("material").strip()):
                    eligible = False
                    break
            mesh_objects = [obj for obj in objects if obj.type == "MESH"]
            by_name = {obj.name: obj for obj in mesh_objects}
            review_assignments_match = (eligible and len(by_name) == len(mesh_objects) and
                                        set(by_name) == set(parts))
            if review_assignments_match:
                for part_name, part in parts.items():
                    slots = part.get("slots") if isinstance(part, dict) else None
                    current = [mat.name if mat else None for mat in by_name[part_name].data.materials]
                    if not isinstance(slots, dict) or set(current) != set(slots) or any(name is None for name in current):
                        review_assignments_match = False
                        break
            if review_assignments_match:
                from .approved_materials import apply_reviewed_materials
                effective_finishes = apply_reviewed_materials(objects, {"spec": spec})
                reviewed_shaders_applied = True
                _event("PRODUCT_FINISHES_APPLIED", job_id=job["job_id"], finishes=effective_finishes)
        data = setup(objects, params)
        count = 1 if job["kind"] == "image" else params["frame_count"]
        selected_shot = params.get("shot_id")
        source_frame_count = params.get("source_frame_count", count)
        source_start_frame = params.get("shot_start_frame", 0)
        if count > 1 or selected_shot:
            animate(job["recipe"], data, source_frame_count if selected_shot else count, mechanism_doc,
                    selected_shot=selected_shot, camera_override=params.get("camera"), output_count=count)
            if selected_shot and data.get("shots"):
                data["shots"][0]["source_start_frame"] = 0
        shots = data.get("shots") or [{"id": "shot-000", "start_frame": 0, "end_frame": count,
                                       "source_start_frame": 0, "camera": {"lens_mm": data["lens_mm"],
                                       "framing": "aggregate-bounds"}}]
        scene = bpy.context.scene
        scene.frame_step = 1
        scene.render.fps, scene.render.fps_base = params["fps"]["num"], params["fps"]["den"]
        for frame in range(count):
            rec = _checkpoint(out, frames, frame, fingerprint, params["width"], params["height"])
            if rec:
                frames[str(frame)] = rec
                continue
            scene.frame_set(frame + source_start_frame)
            rel = "frames/frame_{:06d}.png".format(frame)
            target = validate_output_path(out, rel)
            scene.render.filepath = target
            started = time.time()
            fallback = _render_with_fallback(params)
            if fallback:
                device = fallback
            decoded = decode_png(target)
            if decoded[0:2] != (params["width"], params["height"]):
                raise RuntimeError("rendered frame dimensions do not match job")
            quality = frame_quality(decoded)
            frames[str(frame)] = {"path": rel, "sha256": sha256_file(target), "width": decoded[0], "height": decoded[1],
                                  "decode_ok": True, "job_fingerprint": fingerprint, "anomaly": {"status": "pending"},
                                  "quality": quality,
                                  "render_seconds": round(time.time() - started, 3)}
            _analyze(frames, out, count, shots, frame - 1)
            _write_progress(progress_path, job, fingerprint, "running", "Rendered frame {} of {}".format(frame + 1, count), frames)
            _event("FRAME_DONE", job_id=job["job_id"], frame=frame, frame_count=count, path=rel)
        _analyze(frames, out, count, shots)
        flagged = [int(frame) for frame, rec in frames.items()
                   if rec.get("anomaly", {}).get("anomaly") is True or rec.get("anomaly", {}).get("status") == "scan_error"]
        if flagged:
            _write_progress(progress_path, job, fingerprint, "failed", "Temporal frame anomaly detected: " + ",".join(map(str, flagged)), frames)
            raise RuntimeError("temporal frame anomaly detected at frame(s): " + ", ".join(map(str, flagged)))
        _write_progress(progress_path, job, fingerprint, "encoding", "Frames complete", frames)
        if count > 1:
            movie_rel, movie_mime = _encode_video(out, params)
            artifacts.append(_artifact(out, "movie", movie_rel, movie_mime, "movie"))
            image_rel = "frames/frame_{:06d}.png".format(count // 2)
            hero_rel = _publish_hero(out, image_rel, params["width"], params["height"])
            artifacts.append(_artifact(out, "hero", hero_rel, "image/png", "hero"))
        else:
            hero_rel = _publish_hero(out, frames["0"]["path"], params["width"], params["height"])
            artifacts.append(_artifact(out, "hero", hero_rel, "image/png", "hero"))

    source = {"path": job.get("source"), "sha256": source_hash} if job.get("source") else {"path": None, "sha256": None}
    common = {"schema": 1, "job_id": job["job_id"], "worker_version": __version__, "blender_version": bpy.app.version_string,
              "recipe": {"id": job["recipe"], "version": entry["version"], "sha256": recipe_sha}, "source": source,
              "fps": params["fps"], "artifacts": artifacts}
    common["product_review"] = review_summary(spec, source_hash, assignments_match=review_assignments_match,
                                               shader_applied=reviewed_shaders_applied, overridden=material_override)
    if job["recipe"] != "probe":
        count = 1 if job["kind"] == "image" else params["frame_count"]
        shot_artifacts = [a for a in artifacts if a["role"] in ("movie", "hero")]
        artifact_id = next((a["id"] for a in shot_artifacts if a["role"] == "movie"), "hero")
        rendered_shots = shots if job["kind"] == "video" else [{"id": "hero", "start_frame": 0, "end_frame": 1,
                            "source_start_frame": 0, "camera": {"lens_mm": data["lens_mm"], "framing": "aggregate-bounds"}}]
        regen_params = {k: v for k, v in params.items() if k not in ("source", "source_path", "source_sha256", "recipe_sha256", "recipe_version", "worker_version", "blender_version", "mechanism_json", "mechanism_sha256")}
        regen_params.update(recipe=job["recipe"], source_path=job["source"], source_sha256=source_hash,
                            worker_version=__version__, blender_version=bpy.app.version_string,
                            recipe_version=entry["version"], recipe_sha256=recipe_sha)
        prompt = params.get("prompt")
        if not prompt:
            prompt = {"hero": "Studio hero image showing the GLB product clearly.",
                      "turntable": "Studio turntable film showing the GLB product from all sides.",
                      "exploded": "Studio exploded-view film using only measured mechanism offsets.",
                      "film": "Studio product film showing aggregate silhouette and surface details."}.get(job["recipe"], "")
        regen_params["prompt"] = prompt
        if mech is not None:
            regen_params["mechanism_json"] = mech
            regen_params["mechanism_sha256"] = mechanism_hash
        shot_docs = []
        for shot_data in rendered_shots:
            shot_params = dict(regen_params)
            if job["recipe"] == "film" and job["kind"] == "video":
                shot_len = shot_data["end_frame"] - shot_data["start_frame"]
                shot_params.update(shot_id=shot_data["id"], frame_count=shot_len,
                                   camera={key: shot_data["camera"][key] for key in
                                           ("lens_mm", "azimuth_deg", "elevation_deg", "distance_scale")},
                                   shot_start_frame=(source_start_frame + shot_data.get("source_start_frame", 0)),
                                   source_frame_count=source_frame_count)
            shot_docs.append(dict(shot_data, artifact_id=artifact_id, source_start_frame=shot_data["source_start_frame"],
                                  lens_mm=shot_data["camera"].get("lens_mm", data["lens_mm"]), source_sha256=source_hash,
                                  regen={"provider": "local-blender", "model": "blender-" + job["recipe"],
                                         "params": shot_params}))
        shot_doc = dict(common, type="generated_source", shots=shot_docs)
        atomic_json(os.path.join(out, "shot-manifest.json"), shot_doc)
        artifacts.append(_artifact(out, "shot_manifest", "shot-manifest.json", "application/json", "shot_manifest"))
    result = {"schema": 1, "job_id": job["job_id"], "job_fingerprint": fingerprint,
              "worker": {"name": "renderright", "version": __version__}, "blender_version": bpy.app.version_string,
              "recipe": {"id": job["recipe"], "version": entry["version"], "sha256": recipe_sha},
              "source": source, "fps": params["fps"], "artifacts": artifacts}
    result["product_review"] = common["product_review"]
    atomic_json(os.path.join(out, "result-manifest.json"), result)
    _write_progress(progress_path, job, fingerprint, "complete", "Worker completed", frames)
    _event("WORKER_COMPLETE", job_id=job["job_id"], result_manifest="result-manifest.json", job_fingerprint=fingerprint)


def main(args):
    if args == ["--version"]:
        print(json.dumps({"name": "renderright", "version": __version__}))
        return 0
    if len(args) != 1:
        print("usage: blender -b --python-exit-code 1 --python render_job.py -- job.json", file=sys.stderr)
        return 2
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    try:
        run_job(os.path.abspath(args[0]), root)
        return 0
    except Exception as exc:
        try:
            with open(os.path.abspath(args[0]), encoding="utf-8") as f:
                job = json.load(f)
            out = job.get("output_dir")
            if isinstance(out, str) and os.path.isabs(out):
                progress = os.path.join(out, "render-progress.json")
                try:
                    with open(progress, encoding="utf-8") as f:
                        prior = json.load(f)
                    if prior.get("job_id") == job.get("job_id") and prior.get("job_fingerprint") == _ACTIVE_FINGERPRINT and isinstance(prior.get("frames"), dict):
                        prior.update(state="failed", message=str(exc))
                        atomic_json(progress, prior)
                except (OSError, ValueError):
                    pass
        except Exception:
            pass
        print("RenderRight error: " + str(exc), file=sys.stderr, flush=True)
        raise
