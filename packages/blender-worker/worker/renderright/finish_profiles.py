"""Data-driven product finish profiles, built in knife-space metres.

Draft materials only: nothing here is an approved product finish. Nothing
product-specific lives in this module. A product declares its finish profile in
its product spec JSON, under ``finish_profiles.<id>``:

    {
      "finish_roles": {"<CAD object name>": "<role>"},   # unlisted objects get "hardware"
      "pivot": {"object": "<CAD object name>", "xy_m": [x, y]},   # optional
      "detail_parts": ["<CAD object name>", ...],          # optional crop for frame_part 1
      "parameters": {<every key in REQUIRED_PARAMETERS>}
    }

A CAD object whose name contains ``BLADE`` is the blade by convention. Every
feature size is stated in metres so millimetre stonewash mottle & scratch grain
keep their physical scale. The legacy id ``ti_stonewash_v1`` is an alias that
loads the sample values in ``recipes/samples/fenrir-sample-product.json``.
"""
import json
import math
import os

import bpy
from mathutils import Vector

from .contract import ContractError

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SAMPLE_SPEC = os.path.join(ROOT, "recipes", "samples", "fenrir-sample-product.json")
BASE_PROFILE = "codex_draft"
LEGACY_ALIASES = {"ti_stonewash_v1": "sample_ti_stonewash"}
BLADE_FINISHES = ("stonewash", "black_coated")
ROLES = ("blade", "scale", "liner", "backspacer", "clip", "blanking_plate", "stop_pin", "hardware")
REQUIRED_PARAMETERS = (
    "ti_r", "ti_g", "ti_b", "ti_roughness", "ti_cell_mm", "ti_scratch_mm", "ti_scratch_density",
    "ti_bevel_mm", "ti_mottle_bright", "ti_mottle_rough", "ti_relief", "ti_anisotropy",
    "blade_value", "blade_roughness", "blade_cell_mm", "blade_scratch_density", "blade_mottle_bright",
    "blade_mottle_rough", "blade_anisotropy", "blade_relief", "blade_fine_relief", "blade_fine_scratch",
    "edge_bevel_mm",
)


def part_role(name, finish_roles):
    """Role of a CAD object: blade by name convention, otherwise from the profile's finish_roles."""
    if "BLADE" in name:
        return "blade"
    return finish_roles.get(name, "hardware")


def _pair(value):
    return isinstance(value, list) and len(value) == 2 and all(
        not isinstance(v, bool) and isinstance(v, (int, float)) and math.isfinite(v) for v in value)


def resolve(profile_id, spec, overrides=None):
    """Return the resolved finish profile, or None for the codex draft (no finish profile).

    ``overrides`` is the job's ``lookdev_settings``; any REQUIRED_PARAMETERS key it names wins
    over the profile value, as before.
    """
    if profile_id == BASE_PROFILE:
        return None
    if profile_id in LEGACY_ALIASES:
        print(json.dumps({"event": "FINISH_PROFILE_DEPRECATED", "profile": profile_id,
                          "replacement": LEGACY_ALIASES[profile_id], "loads": "recipes/samples/fenrir-sample-product.json",
                          "note": "declare a finish_profiles entry in the product spec instead"}), flush=True)
        with open(SAMPLE_SPEC, encoding="utf-8") as stream:
            spec = json.load(stream)
        profile_id = LEGACY_ALIASES[profile_id]
    declared = spec.get("finish_profiles") if isinstance(spec, dict) else None
    if not isinstance(declared, dict) or not isinstance(declared.get(profile_id), dict):
        raise ContractError("finish_profile %r is not declared in the product spec (finish_profiles)" % profile_id)
    decl = declared[profile_id]
    params = decl.get("parameters")
    if not isinstance(params, dict):
        raise ContractError("finish profile %s needs a parameters object" % profile_id)
    missing = [key for key in REQUIRED_PARAMETERS if key not in params]
    if missing:
        raise ContractError("finish profile %s lacks parameters: %s" % (profile_id, ", ".join(missing)))
    overrides = overrides or {}
    values = {}
    for key in REQUIRED_PARAMETERS:
        raw = overrides[key] if key in overrides else params[key]
        if isinstance(raw, bool) or not isinstance(raw, (int, float)) or not math.isfinite(raw):
            raise ContractError("finish parameter must be finite: " + key)
        values[key] = float(raw)
    roles = decl.get("finish_roles", {})
    if not isinstance(roles, dict) or any(role not in ROLES for role in roles.values()):
        raise ContractError("finish_roles must map part names to one of: " + ", ".join(ROLES))
    pivot = decl.get("pivot")
    pivot_object, pivot_xy = None, None
    if pivot is not None:
        if not isinstance(pivot, dict) or not isinstance(pivot.get("object"), str) or not _pair(pivot.get("xy_m")):
            raise ContractError("pivot must be {object: name, xy_m: [x, y]}")
        pivot_object, pivot_xy = pivot["object"], tuple(float(v) for v in pivot["xy_m"])
    detail = decl.get("detail_parts", [])
    if not isinstance(detail, list) or not all(isinstance(name, str) for name in detail):
        raise ContractError("detail_parts must be a list of part names")
    return {"id": profile_id, "parameters": values, "finish_roles": dict(roles),
            "pivot_object": pivot_object, "pivot_xy": pivot_xy, "detail_parts": list(detail)}


def _new(name):
    mat = bpy.data.materials.new(name)
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    p = nt.nodes.new("ShaderNodeBsdfPrincipled")
    nt.links.new(p.outputs["BSDF"], out.inputs["Surface"])
    return mat, nt, p


def _math(nt, op, a=None, b=None, c=None, clamp=False):
    n = nt.nodes.new("ShaderNodeMath")
    n.operation = op
    n.use_clamp = clamp
    for i, v in enumerate((a, b, c)):
        if isinstance(v, (int, float)):
            n.inputs[i].default_value = v
        elif v is not None:
            nt.links.new(v, n.inputs[i])
    return n.outputs[0]


def _map_range(nt, value, from_min, from_max, to_min, to_max, smooth=False):
    n = nt.nodes.new("ShaderNodeMapRange")
    n.clamp = True
    if smooth:
        n.interpolation_type = "SMOOTHSTEP"
    n.inputs["From Min"].default_value = from_min
    n.inputs["From Max"].default_value = from_max
    n.inputs["To Min"].default_value = to_min
    n.inputs["To Max"].default_value = to_max
    nt.links.new(value, n.inputs["Value"])
    return n.outputs["Result"]


def _scaled_coords(nt, metres_per_unit, stretch=(1, 1, 1), angle=0.0):
    """Object coordinates (knife-space metres) rotated about Z then scaled so 1 unit = metres_per_unit."""
    tc = nt.nodes.new("ShaderNodeTexCoord")
    vec = tc.outputs["Object"]
    if angle:
        rot = nt.nodes.new("ShaderNodeVectorRotate")
        rot.rotation_type = "AXIS_ANGLE"
        rot.inputs["Axis"].default_value = (0, 0, 1)
        rot.inputs["Angle"].default_value = angle
        nt.links.new(vec, rot.inputs["Vector"])
        vec = rot.outputs["Vector"]
    scale = nt.nodes.new("ShaderNodeVectorMath")
    scale.operation = "MULTIPLY"
    scale.inputs[1].default_value = tuple(s / metres_per_unit for s in stretch)
    nt.links.new(vec, scale.inputs[0])
    return scale.outputs[0]


def _noise(nt, vec, detail=2.0, roughness=0.5):
    n = nt.nodes.new("ShaderNodeTexNoise")
    n.inputs["Scale"].default_value = 1.0
    n.inputs["Detail"].default_value = detail
    n.inputs["Roughness"].default_value = roughness
    nt.links.new(vec, n.inputs["Vector"])
    return n.outputs["Fac"]


def _mottle(nt, cell_m):
    """Tumbled-media mottle: smooth Voronoi cells of cell_m broken by noise, 0..1 around 0.5."""
    vec = _scaled_coords(nt, cell_m)
    vor = nt.nodes.new("ShaderNodeTexVoronoi")
    vor.feature = "SMOOTH_F1"
    vor.inputs["Scale"].default_value = 1.0
    vor.inputs["Smoothness"].default_value = 0.85
    vor.inputs["Randomness"].default_value = 1.0
    nt.links.new(vec, vor.inputs["Vector"])
    broad = _noise(nt, _scaled_coords(nt, cell_m * 3.0), detail=3, roughness=0.55)
    cells = _map_range(nt, vor.outputs["Distance"], 0.0, 0.9, 0.0, 1.0)
    return _math(nt, "MULTIPLY_ADD", _math(nt, "SUBTRACT", cells, 0.5), 0.6, broad)


def _scratches(nt, length_m, width_m, density, angles=(0.2, 1.1, 2.3)):
    """Sparse long scratches in several directions; returns 0..1 mask."""
    mask = None
    for angle in angles:
        vec = _scaled_coords(nt, width_m, stretch=(1.0, width_m / length_m, 1.0), angle=angle)
        fac = _noise(nt, vec, detail=1.5, roughness=0.7)
        lo = 1.0 - density
        one = _map_range(nt, fac, lo, min(0.995, lo + 0.08), 0.0, 1.0)
        mask = one if mask is None else _math(nt, "MAXIMUM", mask, one)
    return mask


def _micro(nt, size_m):
    return _noise(nt, _scaled_coords(nt, size_m), detail=2, roughness=0.6)


def _edge_wear(nt, bevel_normal):
    """Bright burnished chamfer mask from bevel-vs-true normal disagreement."""
    geo = nt.nodes.new("ShaderNodeNewGeometry")
    dot = nt.nodes.new("ShaderNodeVectorMath")
    dot.operation = "DOT_PRODUCT"
    nt.links.new(geo.outputs["True Normal"], dot.inputs[0])
    nt.links.new(bevel_normal, dot.inputs[1])
    off = _math(nt, "SUBTRACT", 1.0, dot.outputs["Value"])
    return _map_range(nt, off, 0.01, 0.12, 0.0, 1.0, smooth=True)


def _bump(nt, height, normal, strength, distance_m):
    b = nt.nodes.new("ShaderNodeBump")
    b.inputs["Strength"].default_value = strength
    b.inputs["Distance"].default_value = distance_m
    nt.links.new(height, b.inputs["Height"])
    if normal is not None:
        nt.links.new(normal, b.inputs["Normal"])
    return b.outputs["Normal"]


def _bevel(nt, radius_m):
    b = nt.nodes.new("ShaderNodeBevel")
    b.samples = 6
    b.inputs["Radius"].default_value = radius_m
    return b.outputs["Normal"]


def _tint(nt, color, factor, bright):
    """color * (1 + (factor-0.5)*bright) as RGB socket."""
    gain = _math(nt, "MULTIPLY_ADD", _math(nt, "SUBTRACT", factor, 0.5), bright, 1.0)
    mix = nt.nodes.new("ShaderNodeMixRGB")
    mix.blend_type = "MULTIPLY"
    mix.inputs[0].default_value = 1.0
    mix.inputs[1].default_value = (*color, 1)
    nt.links.new(gain, mix.inputs[2])
    return mix.outputs[0]


def _stonewash(name, color, rough, cell_m, scratch_len_m, scratch_w_m, scratch_density,
               bevel_m, mottle_bright, mottle_rough, scratch_depth_m, aniso=0.0, tangent=None, relief=0.15,
               edge_line=None, fine_relief=0.0, fine_scratch=0.4):
    cfg_relief = (relief,)
    mat, nt, p = _new(name)
    p.inputs["Metallic"].default_value = 1.0
    p.inputs["Coat Weight"].default_value = 0.0
    mottle = _math(nt, "MULTIPLY_ADD", _math(nt, "SUBTRACT", _mottle(nt, cell_m * 3.0), 0.5), 0.6, _mottle(nt, cell_m))
    fine_cells = _mottle(nt, 0.00045)
    # Scratch populations: long coarse, short medium & dense fine (0.2-0.5 mm), random orientations.
    long_s = _scratches(nt, scratch_len_m, scratch_w_m, scratch_density, angles=(0.2, 1.1, 2.3, 2.9))
    short_s = _scratches(nt, 0.0012, 0.00009, min(0.9, scratch_density * 2.5), angles=(0.6, 1.7, 2.6, 0.1))
    fine_s = _scratches(nt, 0.0004, 0.00005, min(0.9, scratch_density * 3.0), angles=(0.4, 1.3, 2.0, 2.7, 0.9))
    scratch = _math(nt, "MAXIMUM", long_s, _math(nt, "MULTIPLY", short_s, 0.85))
    scratch = _math(nt, "MAXIMUM", scratch, _math(nt, "MULTIPLY", fine_s, fine_scratch))
    micro = _micro(nt, 0.00012)
    micro_amp = 0.02 if aniso else 0.06
    bevel = _bevel(nt, bevel_m)
    wear = _edge_wear(nt, bevel)
    if edge_line is not None:
        edge_line = _edge_line_mask(nt, edge_line[1], edge_line[2])
    # Colour: mottle brightens tumbled peaks, worn chamfers burnish brighter.
    base = _tint(nt, color, mottle, mottle_bright)
    bright = nt.nodes.new("ShaderNodeMixRGB")
    bright.blend_type = "MIX"
    nt.links.new(wear, bright.inputs[0])
    nt.links.new(base, bright.inputs[1])
    bright.inputs[2].default_value = (*(min(1.0, c * 1.35) for c in color), 1)
    streak = nt.nodes.new("ShaderNodeMixRGB")
    streak.blend_type = "MIX"
    nt.links.new(_math(nt, "MULTIPLY", scratch, 0.5), streak.inputs[0])
    nt.links.new(bright.outputs[0], streak.inputs[1])
    streak.inputs[2].default_value = (*(min(1.0, c * 1.25) for c in color), 1)
    if edge_line is not None:
        edge_bright = nt.nodes.new("ShaderNodeMixRGB")
        nt.links.new(edge_line, edge_bright.inputs[0])
        nt.links.new(streak.outputs[0], edge_bright.inputs[1])
        edge_bright.inputs[2].default_value = (*(min(1.0, c * 1.35) for c in color), 1)
        nt.links.new(edge_bright.outputs[0], p.inputs["Base Color"])
    else:
        nt.links.new(streak.outputs[0], p.inputs["Base Color"])
    # Roughness: base, +- mottle, micro sheen breakup, scratches rougher, chamfers smoother.
    r = _math(nt, "MULTIPLY_ADD", _math(nt, "SUBTRACT", mottle, 0.5), mottle_rough, rough)
    r = _math(nt, "MULTIPLY_ADD", _math(nt, "SUBTRACT", micro, 0.5), micro_amp, r)
    r = _math(nt, "MULTIPLY_ADD", scratch, -0.26, r)
    r = _math(nt, "MULTIPLY_ADD", wear, -0.12, r)
    if edge_line is not None:
        r = _math(nt, "MULTIPLY_ADD", edge_line, -0.3, r)
    r = _math(nt, "MAXIMUM", r, 0.12, clamp=True)
    nt.links.new(r, p.inputs["Roughness"])
    # Normals: bevel -> broad mottle relief -> scratch grooves -> micro.
    n = _bump(nt, mottle, bevel, cfg_relief[0], 0.00003)
    n = _bump(nt, fine_cells, n, fine_relief, 0.000012)
    n = _bump(nt, scratch, n, 0.8, scratch_depth_m)
    n = _bump(nt, micro, n, 0.15 if not aniso else 0.03, 0.000002)
    nt.links.new(n, p.inputs["Normal"])
    if aniso and tangent is not None:
        p.inputs["Anisotropic"].default_value = aniso
        p.inputs["Tangent"].default_value = tangent
    return mat


def ti_stonewash(cfg, tangent=None):
    # Satin metal with broad facet gradients, anisotropic along the handle, sparse 3-10 mm scratch streaks.
    return _stonewash("DRAFT Ti stonewash v1", (cfg["ti_r"], cfg["ti_g"], cfg["ti_b"]),
                      cfg["ti_roughness"], cfg["ti_cell_mm"] / 1000, cfg["ti_scratch_mm"] / 1000,
                      0.00015, cfg["ti_scratch_density"], cfg["ti_bevel_mm"] / 1000,
                      cfg["ti_mottle_bright"], cfg["ti_mottle_rough"], 0.00001, relief=cfg["ti_relief"],
                      aniso=cfg["ti_anisotropy"], tangent=tangent)


def _edge_line_mask(nt, edge_y, width_m):
    """1 within width_m of the straight cutting edge (object-space Y), fading to 0."""
    tc = nt.nodes.new("ShaderNodeTexCoord")
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(tc.outputs["Object"], sep.inputs["Vector"])
    return _map_range(nt, sep.outputs["Y"], edge_y + width_m, edge_y + width_m * 0.4, 0.0, 1.0, smooth=True)


def steel_stonewash(cfg, tangent, edge_y=None):
    mat = _stonewash("DRAFT stonewash steel v1", (cfg["blade_value"],) * 3, cfg["blade_roughness"],
                     cfg["blade_cell_mm"] / 1000, 0.005, 0.0001, cfg["blade_scratch_density"], 0.00005,
                     cfg["blade_mottle_bright"], cfg["blade_mottle_rough"], 0.000008,
                     aniso=cfg["blade_anisotropy"], tangent=tangent, relief=cfg["blade_relief"],
                     fine_relief=cfg["blade_fine_relief"], fine_scratch=cfg["blade_fine_scratch"],
                     edge_line=None if edge_y is None else ("edge", edge_y, cfg["edge_bevel_mm"] / 1000))
    return mat


def black_coated_blade(cfg):
    mat, nt, p = _new("DRAFT black coated blade v1")
    p.inputs["Base Color"].default_value = (0.018, 0.018, 0.02, 1)
    p.inputs["Metallic"].default_value = 0.25
    p.inputs["Specular IOR Level"].default_value = 0.45
    mottle = _mottle(nt, 0.0010)
    micro = _micro(nt, 0.0001)
    r = _math(nt, "MULTIPLY_ADD", _math(nt, "SUBTRACT", mottle, 0.5), 0.12, cfg["blade_roughness"] + 0.12, clamp=True)
    nt.links.new(r, p.inputs["Roughness"])
    n = _bump(nt, mottle, _bevel(nt, 0.00005), 0.3, 0.00002)
    n = _bump(nt, micro, n, 0.2, 0.000002)
    nt.links.new(n, p.inputs["Normal"])
    return mat


def pivot_hardware(pivot_xy):
    """Flat black pivot screw with three engraved concentric rings around the pivot axis (xy in knife metres)."""
    mat, nt, p = _new("DRAFT engraved pivot v1")
    p.inputs["Base Color"].default_value = (0.03, 0.03, 0.032, 1)
    p.inputs["Metallic"].default_value = 0.8
    p.inputs["Roughness"].default_value = 0.5
    p.inputs["Coat Weight"].default_value = 0.0
    tc = nt.nodes.new("ShaderNodeTexCoord")
    sep = nt.nodes.new("ShaderNodeSeparateXYZ")
    nt.links.new(tc.outputs["Object"], sep.inputs["Vector"])
    dx = _math(nt, "SUBTRACT", sep.outputs["X"], pivot_xy[0])
    dy = _math(nt, "SUBTRACT", sep.outputs["Y"], pivot_xy[1])
    r = None
    for ang in (0.0, math.pi / 3, 2 * math.pi / 3):
        proj = _math(nt, "ABSOLUTE", _math(nt, "ADD", _math(nt, "MULTIPLY", dx, math.cos(ang)), _math(nt, "MULTIPLY", dy, math.sin(ang))))
        r = proj if r is None else _math(nt, "MAXIMUM", r, proj)
    # three concentric rings, 0.35 mm wide, at 1.6, 2.6 and 3.6 mm from the pivot axis
    rings = None
    for radius in (0.0016, 0.0026, 0.0036):
        d = _math(nt, "ABSOLUTE", _math(nt, "SUBTRACT", r, radius))
        ring = _map_range(nt, d, 0.00022, 0.00012, 0.0, 1.0, smooth=True)
        rings = ring if rings is None else _math(nt, "MAXIMUM", rings, ring)
    groove = _math(nt, "MULTIPLY", rings, -1.0)
    n = _bump(nt, groove, _bevel(nt, 0.00015), 0.9, 0.00015)
    n = _bump(nt, _micro(nt, 0.0002), n, 0.2, 0.000004)
    nt.links.new(n, p.inputs["Normal"])
    rr = _math(nt, "MULTIPLY_ADD", rings, 0.15, 0.5, clamp=True)
    nt.links.new(rr, p.inputs["Roughness"])
    return mat


def black_hardware():
    mat, nt, p = _new("DRAFT black hardware v1")
    p.inputs["Base Color"].default_value = (0.03, 0.03, 0.032, 1)
    p.inputs["Metallic"].default_value = 0.8
    p.inputs["Roughness"].default_value = 0.42
    p.inputs["Coat Weight"].default_value = 0.0
    n = _bump(nt, _micro(nt, 0.0002), _bevel(nt, 0.00025), 0.3, 0.000006)
    nt.links.new(n, p.inputs["Normal"])
    return mat


def black_painted():
    mat, nt, p = _new("DRAFT black painted clip v1")
    p.inputs["Base Color"].default_value = (0.012, 0.012, 0.013, 1)
    p.inputs["Metallic"].default_value = 0.0
    p.inputs["Roughness"].default_value = 0.42
    p.inputs["Specular IOR Level"].default_value = 0.5
    n = _bump(nt, _micro(nt, 0.00015), _bevel(nt, 0.00025), 0.3, 0.000004)
    nt.links.new(n, p.inputs["Normal"])
    return mat


def black_plastic():
    mat, nt, p = _new("DRAFT black plastic backspacer v1")
    p.inputs["Base Color"].default_value = (0.02, 0.02, 0.021, 1)
    p.inputs["Metallic"].default_value = 0.0
    p.inputs["Roughness"].default_value = 0.55
    p.inputs["Specular IOR Level"].default_value = 0.35
    n = _bump(nt, _micro(nt, 0.0008), _bevel(nt, 0.0003), 0.08, 0.000003)
    nt.links.new(n, p.inputs["Normal"])
    return mat


def bead_blast_steel():
    mat, nt, p = _new("DRAFT bead blast liner v1")
    p.inputs["Base Color"].default_value = (0.40, 0.395, 0.385, 1)
    p.inputs["Metallic"].default_value = 1.0
    p.inputs["Roughness"].default_value = 0.52
    nt.links.new(_bevel(nt, 0.0004), p.inputs["Normal"])
    return mat


def apply_profile(meshes, blade, knife_matrix, cfg, finish):
    """Replace every slot by its part role from the resolved profile; returns the effective finish table."""
    if cfg["blade_finish"] not in BLADE_FINISHES:
        raise ContractError("unknown blade_finish")
    P = finish["parameters"]
    tangent = knife_matrix.to_3x3() @ Vector((1, 0, 0))
    edge_y = min(v.co.y for v in blade.data.vertices)
    blade_mat = steel_stonewash(P, tangent, edge_y) if cfg["blade_finish"] == "stonewash" else black_coated_blade(P)
    by_role = {"blade": blade_mat, "scale": ti_stonewash(P, tangent), "liner": bead_blast_steel(),
               "backspacer": black_plastic(), "clip": black_painted(), "blanking_plate": black_hardware(),
               "stop_pin": bead_blast_steel(), "hardware": black_hardware()}
    finish_table = {}
    pivot_mat = pivot_hardware(finish["pivot_xy"]) if finish["pivot_xy"] is not None else None
    for obj in meshes:
        role = "blade" if obj is blade else part_role(obj.name, finish["finish_roles"])
        mat = pivot_mat if pivot_mat is not None and obj.name == finish["pivot_object"] else by_role[role]
        for i in range(len(obj.data.materials)):
            obj.data.materials[i] = mat
        obj.cycles.shadow_terminator_geometry_offset = 0.1
        finish_table[obj.name] = {"role": role, "materials": [mat.name] * len(obj.data.materials)}
    return finish_table
