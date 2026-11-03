"""Generic GLB presentation setup. Source hierarchy and materials remain intact."""
import math

import bpy
from mathutils import Vector, Quaternion


def import_glb(path):
    before = set(bpy.data.objects)
    bpy.ops.import_scene.gltf(filepath=path)
    objects = [o for o in bpy.data.objects if o not in before and o.type == "MESH"]
    if not objects:
        raise RuntimeError("GLB contains no mesh objects")
    return objects


def bounds(objects):
    pts = [o.matrix_world @ Vector(c) for o in objects for c in o.bound_box]
    lo = Vector(tuple(min(p[i] for p in pts) for i in range(3)))
    hi = Vector(tuple(max(p[i] for p in pts) for i in range(3)))
    return lo, hi, (lo + hi) * 0.5, hi - lo


_METAL_PRESETS = {
    "anodized_black": ((0.04, 0.04, 0.04, 1), 0.85, 0.32),
    "machined_aluminum": ((0.78, 0.78, 0.78, 1), 1.0, 0.22),
    "brass": ((0.79, 0.65, 0.32, 1), 1.0, 0.25),
    "copper": ((0.72, 0.45, 0.20, 1), 1.0, 0.20),
    "titanium_stonewash": ((0.62, 0.62, 0.65, 1), 1.0, 0.72),
}


def _apply_material_recipe(objects, recipe):
    """Explicit override only; default path retains imported GLB material graphs and slots."""
    if not recipe:
        return
    if isinstance(recipe, str):
        assignments = {mat.name: recipe for obj in objects for mat in obj.data.materials if mat}
    elif isinstance(recipe, dict):
        assignments = recipe.get("materials", {})
        if not isinstance(assignments, dict):
            raise ValueError("material_recipe.materials must map imported material names to presets")
    else:
        raise ValueError("material_recipe must be a preset name or materials mapping")
    for material_name, preset in assignments.items():
        mat = bpy.data.materials.get(material_name)
        if mat is None:
            raise ValueError("material_recipe names missing imported material: " + material_name)
        if preset not in _METAL_PRESETS:
            raise ValueError("unsupported material preset: " + str(preset))
        if not mat.use_nodes or mat.node_tree is None:
            raise ValueError("material preset requires node-based source material: " + material_name)
        bsdf = next((n for n in mat.node_tree.nodes if n.type == "BSDF_PRINCIPLED"), None)
        if bsdf is None:
            raise ValueError("material preset requires Principled BSDF: " + material_name)
        base, metallic, roughness = _METAL_PRESETS[preset]
        for socket, value in (("Base Color", base), ("Metallic", metallic), ("Roughness", roughness)):
            if not bsdf.inputs[socket].is_linked:
                bsdf.inputs[socket].default_value = value


def _area(name, location, target, power, size, color):
    data = bpy.data.lights.new(name, "AREA")
    data.energy, data.shape, data.size, data.color = power, "DISK", size, color
    obj = bpy.data.objects.new(name, data)
    bpy.context.scene.collection.objects.link(obj)
    obj.location = location
    obj.rotation_euler = (Vector(target) - obj.location).to_track_quat("-Z", "Y").to_euler()
    return obj


def _action_fcurves(action):
    curves = getattr(action, "fcurves", None)
    if curves is not None:
        try:
            return list(curves)
        except Exception:
            pass
    out = []
    for layer in getattr(action, "layers", []):
        for strip in layer.strips:
            for bag in strip.channelbags:
                out.extend(list(bag.fcurves))
    return out


def _parent_keep_world(obj, parent):
    """Parent object while preserving evaluated transforms, including new objects."""
    view_layer = bpy.context.view_layer
    view_layer.update()
    world = obj.matrix_world.copy()
    obj.parent = parent
    view_layer.update()
    obj.matrix_parent_inverse = parent.matrix_world.inverted()
    obj.matrix_world = world
    view_layer.update()


def _camera_direction(camera):
    azimuth = math.radians(camera.get("azimuth_deg", -57.4))
    elevation = math.radians(camera.get("elevation_deg", 31.4))
    horizontal = math.cos(elevation)
    return Vector((math.cos(azimuth) * horizontal, math.sin(azimuth) * horizontal,
                   math.sin(elevation))).normalized()


def _fit_camera_distance(lo, hi, target, direction, lens, width, height, margin=0.85):
    """Fit all world AABB corners against both sensor axes in perspective space."""
    azimuth = math.atan2(direction.y, direction.x)
    right = Vector((-math.sin(azimuth), math.cos(azimuth), 0.0))
    if right.length < 1e-8:
        right = Vector((1.0, 0.0, 0.0))
    right.normalize()
    forward = -direction
    up = right.cross(forward).normalized()
    sensor_width = 36.0
    sensor_height = sensor_width * height / width
    required = 0.0
    for x in (lo.x, hi.x):
        for y in (lo.y, hi.y):
            for z in (lo.z, hi.z):
                rel = Vector((x, y, z)) - target
                toward = rel.dot(direction)
                needed_x = toward + 2.0 * lens * abs(rel.dot(right)) / (sensor_width * margin)
                needed_y = toward + 2.0 * lens * abs(rel.dot(up)) / (sensor_height * margin)
                required = max(required, needed_x, needed_y)
    return max(required * 1.015, 0.001)


def setup(objects, params):
    scene = bpy.context.scene
    lo, hi, center, dims = bounds(objects)
    largest = max(dims)
    if largest <= 1e-8:
        raise RuntimeError("GLB has zero-sized bounds")
    env = params.get("env") or "studio_lightbox"
    envs = {
        "studio_lightbox": ((0.28, 0.30, 0.34, 1), 0.65, 1.0, (0.13, 0.14, 0.16, 1)),
        "studio_grey": ((0.30, 0.30, 0.30, 1), 0.8, 0.9, (0.22, 0.22, 0.22, 1)),
        "studio_dark": ((0.012, 0.014, 0.018, 1), 0.45, 0.75, (0.025, 0.03, 0.04, 1)),
    }
    if env not in envs:
        raise ValueError("unsupported env preset: " + str(env))
    bg_color, bg_strength, light_scale, floor_color = envs[env]
    _apply_material_recipe(objects, params.get("material_recipe"))
    # Preserve GLB material datablocks and face material indices. Bevel modifies geometry only.
    for obj in objects:
        d = min(obj.dimensions)
        if d > 0.002:
            mod = obj.modifiers.new("Conservative edge highlight", "BEVEL")
            mod.width = min(d * 0.0125, largest * 0.0015, 0.001)
            mod.segments = 2
            mod.limit_method = "ANGLE"
            mod.angle_limit = math.radians(30)
    scene.render.engine = "CYCLES"
    scene.cycles.samples = params["samples"]
    scene.cycles.use_denoising = True
    scene.render.resolution_x, scene.render.resolution_y = params["width"], params["height"]
    scene.render.resolution_percentage = 100
    scene.render.image_settings.file_format = "PNG"
    scene.render.image_settings.color_mode = "RGBA" if params.get("transparent") else "RGB"
    scene.render.film_transparent = bool(params.get("transparent"))
    scene.view_settings.view_transform = "AgX"
    world = scene.world
    if world is None:
        world = bpy.data.worlds.new("RenderRight studio")
        scene.world = world
    world.use_nodes = True
    bg = world.node_tree.nodes.get("Background")
    if bg is None:
        bg = world.node_tree.nodes.new("ShaderNodeBackground")
        out = world.node_tree.nodes.get("World Output") or world.node_tree.nodes.new("ShaderNodeOutputWorld")
        world.node_tree.links.new(bg.outputs["Background"], out.inputs["Surface"])
    bg.inputs["Color"].default_value = bg_color
    bg.inputs["Strength"].default_value = bg_strength
    # Large area lights behave as reflection cards for product surfaces.
    power_scale = max(0.08, min(30.0, (largest / 0.1) ** 2))
    _area("Key softbox", center + Vector((largest * 1.2, -largest * 1.6, largest * 1.5)), center, 3.5 * power_scale * light_scale, largest * 1.25, (1.0, 0.94, 0.86))
    _area("Fill card", center + Vector((-largest * 1.4, -largest * 0.3, largest * 0.55)), center, 1.4 * power_scale * light_scale, largest * 1.1, (0.78, 0.86, 1.0))
    _area("Rim card", center + Vector((largest * 0.2, largest * 1.3, largest * 1.1)), center, 4.5 * power_scale * light_scale, largest * 0.9, (1.0, 1.0, 1.0))
    if not params.get("transparent"):
        bpy.ops.mesh.primitive_plane_add(size=largest * 12, location=(center.x, center.y, lo.z))
        floor = bpy.context.object
        floor.name = "Studio ground"
        mat = bpy.data.materials.new("Neutral studio ground")
        mat.diffuse_color = floor_color
        mat.use_nodes = True
        bsdf = mat.node_tree.nodes.get("Principled BSDF")
        bsdf.inputs["Base Color"].default_value = floor_color
        bsdf.inputs["Roughness"].default_value = 0.68
        floor.data.materials.append(mat)
    cam_data = bpy.data.cameras.new("Product camera")
    cam = bpy.data.objects.new("Product camera", cam_data)
    scene.collection.objects.link(cam)
    scene.camera = cam
    camera = params.get("camera") or {}
    cam.data.lens = camera.get("lens_mm", 55.0)
    cam.data.clip_start = max(0.001, largest * 0.001)
    cam.data.clip_end = largest * 100
    direction = _camera_direction(camera)
    distance = _fit_camera_distance(lo, hi, center, direction, cam.data.lens,
                                    params["width"], params["height"])
    distance *= camera.get("distance_scale", 1.0)
    cam.location = center + direction * distance
    cam.rotation_euler = (center - cam.location).to_track_quat("-Z", "Y").to_euler()
    return {"camera": cam, "center": center, "largest": largest, "objects": objects,
            "bounds": {"min": list(lo), "max": list(hi), "dimensions_m": list(dims)},
            "lights": [o for o in scene.objects if o.type == "LIGHT"], "ground_z": lo.z,
            "lens_mm": cam.data.lens}


def _measured_motion(setup_data, count, mechanism):
    """Apply only explicit pivot/angle or offset records from mechanism metadata."""
    if not mechanism:
        return []
    if not isinstance(mechanism, dict) or mechanism.get("schema") != 1:
        raise ValueError("mechanism_json must use schema 1")
    if not isinstance(mechanism.get("parts", []), list) or not isinstance(mechanism.get("explode", []), list):
        raise ValueError("mechanism_json parts and explode values must be arrays")
    known = {o.name: o for o in setup_data["objects"]}
    metadata = []
    for part in mechanism.get("parts", []):
        name, pivot, axis, keys = part.get("object"), part.get("pivot_m"), part.get("axis"), part.get("keys")
        if name not in known or not isinstance(pivot, list) or len(pivot) != 3 or not isinstance(axis, list) or len(axis) != 3:
            raise ValueError("measured part requires existing object name, pivot_m, and axis")
        if any(not isinstance(x, (int, float)) or not math.isfinite(x) for x in pivot + axis) or Vector(axis).length < 1e-8:
            raise ValueError("pivot and axis must be finite numeric vectors; axis cannot be zero")
        if not isinstance(keys, list) or len(keys) < 2:
            raise ValueError("measured part requires at least two frame/angle_deg keys")
        key_frames = [key.get("frame") for key in keys]
        if any(isinstance(f, bool) or not isinstance(f, int) or not 0 <= f < count for f in key_frames):
            raise ValueError("measured angle keys need in-range integer frame values")
        if key_frames != sorted(set(key_frames)) or key_frames[0] != 0 or key_frames[-1] != count - 1:
            raise ValueError("measured part keys must span frames 0 through frame_count - 1 in strict order")
        empty = bpy.data.objects.new("Measured pivot - " + name, None)
        bpy.context.scene.collection.objects.link(empty)
        empty.location = Vector(pivot)
        base_axis = Vector(axis).normalized()
        orient = Vector((0, 0, 1)).rotation_difference(base_axis)
        obj = known[name]
        _parent_keep_world(obj, empty)
        for key in keys:
            frame, angle = key.get("frame"), key.get("angle_deg")
            if isinstance(frame, bool) or not isinstance(frame, int) or not 0 <= frame < count or not isinstance(angle, (int, float)) or not math.isfinite(angle):
                raise ValueError("measured angle keys need in-range integer frame and numeric angle_deg")
            empty.rotation_mode = "QUATERNION"
            empty.rotation_quaternion = orient @ Quaternion((0, 0, 1), math.radians(angle))
            empty.keyframe_insert(data_path="rotation_quaternion", frame=frame)
        metadata.append({"object": name, "pivot_m": pivot, "axis": base_axis[:], "keys": keys})
    return metadata


def animate(kind, setup_data, count, mechanism=None, selected_shot=None, camera_override=None,
            output_count=None):
    scene = bpy.context.scene
    cam, center, largest = setup_data["camera"], setup_data["center"], setup_data["largest"]
    scene.frame_start, scene.frame_end = 0, count - 1
    motion = _measured_motion(setup_data, count, mechanism)
    if kind == "turntable":
        distance = (cam.location - center).length
        initial = cam.location - center
        orientation = initial.to_track_quat("-Z", "Y")
        pivot = bpy.data.objects.new("Turntable camera orbit", None)
        scene.collection.objects.link(pivot)
        pivot.location = center
        _parent_keep_world(cam, pivot)
        steps = max(2, min(count, 25))
        for i in range(steps):
            f = round(i * (count - 1) / (steps - 1))
            angle = 2 * math.pi * i / (steps - 1)
            pivot.rotation_euler.z = angle
            pivot.keyframe_insert(data_path="rotation_euler", frame=f)
        setup_data["shots"] = [{"id": "turntable", "start_frame": 0, "end_frame": count,
                                "source_start_frame": 0, "camera": {"lens_mm": cam.data.lens,
                                "framing": "aggregate-bounds", "orbit_degrees": 360}}]
    elif kind == "film":
        # Camera presets define editorial views only; no product motion is invented.
        shot_specs = [("establish", -57.4, 31.4, 55, 1.0),
                      ("top_detail", -59.0, 70.2, 78, 1.0),
                      ("profile", -159.0, 19.9, 85, 1.0),
                      ("hero", 54.1, 27.4, 65, 1.0)]
        boundaries = [round(i * count / len(shot_specs)) for i in range(len(shot_specs) + 1)]
        dims = Vector(setup_data["bounds"]["dimensions_m"])
        lo = Vector(setup_data["bounds"]["min"])
        hi = Vector(setup_data["bounds"]["max"])
        shots = []
        for i, (name, azimuth, elevation, lens, distance_scale) in enumerate(shot_specs):
            start, end = boundaries[i], boundaries[i + 1]
            if end <= start:
                continue
            if selected_shot is not None and name != selected_shot:
                continue
            override = dict(camera_override or {}) if selected_shot is not None else dict(camera_override or {})
            azimuth = override.get("azimuth_deg", azimuth)
            elevation = override.get("elevation_deg", elevation)
            lens = override.get("lens_mm", lens)
            distance_scale = override.get("distance_scale", distance_scale)
            cam_data = bpy.data.cameras.new("Film camera - " + name)
            shot_cam = bpy.data.objects.new("Film camera - " + name, cam_data)
            scene.collection.objects.link(shot_cam)
            cam_data.lens = lens
            target = center + Vector((0, 0, dims.z * 0.16)) if name == "top_detail" else center
            direction = _camera_direction({"azimuth_deg": azimuth, "elevation_deg": elevation})
            dist = _fit_camera_distance(lo, hi, target, direction, lens,
                                        scene.render.resolution_x, scene.render.resolution_y) * distance_scale
            shot_cam.location = target + direction * dist
            shot_cam.rotation_euler = (target - shot_cam.location).to_track_quat("-Z", "Y").to_euler()
            if selected_shot is None:
                scene.timeline_markers.new(name, frame=start).camera = shot_cam
            pivot = bpy.data.objects.new("Film drift pivot - " + name, None)
            scene.collection.objects.link(pivot)
            pivot.location = target
            _parent_keep_world(shot_cam, pivot)
            if end - start > 1:
                for frame, drift in ((start, -3.0), (end - 1, 3.0)):
                    pivot.rotation_euler.z = math.radians(drift)
                    pivot.keyframe_insert(data_path="rotation_euler", frame=frame)
            if i:
                for light in setup_data["lights"]:
                    light.data.keyframe_insert(data_path="energy", frame=start - 1)
                    light.data.energy *= (0.90 if name == "top_detail" else 1.08)
                    light.data.keyframe_insert(data_path="energy", frame=start)
            shot_start, shot_end = (0, output_count or count) if selected_shot is not None else (start, end)
            shots.append({"id": name, "start_frame": shot_start, "end_frame": shot_end,
                          "source_start_frame": 0 if selected_shot is not None else start,
                          "camera": {"lens_mm": lens, "azimuth_deg": azimuth, "elevation_deg": elevation,
                          "distance_scale": distance_scale, "location_m": list(shot_cam.location), "target_m": list(target),
                          "framing": "measured-bounds-detail-crop" if name == "top_detail" else "aggregate-bounds"}})
        scene.camera = bpy.data.objects.get("Film camera - " + selected_shot) if selected_shot else bpy.data.objects.get("Film camera - establish")
        if scene.camera is None:
            raise ValueError("film shot_id is not supported: " + str(selected_shot))
        setup_data["shots"] = shots
    elif kind == "exploded":
        if not mechanism or not mechanism.get("explode"):
            raise ValueError("exploded recipe requires measured mechanism_json explode offsets")
        known = {o.name: o for o in setup_data["objects"]}
        for part in mechanism["explode"]:
            name, offset, start, end = part.get("object"), part.get("offset_m"), part.get("start_frame", 0), part.get("end_frame", count - 1)
            if name not in known or not isinstance(offset, list) or len(offset) != 3 or any(not isinstance(x, (int, float)) or not math.isfinite(x) for x in offset):
                raise ValueError("measured explode entry requires existing object and numeric offset_m")
            if not 0 <= start <= end < count:
                raise ValueError("measured explode frame range must fit job")
            obj = known[name]
            parent = bpy.data.objects.new("Measured explode - " + name, None)
            scene.collection.objects.link(parent)
            _parent_keep_world(obj, parent)
            base = parent.location.copy()
            for frame, factor in ((start, 0), (end, 1)):
                parent.location = base + Vector(offset) * factor
                parent.keyframe_insert(data_path="location", frame=frame)
    for obj in bpy.context.scene.objects:
        if obj.animation_data and obj.animation_data.action:
            for fc in _action_fcurves(obj.animation_data.action):
                for key in fc.keyframe_points:
                    key.interpolation = "LINEAR"
    setup_data["measured_motion"] = motion
    return setup_data.get("shots")
