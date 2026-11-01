"""Optional product review metadata; authoring remains available for drafts."""
import json
import os

from .contract import ContractError


def load_product_spec(job):
    """Load optional product spec, snapshotting it through normal job input hashing."""
    params = job.get("params", {}) if isinstance(job, dict) else {}
    path = params.get("product_spec") if isinstance(params, dict) else None
    if path in (None, ""):
        return None
    if not isinstance(path, str) or not os.path.isabs(path) or not os.path.isfile(path):
        raise ContractError("product_spec must name an existing absolute JSON path")
    try:
        with open(path, encoding="utf-8") as stream:
            spec = json.load(stream)
    except (OSError, UnicodeError, ValueError) as exc:
        raise ContractError("cannot read product spec: " + str(exc))
    if not isinstance(spec, dict):
        raise ContractError("product spec must be a JSON object")
    finishes = spec.get("finishes", {})
    if isinstance(finishes, dict):
        for finish in finishes.values():
            shader = finish.get("shader") if isinstance(finish, dict) else None
            if isinstance(shader, dict) and shader.get("path"):
                shader_path = shader["path"]
                if not isinstance(shader_path, str) or not os.path.isabs(shader_path) or not os.path.isfile(shader_path):
                    raise ContractError("requested finish shader path must exist and be absolute")
    return spec


def review_summary(spec, source_sha256, assignments_match=False, shader_applied=False, overridden=False):
    """Describe review state; malformed/incomplete facts remain unreviewed drafts."""
    spec = spec if isinstance(spec, dict) else {}
    owner = spec.get("owner")
    owner_valid = isinstance(owner, str) and bool(owner.strip())
    identity_valid = all(isinstance(spec.get(k), str) and spec[k].strip() for k in ("product_id", "variant"))
    finishes = spec.get("finishes") if isinstance(spec.get("finishes"), dict) else {}
    parts = spec.get("parts") if isinstance(spec.get("parts"), dict) else {}
    confirmed = spec.get("schema") == 1 and spec.get("confirmed") is True and spec.get("confirmed_by") == owner and owner_valid and identity_valid
    used = {finish for part in parts.values() if isinstance(part, dict) and isinstance(part.get("slots"), dict)
            for finish in part["slots"].values() if isinstance(finish, str)}
    tags, all_approved = {}, bool(used)
    for finish_id in used:
        value = finishes.get(finish_id)
        if not isinstance(value, dict):
            all_approved = False
            continue
        version = value.get("version")
        if isinstance(version, str) and version.strip():
            tags[finish_id] = version
        else:
            all_approved = False
        if value.get("approved") is not True or value.get("approved_by") != owner or not owner_valid:
            all_approved = False
    covered = bool(parts) and all(isinstance(p, dict) and isinstance(p.get("slots"), dict) and p["slots"] and
                                  all(isinstance(k, str) and k.strip() and isinstance(v, str) and v in finishes for k, v in p["slots"].items())
                                  for p in parts.values())
    reviewed = bool(confirmed and all_approved and covered and assignments_match and shader_applied and not overridden)
    return {"reviewed": reviewed, "product_id": spec.get("product_id"), "variant": spec.get("variant"),
            "confirmed": confirmed, "source_sha256": source_sha256, "finish_tags": tags,
            "approved_by": owner if reviewed else None}
