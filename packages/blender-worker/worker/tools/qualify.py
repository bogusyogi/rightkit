#!/usr/bin/env python3
"""Run one small Blender probe and write machine-readable qualification evidence."""
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import subprocess
import sys


ROOT = Path(__file__).resolve().parents[1]
PINNED_BLENDER = "5.2.2"
BLENDER_ARCHIVE_SHA256 = "3849d17a682cba006075aaa3f3597ecb5c9c30ec31035b2e092c53e40679b535"
BLENDER_SHA256_LISTING = "https://download.blender.org/release/Blender5.2/blender-5.2.2.sha256"


def run(args, env=None):
    creationflags = getattr(subprocess, "CREATE_NO_WINDOW", 0) if os.name == "nt" else 0
    return subprocess.run(args, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True,
                          encoding="utf-8", errors="replace", env=env, creationflags=creationflags)


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def worker_code_identity(worker: Path) -> dict:
    paths = ("render_job.py", "renderright/__init__.py", "renderright/contract.py",
             "renderright/pngcheck.py", "renderright/scene.py", "renderright/worker.py",
             "renderright/rest_pose.py")
    optional = ("renderright/authoring.py", "renderright/premium_look.py", "renderright/fenrir_geometry.py", "renderright/product_geometry.py", "renderright/product_registration.py",
                "renderright/product_review.py", "renderright/approved_materials.py", "renderright/swatch.py",
                "recipes/samples/fenrir-sample-product.json")
    paths += tuple(relative for relative in optional if (worker / relative).is_file())
    files = {relative: sha256(worker / relative) for relative in paths}
    canonical = json.dumps(files, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return {"files_sha256": files, "worker_code_sha256": hashlib.sha256(canonical).hexdigest()}


def canonical_hash(value: dict) -> str:
    payload = json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    return hashlib.sha256(payload).hexdigest()


def contained(root: Path, relative: str) -> Path:
    candidate = (root / relative).resolve()
    if candidate != root and root not in candidate.parents:
        raise ValueError("artifact path escapes output directory: " + relative)
    return candidate


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--blender", required=True, type=Path, help="exact Blender executable to qualify")
    parser.add_argument("--worker", type=Path, default=ROOT, help="installed RenderRight worker root")
    parser.add_argument("--output", required=True, type=Path, help="new or reusable qualification output directory")
    parser.add_argument("--ffmpeg", type=Path, help="optional FFmpeg executable whose version is recorded")
    parser.add_argument("--blender-archive", type=Path, help="optional official portable archive to hash against Blender's published listing")
    parser.add_argument("--samples", type=int, default=1)
    parser.add_argument("--device", choices=("auto", "cpu"), default="auto")
    parser.add_argument("--require-gpu", action="store_true", help="fail unless probe confirms GPU use")
    args = parser.parse_args()
    blender = args.blender.resolve(strict=True)
    worker = args.worker.resolve(strict=True)
    output = args.output.resolve()
    if not blender.is_file() or not worker.is_dir():
        parser.error("Blender must be a file and worker must be a directory")
    if not 1 <= args.samples <= 512:
        parser.error("samples must be between 1 and 512")
    manifest = json.loads((worker / "worker-manifest.json").read_text(encoding="utf-8"))
    if (manifest.get("schema"), manifest.get("name"), manifest.get("version"), manifest.get("protocol")) != (1, "renderright", "0.1.0", 1):
        parser.error("worker manifest must declare RenderRight 0.1.0 protocol 1")
    if manifest.get("entry") != "render_job.py" or manifest.get("recipes") != "recipes/manifest.json":
        parser.error("worker manifest entry paths do not match protocol")
    entry = worker / manifest["entry"]
    if not entry.is_file() or not (worker / manifest["recipes"]).is_file():
        parser.error("worker entry or recipe manifest is missing")
    code_identity_before = worker_code_identity(worker)
    recipe_sha256_before = sha256(worker / manifest["recipes"])

    version = run([str(blender), "--version"])
    if version.returncode != 0:
        parser.error("Blender --version failed: " + version.stdout[-1000:])
    match = re.search(r"(?m)^Blender\s+(\d+\.\d+\.\d+)\b", version.stdout)
    if not match or match.group(1) != PINNED_BLENDER:
        parser.error("expected Blender " + PINNED_BLENDER + "; got " + (match.group(1) if match else version.stdout[:120].strip()))

    output.mkdir(parents=True, exist_ok=True)
    if (output / "job.json").exists() or (output / "job").exists():
        parser.error("qualification output already contains a job; choose a new directory")
    job_id = "qualification-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    job_output = output / "job"
    job = {"schema": 1, "job_id": job_id, "kind": "image", "recipe": "probe",
           "output_dir": str(job_output), "params": {"width": 64, "height": 64, "samples": args.samples,
           "frame_count": 1, "fps": {"num": 1, "den": 1}, "device": args.device, "transparent": False}}
    job_path = output / "job.json"
    job_path.write_text(json.dumps(job, indent=2) + "\n", encoding="utf-8")
    env = os.environ.copy()
    if args.ffmpeg:
        ffmpeg = args.ffmpeg.resolve(strict=True)
    command = [str(blender), "-b", "--factory-startup", "--python-exit-code", "1", "--python", str(entry), "--", str(job_path)]
    result = run(command, env=env)
    (output / "blender.log").write_text(result.stdout, encoding="utf-8")
    if result.returncode != 0:
        raise RuntimeError("Blender probe failed with exit code {} (see {})".format(result.returncode, output / "blender.log"))
    code_identity_after = worker_code_identity(worker)
    recipe_sha256_after = sha256(worker / manifest["recipes"])
    if code_identity_after != code_identity_before or recipe_sha256_after != recipe_sha256_before:
        raise RuntimeError("worker or recipe sources changed during qualification; result is not qualified")

    result_path = job_output / "result-manifest.json"
    probe_path = job_output / "probe.json"
    result_manifest = json.loads(result_path.read_text(encoding="utf-8"))
    probe = json.loads(probe_path.read_text(encoding="utf-8"))
    progress = json.loads((job_output / "render-progress.json").read_text(encoding="utf-8"))
    if result_manifest.get("job_id") != job_id or result_manifest.get("worker") != {"name": "renderright", "version": "0.1.0"}:
        raise ValueError("result manifest identity does not match qualification job")
    if result_manifest.get("recipe", {}).get("id") != "probe" or result_manifest.get("source") != {"path": None, "sha256": None}:
        raise ValueError("result manifest does not describe a source-free probe")
    if result_manifest.get("recipe", {}).get("sha256") != recipe_sha256_before:
        raise ValueError("result manifest recipe hash does not match qualification input")
    worker_blender_version = result_manifest.get("blender_version", "")
    normalized_job = {"schema": 1, "job_id": job_id, "kind": "image", "recipe": "probe", "params": {
        "width": 64, "height": 64, "samples": args.samples, "frame_count": 1,
        "fps": {"num": 1, "den": 1}, "device": args.device, "transparent": False,
        "env": "studio_lightbox"}}
    expected_fingerprint = canonical_hash({"job": normalized_job, "source_sha256": None, "mechanism_sha256": None,
        "worker_version": manifest["version"], "recipe_version": result_manifest["recipe"]["version"],
        "recipe_sha256": recipe_sha256_before, "blender_version": worker_blender_version,
        "worker_code_sha256": code_identity_before["worker_code_sha256"]})
    if result_manifest.get("job_fingerprint") != expected_fingerprint:
        raise ValueError("result fingerprint does not match worker code, recipe, and normalized qualification job")
    if progress.get("job_id") != job_id or progress.get("job_fingerprint") != result_manifest.get("job_fingerprint") or progress.get("state") != "complete":
        raise ValueError("worker progress does not confirm completion for this job")
    if not worker_blender_version.startswith(PINNED_BLENDER + " ") and worker_blender_version != PINNED_BLENDER:
        raise ValueError("worker result reports unexpected Blender version: " + repr(worker_blender_version))
    if probe.get("width") != 64 or probe.get("height") != 64:
        raise ValueError("Blender version or probe dimensions do not match expected values")
    artifacts = []
    for artifact in result_manifest.get("artifacts", []):
        path = contained(job_output, artifact["path"])
        if not path.is_file() or path.stat().st_size != artifact.get("bytes") or sha256(path) != artifact.get("sha256"):
            raise ValueError("artifact verification failed: " + str(artifact.get("path")))
        if artifact.get("role") == "probe" and path.suffix.lower() == ".png":
            with path.open("rb") as image:
                header = image.read(24)
            if len(header) != 24 or header[:8] != b"\x89PNG\r\n\x1a\n" or struct.unpack(">II", header[16:24]) != (64, 64):
                raise ValueError("probe PNG dimensions or signature are invalid")
        artifacts.append({"id": artifact["id"], "path": artifact["path"], "role": artifact["role"],
                          "bytes": path.stat().st_size, "sha256": artifact["sha256"]})
    if not artifacts or not any(item["role"] == "probe" for item in artifacts):
        raise ValueError("probe artifact is missing")
    if args.require_gpu and not probe.get("gpu_used"):
        raise RuntimeError("GPU was requested but probe reports no GPU use")
    for frame_number, frame in progress.get("frames", {}).items():
        if frame.get("decode_ok") is not True or (frame.get("width"), frame.get("height")) != (job["params"]["width"], job["params"]["height"]):
            raise ValueError("frame dimensions or decode status are invalid: " + str(frame_number))
        frame_path = contained(job_output, frame["path"])
        if not frame_path.is_file() or sha256(frame_path) != frame.get("sha256"):
            raise ValueError("frame hash verification failed: " + str(frame_number))

    ffmpeg_info = None
    if args.ffmpeg:
        ffmpeg_result = run([str(ffmpeg), "-version"])
        if ffmpeg_result.returncode != 0:
            raise RuntimeError("FFmpeg --version failed")
        ffmpeg_info = {"path": str(ffmpeg), "version": ffmpeg_result.stdout.splitlines()[0] if ffmpeg_result.stdout else "",
                       "binary_sha256": sha256(ffmpeg)}
    archive_info = None
    if args.blender_archive:
        archive = args.blender_archive.resolve(strict=True)
        actual_archive_sha256 = sha256(archive)
        if actual_archive_sha256 != BLENDER_ARCHIVE_SHA256:
            raise ValueError("Blender portable archive SHA256 does not match official checksum listing")
        archive_info = {"path": str(archive), "sha256": actual_archive_sha256, "checksum_listing": BLENDER_SHA256_LISTING}
    evidence_path = worker / "evidence" / "blender-5.2.2-windows.json"
    manifest["blender"] = {"version": PINNED_BLENDER, "qualification": "evidence/blender-5.2.2-windows.json"}
    (worker / "worker-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    record = {"schema": 1, "status": "qualified", "qualified_at": datetime.now(timezone.utc).isoformat(),
              "worker": {"name": "renderright", "version": manifest["version"], "protocol": manifest["protocol"], "manifest_sha256": sha256(worker / "worker-manifest.json"),
                         "code_sha256": code_identity_after["worker_code_sha256"], "files_sha256": code_identity_after["files_sha256"]},
              "recipe": {"path": manifest["recipes"], "version": result_manifest["recipe"]["version"], "sha256": recipe_sha256_after},
              "blender": {"path": str(blender), "version": worker_blender_version, "binary_sha256": sha256(blender),
                          "version_output_sha256": hashlib.sha256(version.stdout.encode("utf-8")).hexdigest()},
              "probe": {"device": probe.get("device"), "gpu_requested": probe.get("gpu_requested"), "gpu_used": probe.get("gpu_used"),
                        "samples": args.samples, "width": probe["width"], "height": probe["height"], "render_ok": probe.get("render_ok")},
              "blender_archive": archive_info, "ffmpeg": ffmpeg_info, "artifacts": artifacts,
              "job_id": job_id, "job_fingerprint": expected_fingerprint}
    record_path = output / "qualification.json"
    record_path.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    evidence_path.parent.mkdir(parents=True, exist_ok=True)
    evidence_path.write_text(json.dumps(record, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps({"status": "qualified", "blender": record["blender"], "probe": record["probe"], "record": str(record_path)}, sort_keys=True))
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except Exception as exc:
        print("qualification failed: " + str(exc), file=sys.stderr)
        raise SystemExit(1)
