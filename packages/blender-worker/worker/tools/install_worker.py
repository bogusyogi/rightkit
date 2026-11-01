#!/usr/bin/env python3
"""Install the RenderRight worker files into a dedicated worker directory."""
import argparse
import json
import os
from pathlib import Path
import shutil
import tempfile
import hashlib
import uuid


ROOT = Path(__file__).resolve().parents[1]
FILES = ("render_job.py", "worker-manifest.json", "LICENSE")


def files_sha256(root: Path) -> dict[str, str]:
    inventory = {}
    for directory in ("renderright", "recipes", "evidence"):
        base = root / directory
        if not base.exists():
            continue
        for path in sorted(base.rglob("*")):
            if path.is_file() and "__pycache__" not in path.parts and path.suffix != ".pyc":
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                inventory[path.relative_to(root).as_posix()] = digest
    for name in FILES:
        path = root / name
        if path.is_file():
            inventory[name] = hashlib.sha256(path.read_bytes()).hexdigest()
    return inventory


def package_sha256(inventory: dict[str, str]) -> str:
    payload = json.dumps(inventory, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def ensure_owned_tree(root: Path) -> None:
    """Reject files outside the package's explicit top-level allowlist."""
    allowed = set(FILES) | {"renderright", "recipes", "evidence"}
    unknown = sorted(item.name for item in root.iterdir() if item.name not in allowed)
    if unknown:
        raise ValueError("package contains unowned top-level entries: " + ", ".join(unknown))
    for name in FILES:
        if not (root / name).is_file():
            raise ValueError("package file missing: " + name)
    for name in ("renderright", "recipes", "evidence"):
        path = root / name
        if path.exists() and not path.is_dir():
            raise ValueError("package path is not a directory: " + name)
    for relative in ("renderright", "recipes", "evidence"):
        base = root / relative
        if not base.exists():
            continue
        for path in base.rglob("*"):
            if path.is_symlink():
                raise ValueError("package contains a symlink: " + path.relative_to(root).as_posix())
            if path.is_file() and ("__pycache__" in path.parts or path.suffix == ".pyc"):
                continue
            if not path.is_file() and not path.is_dir():
                raise ValueError("package contains an unsupported entry: " + path.relative_to(root).as_posix())


def validate(root: Path) -> dict:
    manifest_path = root / "worker-manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    if (manifest.get("schema"), manifest.get("name"), manifest.get("version"), manifest.get("protocol")) != (1, "renderright", "0.1.0", 1):
        raise ValueError("unsupported or invalid RenderRight worker manifest")
    if manifest.get("entry") != "render_job.py" or manifest.get("recipes") != "recipes/manifest.json":
        raise ValueError("worker manifest entry paths are invalid")
    for relative in (manifest["entry"], manifest["recipes"], "LICENSE", "renderright/__init__.py", "renderright/contract.py",
                     "renderright/pngcheck.py", "renderright/scene.py", "renderright/worker.py"):
        if not (root / relative).is_file():
            raise ValueError("worker file missing: " + relative)
    blender = manifest.get("blender") or {}
    if blender.get("version") is not None:
        if blender.get("version") != "5.2.2" or not blender.get("qualification"):
            raise ValueError("unsupported Blender qualification pin")
        evidence_path = root / blender["qualification"]
        if not evidence_path.is_file():
            raise ValueError("pinned Blender qualification evidence is missing")
        evidence = json.loads(evidence_path.read_text(encoding="utf-8"))
        if evidence.get("status") != "qualified" or not str(evidence.get("blender", {}).get("version", "")).startswith("5.2.2"):
            raise ValueError("Blender qualification evidence does not match worker pin")
        expected = evidence.get("worker", {}).get("files_sha256", {})
        for relative, digest in expected.items():
            path = root / relative
            if not path.is_file() or hashlib.sha256(path.read_bytes()).hexdigest() != digest:
                raise ValueError("qualification source hash mismatch: " + relative)
        if hashlib.sha256((root / manifest["recipes"]).read_bytes()).hexdigest() != evidence.get("recipe", {}).get("sha256"):
            raise ValueError("qualification recipe hash mismatch")
    return manifest


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--destination", required=True, type=Path, help="dedicated worker directory")
    parser.add_argument("--upgrade", action="store_true", help="preserve a verified prior package, then atomically upgrade it")
    args = parser.parse_args()
    source = ROOT.resolve()
    validate(source)
    destination = args.destination.expanduser().resolve()
    if destination == source or source in destination.parents or destination in source.parents:
        parser.error("destination must be outside this source checkout")
    if destination.exists() and not destination.is_dir():
        parser.error("destination exists and is not a directory")
    if destination.exists():
        existing_entries = list(destination.iterdir())
        if existing_entries:
            try:
                ensure_owned_tree(destination)
                existing_manifest = validate(destination)
            except (OSError, ValueError, KeyError, json.JSONDecodeError):
                parser.error("destination is nonempty, contains unowned entries, or is not a valid RenderRight package; refusing overwrite")
            old_inventory = files_sha256(destination)
            source_inventory = files_sha256(source)
            if (existing_manifest == json.loads((source / "worker-manifest.json").read_text(encoding="utf-8"))
                    and old_inventory == source_inventory):
                print(json.dumps({"installed": str(destination), "version": "0.1.0", "protocol": 1,
                                  "package_sha256": package_sha256(old_inventory), "status": "already_current"}, sort_keys=True))
                return 0
            if not args.upgrade:
                parser.error("destination contains a differing verified RenderRight package; pass --upgrade to preserve it and upgrade")
    destination.parent.mkdir(parents=True, exist_ok=True)
    if destination.exists() and not any(destination.iterdir()):
        destination.rmdir()
    stage = Path(tempfile.mkdtemp(prefix=".renderright-install-", dir=str(destination.parent)))
    history = None
    try:
        for name in FILES:
            shutil.copy2(source / name, stage / name)
        shutil.copytree(source / "renderright", stage / "renderright", ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
        shutil.copytree(source / "recipes", stage / "recipes")
        if (source / "evidence").is_dir():
            shutil.copytree(source / "evidence", stage / "evidence")
        ensure_owned_tree(stage)
        validate(stage)
        new_inventory = files_sha256(stage)
        if destination.exists() and any(destination.iterdir()):
            old_manifest = validate(destination)
            old_inventory = files_sha256(destination)
            old_code = old_manifest.get("blender", {}).get("qualification")
            old_evidence = json.loads((destination / old_code).read_text(encoding="utf-8")) if old_code else {}
            old_identity = old_evidence.get("worker", {}).get("code_sha256")
            if not old_identity:
                raise ValueError("verified prior package has no source identity to preserve")
            history_root = destination.parent / "renderright-history"
            history_root.mkdir(parents=True, exist_ok=True)
            history = history_root / old_identity[:16]
            if history.exists():
                if files_sha256(history) != old_inventory:
                    raise ValueError("history destination exists with differing content; refusing upgrade")
                raise ValueError("prior package identity is already archived; refusing to replace canonical package")
            destination.rename(history)
        try:
            os.replace(stage, destination)
            stage = None
            validate(destination)
            if files_sha256(destination) != new_inventory:
                raise ValueError("installed package inventory differs from staged package")
        except Exception:
            if history is not None and history.exists():
                if destination.exists():
                    failed = destination.parent / (".renderright-failed-" + uuid.uuid4().hex[:12])
                    destination.rename(failed)
                history.rename(destination)
                history = None
            raise
    finally:
        if stage is not None:
            shutil.rmtree(stage, ignore_errors=True)
    print(json.dumps({"installed": str(destination), "version": "0.1.0", "protocol": 1,
                      "package_sha256": package_sha256(new_inventory),
                      "preserved_prior": str(history) if history else None}, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
