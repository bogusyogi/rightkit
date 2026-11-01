#!/usr/bin/env python3
"""Build reproducible private Mac TDT archive with validated fused graph.

Inputs are local split model files plus a local compiled CoreML graph and
its export receipt. The receipt binds graph identity and output contract to a
recorded validation run; a directory name alone is not accepted.
No network, credentials, signing, or upload is performed. Archive contains one
``parakeet-tdt-v3`` directory, split files, graph, and its hash manifest.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import tarfile
import tempfile
from pathlib import Path


TOP = "parakeet-tdt-v3"
SCHEMA = "heardright.coreml.fused-model.v1"
GRAPH_NAMES = {"fused_predictor_joint.mlmodelc"}
GRAPH_VOCAB = 8198
GRAPH_DIMS = {"pred_layers": 2, "pred_hidden": 640, "enc_d_model": 1024, "vocab": GRAPH_VOCAB}


def digest(path: Path) -> str:
    if path.is_file():
        h = hashlib.sha256()
        with path.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                h.update(block)
        return h.hexdigest()
    h = hashlib.sha256()
    for child in sorted(p for p in path.rglob("*") if p.is_file()):
        h.update(str(child.relative_to(path)).replace("\\", "/").encode())
        h.update(b"\0")
        with child.open("rb") as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                h.update(block)
    return h.hexdigest()


def files(root: Path):
    return sorted(p for p in root.rglob("*") if p.is_file())


def tar_add(tar: tarfile.TarFile, path: Path, arcname: str) -> None:
    info = tar.gettarinfo(str(path), arcname=arcname)
    info.uid = info.gid = 0
    info.uname = info.gname = ""
    info.mtime = 0
    info.mode = 0o755 if path.is_dir() else 0o644
    if path.is_file():
        with path.open("rb") as stream:
            tar.addfile(info, stream)
    else:
        tar.addfile(info)


def _receipt_export_path(receipt: Path, value: str) -> Path:
    exported = Path(value).expanduser()
    candidates = [
        exported if exported.is_absolute() else Path.cwd() / exported,
        receipt.parent / exported,
    ]
    for candidate in candidates:
        if candidate.resolve().exists():
            return candidate.resolve()
    return candidates[0].resolve()


def validate_export_receipt(graph: Path, receipt: Path | None) -> Path:
    receipt = (receipt or graph.parent / "fused_probe.json").expanduser().resolve()
    if not receipt.is_file():
        raise ValueError(f"verified fused export receipt missing: {receipt}")
    try:
        report = json.loads(receipt.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise ValueError(f"invalid fused export receipt {receipt}: {error}") from error
    if _receipt_export_path(receipt, str(report.get("export", ""))) != graph:
        raise ValueError(f"export receipt does not identify graph {graph}")
    if report.get("graph_sha256") != digest(graph):
        raise ValueError("export receipt graph hash is missing or stale")
    if report.get("dims") != GRAPH_DIMS:
        raise ValueError(f"export receipt dimensions are not production TDT v3: {report.get('dims')}")
    if report.get("joint_output") != "raw_linear" or report.get("joint_layout") != "rank3":
        raise ValueError("export receipt does not prove raw rank-3 joint output")
    outputs = set(report.get("outputs", []))
    if not {"logits", "predictor_g", "state_h_out", "state_c_out"}.issubset(outputs):
        raise ValueError("export receipt is missing fused decoder outputs")
    batches = report.get("batches", [])
    batch_two = next((item for item in batches if item.get("batch") == 2), None)
    if not batch_two or [2, 1, 1, GRAPH_VOCAB] not in batch_two.get("shapes", []):
        raise ValueError("export receipt does not prove batch-2 raw logits")
    return receipt


def build(split: Path, graph: Path, output: Path, receipt: Path | None = None) -> dict:
    split = split.expanduser().resolve()
    graph = graph.expanduser().resolve()
    if not split.is_dir():
        raise ValueError(f"split model directory missing: {split}")
    if graph.name not in GRAPH_NAMES or not graph.is_dir():
        raise ValueError(f"graph must be a directory named one of {sorted(GRAPH_NAMES)}: {graph}")
    required_stages = ("MelSpectrogram", "AudioEncoder", "TextDecoder", "Joint")
    for name in required_stages:
        if not (split / (name + ".mlmodelc")).is_dir():
            raise ValueError(f"production split stage must be precompiled: {name}.mlmodelc")
    if any(path.suffix in {".mlpackage", ".mlmodel"} for path in split.rglob("*")):
        raise ValueError("production split archive cannot contain source .mlpackage/.mlmodel stages")
    receipt = validate_export_receipt(graph, receipt)
    pipeline = split / "pipeline.json"
    vocab = split / "vocab.txt"
    for required in (pipeline, vocab):
        if not required.is_file():
            raise ValueError(f"split model missing {required.name}")
    meta = json.loads(pipeline.read_text(encoding="utf-8"))
    vocab_count = sum(1 for _ in vocab.open(encoding="utf-8"))
    required_meta = (
        "pred_layers",
        "pred_hidden",
        "enc_d_model",
        "blank_id",
        "tdt_durations",
        "is_tdt",
    )
    missing_meta = [name for name in required_meta if name not in meta]
    if missing_meta:
        raise ValueError(f"pipeline.json missing required fields: {missing_meta}")
    dimensions = {
        "pred_layers": int(meta["pred_layers"]),
        "pred_hidden": int(meta["pred_hidden"]),
        "enc_d_model": int(meta["enc_d_model"]),
        "vocab": GRAPH_VOCAB,
    }
    if dimensions != {"pred_layers": 2, "pred_hidden": 640, "enc_d_model": 1024, "vocab": GRAPH_VOCAB}:
        raise ValueError(f"unsupported production dimensions: {dimensions}")
    if vocab_count != 8192:
        raise ValueError(f"unsupported Parakeet TDT vocabulary size: {vocab_count}")
    if (
        meta["is_tdt"] is not True
        or int(meta["blank_id"]) != 8192
        or [int(value) for value in meta["tdt_durations"]] != [0, 1, 2, 3, 4]
    ):
        raise ValueError("pipeline.json TDT blank/duration contract is incompatible")

    output = output.expanduser().resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    with tempfile.TemporaryDirectory(prefix="heardright-model-archive-") as temp:
        root = Path(temp) / TOP
        shutil.copytree(split, root, symlinks=False)
        target_graph = root / graph.name
        if target_graph.is_dir():
            shutil.rmtree(target_graph)
        elif target_graph.exists():
            target_graph.unlink()
        shutil.copytree(graph, target_graph, symlinks=False)
        split_files = [
            {"name": str(path.relative_to(root)).replace("\\", "/"), "sha256": digest(path)}
            for path in files(root)
            if path != target_graph
            and not path.is_relative_to(target_graph)
            and path.name != "fused_predictor_joint.manifest.json"
        ]
        manifest = {
            "schema": SCHEMA,
            "graph": graph.name,
            "graph_sha256": digest(target_graph),
            "pipeline_sha256": digest(root / "pipeline.json"),
            "vocab_sha256": digest(root / "vocab.txt"),
            "vocab_tokens": vocab_count,
            "blank_id": 8192,
            "tdt_durations": [0, 1, 2, 3, 4],
            "is_tdt": True,
            "split_files": split_files,
            "dimensions": dimensions,
            "beam_width": 2,
            "max_rows": 2,
            "joint_output": "raw_linear",
            "joint_norm_precision": "host_fp32",
            "export_receipt_sha256": digest(receipt),
        }
        (root / "fused_predictor_joint.manifest.json").write_text(
            json.dumps(manifest, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
        with output.open("wb") as raw:
            import gzip

            with gzip.GzipFile(fileobj=raw, mode="wb", mtime=0, filename="") as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as archive:
                    tar_add(archive, root, TOP)
                    for path in sorted(root.rglob("*")):
                        tar_add(archive, path, str(path.relative_to(Path(temp))).replace("\\", "/"))
    return {"archive": str(output), "archive_sha256": digest(output), "manifest": manifest}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--split-dir", type=Path, required=True)
    parser.add_argument("--graph", type=Path, required=True)
    parser.add_argument(
        "--receipt",
        type=Path,
        help="verified fused_probe.json; defaults to graph sibling receipt",
    )
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    print(json.dumps(build(args.split_dir, args.graph, args.out, args.receipt), indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
