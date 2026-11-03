"""Render primitive finish samples for look development; output remains a draft."""
import os
import math
import time

import bpy
from mathutils import Vector

from .contract import ContractError, atomic_json, canonical_hash, sha256_file


def _sample_material(job, spec):
    params = job["params"]
    finish_id = params.get("swatch_finish")
    finish = None
    if isinstance(spec, dict):
        finishes = spec.get("finishes", {})
        if isinstance(finishes, dict) and finishes:
            if finish_id is not None and finish_id not in finishes:
                raise ContractError("requested swatch finish is absent from product spec")
            finish_id = finish_id if finish_id is not None else next(iter(finishes))
            finish = finishes.get(finish_id)
    shader = finish.get("shader") if isinstance(finish, dict) else None
    if isinstance(shader, dict) and shader.get("path") and shader.get("material"):
        path, name = shader["path"], shader["material"]
        if not os.path.isabs(path) or not os.path.isfile(path):
            raise ContractError("requested finish shader path must exist and be absolute")
        with bpy.data.libraries.load(path, link=False) as (source, target):
            if name not in source.materials:
                raise ContractError("requested shader material is missing: " + name)
            target.materials = [name]
        if target.materials[0] is None:
            raise ContractError("Blender could not load requested shader material")
        return target.materials[0], finish_id
    preset = params.get("material_recipe")
    material = bpy.data.materials.new("Draft swatch material")
    material.diffuse_color = (0.42, 0.46, 0.5, 1)
    material.use_nodes = True
    shader_node = material.node_tree.nodes.get("Principled BSDF")
    colors = {"brass": (0.55, 0.32, 0.12, 1), "copper": (0.55, 0.22, 0.12, 1),
              "anodized_black": (0.025, 0.03, 0.035, 1), "machined_aluminum": (0.48, 0.52, 0.56, 1),
              "titanium_stonewash": (0.28, 0.31, 0.34, 1)}
    if isinstance(preset, dict):
        raise ContractError("primitive swatch material_recipe must name one preset")
    color = colors.get(preset, material.diffuse_color[:])
    shader_node.inputs["Base Color"].default_value = color
    shader_node.inputs["Metallic"].default_value = 0.82
    shader_node.inputs["Roughness"].default_value = 0.28
    material.diffuse_color = color
    return material, finish_id


def run_swatch(job, root, review=None):
    started = time.monotonic()
    out, params = job["output_dir"], job["params"]
    if os.path.exists(out):
        raise ContractError("swatch requires a fresh output directory")
    from .product_review import load_product_spec, review_summary
    spec = load_product_spec(job)
    material, finish_id = _sample_material(job, spec)
    os.makedirs(out)
    scene = bpy.context.scene
    bpy.ops.object.select_all(action="SELECT")
    bpy.ops.object.delete(use_global=False)
    bpy.ops.mesh.primitive_plane_add(size=2.0, location=(0, 0, 0))
    ground = bpy.context.object
    ground.name = "Swatch ground"
    bpy.ops.mesh.primitive_uv_sphere_add(segments=64, ring_count=40, radius=0.12, location=(0, 0, 0.12))
    sphere = bpy.context.object
    sphere.name = "Draft finish sample"
    bpy.ops.object.shade_smooth()
    sphere.data.materials.append(material)
    # Flat sample exposes finish direction & roughness concealed by curved highlights.
    bpy.ops.mesh.primitive_plane_add(size=.18, location=(-.18, .04, .10), rotation=(math.pi / 2, 0, 0))
    sample_plane = bpy.context.object
    sample_plane.name = "Draft flat sample"
    sample_plane.data.materials.append(material)
    if params.get("view") == "detail":
        # Macro samples: 60 mm sphere & 45 mm coupon. Keep world units in metres
        # so millimetre-scale finish nodes retain their physical feature size.
        bpy.data.objects.remove(sample_plane, do_unlink=True)
        bpy.ops.mesh.primitive_cube_add(size=1, location=(-.18, .04, .10))
        sample_plane = bpy.context.object
        sample_plane.name = "Draft 45 mm finish coupon"
        sample_plane.dimensions = (.18, .006, .18)
        bpy.ops.object.transform_apply(location=False, rotation=False, scale=True)
        sample_plane.data.materials.append(material)
        bevel = sample_plane.modifiers.new("0.2 mm coupon chamfer", "BEVEL")
        bevel.width, bevel.segments = .0008, 3
    ground_mat = bpy.data.materials.new("Neutral ground")
    ground_mat.diffuse_color = (0.18, 0.18, 0.18, 1)
    ground_mat.use_nodes = True
    ground_mat.node_tree.nodes['Principled BSDF'].inputs['Base Color'].default_value = ground_mat.diffuse_color
    ground_mat.node_tree.nodes['Principled BSDF'].inputs['Roughness'].default_value = .72
    ground.data.materials.append(ground_mat)
    world = bpy.data.worlds.new("Swatch studio")
    world.use_nodes = True
    world.node_tree.nodes["Background"].inputs["Color"].default_value = (0.2, 0.2, 0.2, 1)
    world.node_tree.nodes["Background"].inputs["Strength"].default_value = 0.35
    scene.world = world
    for name, location, energy, size in (("Key", (0.35, -0.35, 0.55), 10, 0.45),
                                         ("Fill", (-0.4, -0.1, 0.32), 3, 0.4),
                                         ("Rim", (0, 0.35, 0.42), 5, 0.28)):
        data = bpy.data.lights.new(name, "AREA")
        data.energy, data.shape, data.size = energy, "DISK", size
        lamp = bpy.data.objects.new(name, data)
        scene.collection.objects.link(lamp)
        lamp.location = location
        lamp.rotation_euler = (Vector((0, 0, 0.12)) - lamp.location).to_track_quat("-Z", "Y").to_euler()
    camera_data = bpy.data.cameras.new("Swatch camera")
    camera = bpy.data.objects.new("Swatch camera", camera_data)
    scene.collection.objects.link(camera)
    camera.location = (0.4, -0.8, 0.38)
    camera.rotation_euler = (Vector((-.05, 0, 0.12)) - camera.location).to_track_quat("-Z", "Y").to_euler()
    camera_data.lens = 55
    scene.camera = camera
    if params.get("view") == "detail":
        for obj in list(scene.objects):
            obj.location *= .25
            if obj.type == "MESH":
                for vertex in obj.data.vertices:
                    vertex.co *= .25
                for modifier in obj.modifiers:
                    if modifier.type == "BEVEL":
                        modifier.width *= .25
            elif obj.type == "LIGHT":
                obj.data.energy *= .25 ** 2
                obj.data.size *= .25
        camera_data.clip_start = .001
        bpy.context.view_layer.update()
    scene.render.engine = "CYCLES"
    scene.cycles.samples = int(params["samples"])
    scene.cycles.use_denoising = False
    scene.render.resolution_x, scene.render.resolution_y = int(params["width"]), int(params["height"])
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.render.filepath = os.path.join(out, "swatch.png")
    from .worker import _configure_device, _render_with_fallback
    device = _configure_device(params)
    fallback = _render_with_fallback(params)
    image_path = os.path.abspath(scene.render.filepath)
    if not os.path.isfile(image_path):
        raise ContractError("Blender did not produce swatch image")
    image_hash = sha256_file(image_path)
    source_hash = sha256_file(job["source"]) if job.get("source") and os.path.isfile(job["source"]) else None
    summary = review_summary(spec, source_hash)
    manifest = {"schema": 1, "recipe": "swatch", "finish_id": finish_id,
                "reviewed": False, "image": {"path": image_path, "sha256": image_hash}}
    atomic_json(os.path.join(out, "swatch-manifest.json"), manifest)
    identity = {"job": {k: v for k, v in job.items() if k != "output_dir"},
                "product_spec_sha256": sha256_file(params["product_spec"]) if params.get("product_spec") else None,
                "source_sha256": source_hash, "worker_code_sha256": __import__("renderright.worker", fromlist=["_worker_code_hash"])._worker_code_hash(root)}
    fingerprint = canonical_hash(identity)
    atomic_json(os.path.join(out, "authoring-manifest.json"), {
        "schema": 1, "kind": "authoring", "job_id": job["job_id"], "fingerprint": fingerprint,
        "identity": identity, "elapsed_seconds": time.monotonic() - started,
        "diagnostics": {"samples": scene.cycles.samples, "denoise": False, "device": fallback or device,
                        "geometry": [sphere.name, sample_plane.name, ground.name],
                        "sphere_diameter_mm": 60 if params.get("view") == "detail" else 240,
                        "flat_sample_width_mm": 45 if params.get("view") == "detail" else 180}, "product_review": summary,
        "artifacts": [{"path": name, "sha256": sha256_file(os.path.join(out, name)),
                       "bytes": os.path.getsize(os.path.join(out, name))}
                      for name in ("swatch.png", "swatch-manifest.json")]})
    return manifest
