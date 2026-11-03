"""Product registration schema: GLB-to-knife transform, pivot, bore, tip and archive reference.

A product declares this block as ``registration`` in its product spec JSON. The Blender
module ``product_geometry.py`` consumes it. Without a ``registration`` block the bundled
Fenrir sample registration applies, so legacy jobs keep resolving; that block declares a
source SHA-256, so any other GLB is rejected with a message naming the missing declaration.

Pure Python on purpose (no bpy, no numpy) so the schema is testable without Blender.
"""
import json
import math
import os
import re

from .contract import ContractError

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SAMPLE_SPEC = os.path.join(ROOT, "recipes", "samples", "fenrir-sample-product.json")
SHA256 = re.compile(r"^[0-9a-f]{64}$")
DEFAULT_TIP_BAND_MM = 0.15
DEFAULT_TOLERANCES = {"residual_mm": 0.05, "normal_deg": 0.05, "axis_direction": 0.00005}
DEFAULT_BLADE_MARKER = "BLADE"


def _number(value, name):
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ContractError("registration.%s must be a finite number" % name)
    return float(value)


def _vector(value, count, name):
    if not isinstance(value, list) or len(value) != count:
        raise ContractError("registration.%s must be a list of %d numbers" % (name, count))
    return [_number(v, name) for v in value]


def _sha(value, name):
    if not isinstance(value, str) or not SHA256.fullmatch(value):
        raise ContractError("registration.%s must be a lowercase SHA-256 hex digest" % name)
    return value


def _matrix(value, name):
    if not isinstance(value, list) or len(value) != 4:
        raise ContractError("registration.%s must be a 4x4 matrix" % name)
    rows = [_vector(row, 4, name) for row in value]
    if rows[3] != [0.0, 0.0, 0.0, 1.0]:
        raise ContractError("registration.%s last row must be [0, 0, 0, 1]" % name)
    return rows


def validate_registration(block):
    """Return a normalized copy of a registration block with defaults filled; raise ContractError otherwise."""
    if not isinstance(block, dict):
        raise ContractError("registration must be an object")
    out = {"glb_to_knife": _matrix(block.get("glb_to_knife"), "glb_to_knife"),
           "pivot_knife_mm": _vector(block.get("pivot_knife_mm"), 3, "pivot_knife_mm")}
    bore = block.get("bore")
    if not isinstance(bore, dict):
        raise ContractError("registration.bore must be an object")
    ring = _vector(bore.get("ring_mm"), 2, "bore.ring_mm")
    if not 0 < ring[0] < ring[1]:
        raise ContractError("registration.bore.ring_mm must be an increasing positive pair")
    out["bore"] = {"axis_origin_mm": _vector(bore.get("axis_origin_mm"), 3, "bore.axis_origin_mm"),
                   "axis_datum_mm": _number(bore.get("axis_datum_mm"), "bore.axis_datum_mm"),
                   "ring_mm": ring}
    tip = block.get("tip_band_mm", DEFAULT_TIP_BAND_MM)
    if _number(tip, "tip_band_mm") <= 0:
        raise ContractError("registration.tip_band_mm must be positive")
    out["tip_band_mm"] = float(tip)
    tolerances = dict(DEFAULT_TOLERANCES)
    declared = block.get("tolerances", {})
    if not isinstance(declared, dict) or set(declared) - set(DEFAULT_TOLERANCES):
        raise ContractError("registration.tolerances may contain only: " + ", ".join(sorted(DEFAULT_TOLERANCES)))
    for key, value in declared.items():
        if _number(value, "tolerances." + key) <= 0:
            raise ContractError("registration.tolerances.%s must be positive" % key)
        tolerances[key] = float(value)
    out["tolerances"] = tolerances
    if block.get("source_sha256") is not None:
        out["source_sha256"] = _sha(block["source_sha256"], "source_sha256")
    if block.get("archive_obj_sha256") is not None:
        out["archive_obj_sha256"] = _sha(block["archive_obj_sha256"], "archive_obj_sha256")
    reference = block.get("archive_reference")
    if reference is not None:
        if not isinstance(reference, dict):
            raise ContractError("registration.archive_reference must be an object")
        ref_sha = _sha(reference.get("source_obj_sha256"), "archive_reference.source_obj_sha256")
        if "archive_obj_sha256" in out and ref_sha != out["archive_obj_sha256"]:
            raise ContractError("registration archive_reference belongs to another OBJ")
        out["archive_reference"] = dict(reference)
        out["archive_reference"]["rest_normal"] = _vector(reference.get("rest_normal"), 3, "archive_reference.rest_normal")
        out["archive_reference"]["pivot_world_m"] = _vector(reference.get("pivot_world_m"), 3, "archive_reference.pivot_world_m")
        out["archive_reference"]["tip_world_m"] = _vector(reference.get("tip_world_m"), 3, "archive_reference.tip_world_m")
    part_map = block.get("part_map", {})
    if not isinstance(part_map, dict) or not all(isinstance(k, str) and k and isinstance(v, str) and v for k, v in part_map.items()):
        raise ContractError("registration.part_map must map GLB object names to product part names")
    out["part_map"] = dict(part_map)
    marker = block.get("blade_marker", DEFAULT_BLADE_MARKER)
    if not isinstance(marker, str) or not marker:
        raise ContractError("registration.blade_marker must be a non-empty string")
    out["blade_marker"] = marker
    for key in ("derivation", "knife_space_label"):
        if block.get(key) is not None:
            if not isinstance(block[key], str):
                raise ContractError("registration.%s must be text" % key)
            out[key] = block[key]
    return out


def sample_registration():
    with open(SAMPLE_SPEC, encoding="utf-8") as stream:
        sample = json.load(stream)
    return validate_registration(sample["registration"])


def resolve_registration(spec):
    """Return ``(registration, source)``; source is ``product_spec`` or ``sample_default``."""
    if isinstance(spec, dict) and "registration" in spec:
        return validate_registration(spec["registration"]), "product_spec"
    return sample_registration(), "sample_default"


def check_source_sha(registration, digest, source_label):
    expected = registration.get("source_sha256")
    if expected is None:
        return
    if digest != expected:
        raise ContractError("GLB SHA-256 %s is not the source declared by %s (%s)" % (digest, source_label, expected))
