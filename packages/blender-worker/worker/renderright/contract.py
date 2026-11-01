"""Pure-Python worker contract validation and durable file helpers."""
import hashlib
import json
import os
import re
import tempfile
from math import gcd, isfinite

SCHEMA = 1
RECIPES = {"hero", "turntable", "exploded", "film", "probe", "lookdev", "measure", "swatch"}
MAX_DIMENSION = 4096
MAX_FRAMES = 1200
MAX_PIXELS = 16_777_216
# Canonical look profile id; legacy product-specific id accepted as an alias.
LOOK_PROFILE = "product_premium_v1"
LOOK_PROFILE_ALIASES = {"fenrir_premium_v1": LOOK_PROFILE}
JOB_ID = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$")


class ContractError(ValueError):
    pass


def sha256_file(path, chunk=1024 * 1024):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for data in iter(lambda: f.read(chunk), b""):
            h.update(data)
    return h.hexdigest()


def canonical_hash(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")).hexdigest()


def atomic_json(path, value):
    folder = os.path.dirname(os.path.abspath(path))
    os.makedirs(folder, exist_ok=True)
    fd, tmp = tempfile.mkstemp(prefix="." + os.path.basename(path) + ".", suffix=".tmp", dir=folder)
    try:
        with os.fdopen(fd, "w", encoding="utf-8", newline="\n") as f:
            json.dump(value, f, ensure_ascii=False, sort_keys=True, indent=2)
            f.write("\n")
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    finally:
        if os.path.exists(tmp):
            os.unlink(tmp)


def normalize_job(job, worker_root):
    if not isinstance(job, dict) or job.get("schema") != SCHEMA:
        raise ContractError("job schema must be 1")
    job_id = job.get("job_id")
    if not isinstance(job_id, str) or not JOB_ID.fullmatch(job_id):
        raise ContractError("job_id must be 1-128 safe filename characters")
    recipe = job.get("recipe")
    if recipe not in RECIPES:
        raise ContractError("unsupported recipe")
    kind = job.get("kind")
    expected = "authoring" if recipe in ("lookdev", "measure", "swatch") else "image" if recipe in ("hero", "probe") else "video"
    if kind != expected:
        raise ContractError("kind does not match recipe")
    outdir = job.get("output_dir")
    if not isinstance(outdir, str) or not os.path.isabs(outdir):
        raise ContractError("output_dir must be absolute")
    outdir = os.path.realpath(outdir)
    source = job.get("source")
    if recipe == "probe":
        if source not in (None, ""):
            raise ContractError("probe source must be omitted")
    elif recipe == "swatch" and source in (None, ""):
        pass
    elif not isinstance(source, str) or not os.path.isabs(source) or not source.lower().endswith(".glb"):
        raise ContractError("source must be an absolute .glb path")
    elif not os.path.isfile(source):
        raise ContractError("source file does not exist")
    if source:
        source_real = os.path.realpath(source)
        try:
            if os.path.commonpath([outdir, source_real]) == outdir:
                raise ContractError("output_dir cannot contain source GLB")
        except ValueError:
            pass
    params = job.get("params", {})
    if not isinstance(params, dict):
        raise ContractError("params must be an object")
    defaults_path = os.path.join(worker_root, "recipes", "manifest.json")
    with open(defaults_path, encoding="utf-8") as f:
        manifest = json.load(f)
    defaults = manifest["recipes"][recipe]["defaults"]
    p = dict(defaults)
    p.update(params)
    p.setdefault("frame_count", 1 if kind == "image" else 120)
    for name in ("width", "height"):
        v = p.get(name)
        if isinstance(v, bool) or not isinstance(v, int) or not 16 <= v <= MAX_DIMENSION:
            raise ContractError(name + " must be an integer from 16 through 4096")
    if p["width"] * p["height"] > MAX_PIXELS:
        raise ContractError("render pixel count exceeds limit")
    samples = p.get("samples")
    if isinstance(samples, bool) or not isinstance(samples, int) or not 1 <= samples <= 512:
        raise ContractError("samples must be 1 through 512")
    if kind == "authoring":
        allowed = {"width", "height", "samples", "frame_count", "fps", "device", "transparent", "env",
                   "look_profile", "view", "denoise", "source_sha256", "product_spec", "material_recipe", "swatch_finish", "lookdev_settings"}
        if set(params) - allowed:
            raise ContractError("unsupported authoring parameters: " + ", ".join(sorted(set(params) - allowed)))
        if p.get("transparent", False) is not False or p.get("env", "studio_dark") != "studio_dark":
            raise ContractError("authoring requires opaque studio_dark set")
        p["env"] = "studio_dark"
        if p["samples"] < 96:
            raise ContractError("authoring jobs require at least 96 samples")
        look = p.get("look_profile", LOOK_PROFILE)
        if look not in (LOOK_PROFILE, *LOOK_PROFILE_ALIASES):
            raise ContractError("look_profile must be " + LOOK_PROFILE + " (or alias fenrir_premium_v1)")
        p["look_profile"] = LOOK_PROFILE
        if p.get("view", "hero") not in ("hero", "detail", "overview"):
            raise ContractError("view must be hero, detail, or overview")
        p["view"] = p.get("view", "hero")
        if "denoise" in p and p["denoise"] is not False:
            raise ContractError("authoring denoise must be off")
        p["denoise"] = False
    frames = p.get("frame_count")
    if isinstance(frames, bool) or not isinstance(frames, int) or not 1 <= frames <= MAX_FRAMES:
        raise ContractError("frame_count must be 1 through 1200")
    if kind in ("image", "authoring") and frames != 1:
        raise ContractError("image jobs must use frame_count 1")
    fps = p.get("fps")
    if not isinstance(fps, dict) or any(isinstance(fps.get(k), bool) or not isinstance(fps.get(k), int) or not 1 <= fps[k] <= 240000 for k in ("num", "den")):
        raise ContractError("fps must contain positive integer num and den")
    rate = fps["num"] / fps["den"]
    if not 1 <= rate <= 240:
        raise ContractError("fps must be between 1 and 240 frames per second")
    d = gcd(fps["num"], fps["den"])
    p["fps"] = {"num": fps["num"] // d, "den": fps["den"] // d}
    if p.get("device", "auto") not in ("auto", "cpu"):
        raise ContractError("device must be auto or cpu")
    p["device"] = p.get("device", "auto")
    if not isinstance(p.get("transparent", False), bool):
        raise ContractError("transparent must be boolean")
    env = p.get("env", "studio_lightbox") or "studio_lightbox"
    if env not in ("studio_lightbox", "studio_grey", "studio_dark"):
        raise ContractError("env must be studio_lightbox, studio_grey, or studio_dark")
    p["env"] = env
    material_recipe = p.get("material_recipe")
    if material_recipe == "":
        material_recipe = None
        p["material_recipe"] = None
    if material_recipe is not None and not isinstance(material_recipe, (str, dict)):
        raise ContractError("material_recipe must be a preset name or materials mapping")
    if isinstance(material_recipe, str) and material_recipe not in ("anodized_black", "machined_aluminum", "brass", "copper", "titanium_stonewash"):
        raise ContractError("unsupported material_recipe preset")
    if isinstance(material_recipe, dict):
        assignments = material_recipe.get("materials")
        if not isinstance(assignments, dict) or any(v not in ("anodized_black", "machined_aluminum", "brass", "copper", "titanium_stonewash") for v in assignments.values()):
            raise ContractError("material_recipe.materials must map names to supported preset names")
    shot_id = p.get("shot_id")
    if shot_id is not None:
        if recipe != "film" or shot_id not in ("establish", "top_detail", "profile", "hero"):
            raise ContractError("shot_id is supported only for a named film shot")
    prompt = p.get("prompt")
    if prompt is not None and (not isinstance(prompt, str) or len(prompt) > 4000):
        raise ContractError("prompt must be text of at most 4000 characters")
    camera = p.get("camera")
    if camera is not None:
        if not isinstance(camera, dict):
            raise ContractError("camera must be an object")
        allowed_camera = {"lens_mm", "azimuth_deg", "elevation_deg", "distance_scale"}
        if set(camera) - allowed_camera:
            raise ContractError("camera contains unsupported fields")
        bounds = {"lens_mm": (15.0, 200.0), "azimuth_deg": (-360.0, 360.0),
                  "elevation_deg": (-89.0, 89.0), "distance_scale": (0.25, 4.0)}
        for key, value in camera.items():
            if isinstance(value, bool) or not isinstance(value, (int, float)) or not isfinite(value) or not bounds[key][0] <= value <= bounds[key][1]:
                raise ContractError("camera.{} outside supported finite range".format(key))
        p["camera"] = {key: float(value) for key, value in camera.items()}
    if p.get("shot_start_frame") is not None:
        start = p["shot_start_frame"]
        if shot_id is None or isinstance(start, bool) or not isinstance(start, int) or start < 0:
            raise ContractError("shot_start_frame requires film shot_id and a nonnegative integer")
        source_count = p.get("source_frame_count")
        if isinstance(source_count, bool) or not isinstance(source_count, int) or not 1 <= source_count <= MAX_FRAMES or start + frames > source_count:
            raise ContractError("shot source range must fit source_frame_count")
    elif p.get("source_frame_count") is not None:
        raise ContractError("source_frame_count requires shot_start_frame")
    if recipe in ("turntable", "exploded", "film") and not p["transparent"] and (p["width"] % 2 or p["height"] % 2):
        raise ContractError("opaque video dimensions must be even for H.264")
    if recipe == "exploded" and not p.get("mechanism_json"):
        raise ContractError("exploded recipe requires measured mechanism_json metadata")
    normalized = dict(job, output_dir=outdir, params=p)
    if source:
        normalized["source"] = os.path.realpath(source)
    return normalized, manifest


def validate_output_path(root, relative):
    if not isinstance(relative, str) or os.path.isabs(relative):
        raise ContractError("artifact path must be relative")
    full = os.path.realpath(os.path.join(root, relative))
    if os.path.commonpath([os.path.realpath(root), full]) != os.path.realpath(root):
        raise ContractError("artifact path escapes output directory")
    return full
