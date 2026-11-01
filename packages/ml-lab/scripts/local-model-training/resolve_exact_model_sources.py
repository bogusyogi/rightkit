#!/usr/bin/env python3
"""Resolve exact Hugging Face revisions to verified local snapshot directories."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any, Callable

from preflight_exact_tokenizers import candidate_roster


def sha_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def source_roster(recipes: dict[str, Any]) -> list[dict[str, str]]:
    roster = candidate_roster(recipes)
    grmr = recipes["families"]["gemma"]["grmr"]
    return [*roster, {"lane": "gemma_grmr", "family": "gemma", "id": grmr["id"], "revision": grmr["revision"], "required_backend": "AutoTokenizer"}]


def resolve_sources(recipes: dict[str, Any], downloader: Callable[..., str]) -> list[dict[str, str]]:
    resolved = []
    for item in source_roster(recipes):
        snapshot = Path(downloader(repo_id=item["id"], revision=item["revision"])).resolve()
        if not snapshot.is_dir() or snapshot.name != item["revision"]:
            raise RuntimeError(f"{item['lane']} did not resolve to the exact revision snapshot directory")
        resolved.append({
            "lane": item["lane"],
            "family": item["family"],
            "source_id": item["id"],
            "revision": item["revision"],
            "resolved_path": str(snapshot),
        })
    return resolved


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--recipes", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    from huggingface_hub import snapshot_download

    recipes = json.loads(args.recipes.read_text("utf-8"))
    entries = resolve_sources(recipes, snapshot_download)
    receipt = {
        "schema_version": "heardright.l1.exact_source_resolution.v1",
        "status": "passed",
        "recipes_path": str(args.recipes.resolve()),
        "recipes_sha256": sha_file(args.recipes),
        "entry_count": len(entries),
        "entries": entries,
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    temporary = args.output.with_suffix(args.output.suffix + ".tmp")
    temporary.write_text(json.dumps(receipt, indent=2) + "\n", "utf-8")
    temporary.replace(args.output)
    print(json.dumps({"status": "passed", "output": str(args.output), "entry_count": len(entries)}))


if __name__ == "__main__":
    main()
