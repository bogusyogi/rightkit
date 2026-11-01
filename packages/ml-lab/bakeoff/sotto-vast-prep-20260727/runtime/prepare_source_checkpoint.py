#!/usr/bin/env python3
from __future__ import annotations
import argparse, hashlib, json
from pathlib import Path
from huggingface_hub import snapshot_download

def sha_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()

def sha_tree(directory: Path) -> str:
    entries = [[item.relative_to(directory).as_posix(), item.stat().st_size, sha_file(item)] for item in sorted(directory.rglob("*")) if item.is_file()]
    return "sha256:" + hashlib.sha256(json.dumps(entries, separators=(",", ":")).encode()).hexdigest()

def main() -> None:
    parser = argparse.ArgumentParser(description="Freeze an exact Hugging Face source checkpoint as an evaluation-compatible artifact.")
    parser.add_argument("--model-source", required=True)
    parser.add_argument("--model-revision", required=True)
    parser.add_argument("--resolved-model-source")
    parser.add_argument("--output-dir", required=True)
    args = parser.parse_args()
    output = Path(args.output_dir).resolve(); output.mkdir(parents=True, exist_ok=True)
    checkpoint = Path(args.resolved_model_source).resolve() if args.resolved_model_source else Path(snapshot_download(repo_id=args.model_source, revision=args.model_revision)).resolve()
    if not checkpoint.is_dir() or (args.resolved_model_source and checkpoint.name != args.model_revision):
        raise SystemExit("resolved model source is not the exact revision snapshot")
    manifest = {"schema_version":"heardright.l1.action_manifest.v1","action":"train","family":"source-preflight","source_checkpoint_only":True,"checkpoint_dir":str(checkpoint),"checkpoint_dir_sha256":sha_tree(checkpoint),"model_source":args.model_source,"model_revision":args.model_revision,"resolved_model_source":str(checkpoint)}
    target = output / "train.manifest.json"; temporary = target.with_suffix(".tmp")
    temporary.write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8"); temporary.replace(target)
    print(json.dumps({"status":"ok","manifest":str(target),"checkpoint_dir":str(checkpoint),"checkpoint_dir_sha256":manifest["checkpoint_dir_sha256"]}))

if __name__ == "__main__": main()
