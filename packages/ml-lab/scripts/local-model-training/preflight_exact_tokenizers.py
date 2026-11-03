#!/usr/bin/env python3
"""Fail-fast compatibility probe for the exact six-candidate tokenizer roster."""

from __future__ import annotations

import argparse
from collections.abc import Mapping, Sequence
import hashlib
import importlib.metadata
import json
from pathlib import Path
from typing import Any


EXPECTED_TRANSFORMERS_VERSION = "5.5.0"
PROBE_TEXT = "HeardRight exact tokenizer compatibility check."


def sha_file(path: Path) -> str:
    return "sha256:" + hashlib.sha256(path.read_bytes()).hexdigest()


def candidate_roster(recipes: dict[str, Any]) -> list[dict[str, str]]:
    families = recipes["families"]
    opt_in = recipes["candidate_pool"]["falcon_h1_1_5b"]
    roster = [
        {"lane": "falcon", "family": "falcon", **families["falcon"]["source"]},
        {"lane": "granite", "family": "granite", **families["granite"]["source"]},
        {"lane": "gemma", "family": "gemma", **families["gemma"]["stock"]},
        {"lane": "sotto", "family": "sotto", **families["sotto"]["source"]},
        {"lane": "falcon_1_5b", "family": "falcon", "id": opt_in[0]["id"], "revision": opt_in[0]["revision"]},
        {"lane": "falcon_1_5b_deep", "family": "falcon", "id": opt_in[1]["id"], "revision": opt_in[1]["revision"]},
    ]
    if len(roster) != 6 or len({(item["id"], item["revision"]) for item in roster}) != 6:
        raise RuntimeError("tokenizer compatibility roster must contain six unique exact model revisions")
    for item in roster:
        if not item["id"] or not item["revision"] or len(item["revision"]) != 40:
            raise RuntimeError(f"invalid exact tokenizer binding for {item['lane']}")
        item["required_backend"] = "TokenizersBackend" if item["family"] in {"falcon", "sotto"} else "AutoTokenizer"
    return roster


def validate_encoded_input_ids(encoded: Any, lane: str) -> Sequence[int]:
    if not isinstance(encoded, Mapping):
        raise RuntimeError(f"{lane} tokenizer output is not a Mapping")
    input_ids = encoded.get("input_ids")
    if not isinstance(input_ids, Sequence) or isinstance(input_ids, (str, bytes, bytearray)) or not input_ids:
        raise RuntimeError(f"{lane} tokenizer did not produce a nonempty input_ids sequence")
    if any(not isinstance(token_id, int) or isinstance(token_id, bool) for token_id in input_ids):
        raise RuntimeError(f"{lane} tokenizer input_ids must be an integer token sequence")
    return input_ids


def probe_candidates(
    recipes: dict[str, Any],
    transformers_version: str,
    auto_tokenizer: Any,
    tokenizers_backend: type,
) -> list[dict[str, Any]]:
    if transformers_version != EXPECTED_TRANSFORMERS_VERSION:
        raise RuntimeError(
            f"transformers version must equal {EXPECTED_TRANSFORMERS_VERSION}; received {transformers_version}"
        )
    results = []
    for item in candidate_roster(recipes):
        tokenizer = auto_tokenizer.from_pretrained(
            item["id"], revision=item["revision"], trust_remote_code=False
        )
        if item["required_backend"] == "TokenizersBackend" and not isinstance(tokenizer, tokenizers_backend):
            raise RuntimeError(
                f"{item['lane']} resolved to {type(tokenizer).__name__}, expected TokenizersBackend"
            )
        encoded = tokenizer(PROBE_TEXT, add_special_tokens=True)
        input_ids = validate_encoded_input_ids(encoded, item["lane"])
        token_payload = json.dumps(list(input_ids), separators=(",", ":")).encode("utf-8")
        results.append(
            {
                **item,
                "tokenizer_class": type(tokenizer).__name__,
                "encoded_tokens": len(input_ids),
                "encoded_container_class": type(encoded).__name__,
                "input_ids_class": type(input_ids).__name__,
                "input_ids_sha256": "sha256:" + hashlib.sha256(token_payload).hexdigest(),
                "integer_token_sequence": True,
                "token_id_min": min(input_ids),
                "token_id_max": max(input_ids),
                "vocab_size": getattr(tokenizer, "vocab_size", None),
                "has_chat_template": bool(getattr(tokenizer, "chat_template", None)),
            }
        )
    return results


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--recipes", required=True)
    parser.add_argument("--output", required=True)
    return parser.parse_args()


def main() -> None:
    args = parse_args()
    recipes_path = Path(args.recipes).resolve()
    output_path = Path(args.output).resolve()
    recipes = json.loads(recipes_path.read_text(encoding="utf-8"))
    from transformers import AutoTokenizer, TokenizersBackend

    version = importlib.metadata.version("transformers")
    results = probe_candidates(recipes, version, AutoTokenizer, TokenizersBackend)
    receipt = {
        "schema_version": "heardright.l1.exact_tokenizer_preflight.v1",
        "status": "passed",
        "transformers_version": version,
        "recipes_path": str(recipes_path),
        "recipes_sha256": sha_file(recipes_path),
        "candidate_count": len(results),
        "candidates": results,
    }
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"status": "passed", "output": str(output_path), "candidate_count": len(results)}))


if __name__ == "__main__":
    main()
