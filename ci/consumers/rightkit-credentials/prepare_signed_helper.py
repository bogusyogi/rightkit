#!/usr/bin/env python3
"""Prepare existing credentials consumer layout; never build, sign or authenticate."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys

NAME = "HeardRightAppleCredentials"
TEAM = "6KLGD3LLKF"
REQUIREMENT = 'anchor apple generic and certificate leaf[subject.OU] = "' + TEAM + '"'


def digest_bundle(bundle):
    entries = []
    for item in sorted(bundle.rglob("*")):
        rel = item.relative_to(bundle).as_posix()
        mode = item.lstat().st_mode
        if stat.S_ISLNK(mode):
            resolved = item.resolve(strict=True)
            if not resolved.is_relative_to(bundle.resolve()):
                raise RuntimeError("helper contains external symlink")
            entry = {"path": rel, "kind": "link", "target": os.readlink(item)}
        elif stat.S_ISDIR(mode):
            entry = {"path": rel, "kind": "directory"}
        elif stat.S_ISREG(mode):
            sha = hashlib.sha256()
            with item.open("rb") as stream:
                for data in iter(lambda: stream.read(1024 * 1024), b""):
                    sha.update(data)
            entry = {"path": rel, "kind": "file", "sha256": sha.hexdigest(),
                "executableBits": mode & 0o111}
        else:
            raise RuntimeError("helper contains special file")
        entries.append(entry)
    encoded = json.dumps(entries, sort_keys=True, separators=(",", ":")).encode()
    return {"format": "relative-path-kind-sha256-executable-bits-v1",
        "sha256": hashlib.sha256(encoded).hexdigest(), "entries": entries}


def verify(bundle):
    subprocess.run(["/usr/bin/codesign", "--verify", "--strict", "-R", "=" + REQUIREMENT,
        str(bundle)], check=True, timeout=30, stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL)



def checked_path(raw, *, existing):
    # Reject lexical escapes and every existing symlink component before any write.
    if not raw.is_absolute() or ".." in raw.parts:
        raise RuntimeError("paths must be absolute without parent traversal")
    for component in (raw, *raw.parents):
        if component.is_symlink():
            raise RuntimeError("symlink path components are refused")
    return raw.resolve(strict=existing)


def overlaps(left, right):
    return left == right or left.is_relative_to(right) or right.is_relative_to(left)


def validate_paths(args, source):
    scratch = checked_path(args.scratch_root, existing=True)
    if not scratch.is_dir():
        raise RuntimeError("approved scratch root must already exist")
    consumer = checked_path(args.consumer_executable, existing=True)
    bundle = checked_path(args.bundle, existing=False)
    receipt = checked_path(args.receipt, existing=False)
    protected = (source, *(parent for parent in source.parents if parent.suffix == ".app"))
    for item in (consumer, bundle, receipt):
        if item == scratch or not item.is_relative_to(scratch):
            raise RuntimeError("consumer and outputs must be inside approved scratch root")
        if any(overlaps(item, artifact) for artifact in protected):
            raise RuntimeError("consumer/output aliases signed helper or enclosing app")
    if not consumer.is_file():
        raise RuntimeError("consumer executable must be an existing regular file")
    if bundle.suffix != ".app" or os.path.lexists(bundle) or os.path.lexists(receipt):
        raise RuntimeError("bundle and receipt must be fresh output names")
    if any(overlaps(left, right) for left, right in
           ((bundle, receipt), (bundle, consumer), (receipt, consumer))):
        raise RuntimeError("bundle/receipt/consumer aliases are refused")
    # Parents must already exist, so validation covers their complete resolved ancestry.
    if not bundle.parent.is_dir() or not receipt.parent.is_dir():
        raise RuntimeError("output parents must already exist inside approved scratch")
    return scratch, consumer, bundle, receipt


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--scratch-root", required=True, type=Path)
    parser.add_argument("--consumer-executable", required=True, type=Path)
    parser.add_argument("--bundle", required=True, type=Path)
    parser.add_argument("--receipt", required=True, type=Path)
    args = parser.parse_args()
    if sys.platform != "darwin":
        raise RuntimeError("native macOS required")
    source = Path(os.environ["RK_SOURCE_HELPER_APP"]).resolve(strict=True)
    if source.name != NAME + ".app" or not source.is_dir():
        raise RuntimeError("expected existing HeardRightAppleCredentials.app")
    binary = source / "Contents" / "MacOS" / NAME
    if not binary.is_file():
        raise RuntimeError("signed helper executable missing")
    # Complete all containment/alias checks before mkdir, copy, verification or receipt writes.
    scratch, consumer, bundle, receipt_path = validate_paths(args, source)
    # Source artifact selection belongs Fable. Derive hashes from that actual selection.
    verify(source)
    before = digest_bundle(source)
    bundle.mkdir(mode=0o700)  # Exclusive claim; never reuse an output bundle.
    executable = bundle / "Contents" / "MacOS" / "consumer"
    helper = bundle / "Contents" / "Library" / "LoginItems" / (NAME + ".app")
    executable.parent.mkdir(parents=True)
    helper.parent.mkdir(parents=True)
    shutil.copy2(consumer, executable)
    subprocess.run(["/usr/bin/ditto", str(source), str(helper)], check=True, timeout=60)
    verify(helper)
    after = digest_bundle(helper)
    if before != after or digest_bundle(source) != before:
        raise RuntimeError("selected signed helper changed during copy")
    consumer_hash = hashlib.sha256(consumer.read_bytes()).hexdigest()
    if hashlib.sha256(executable.read_bytes()).hexdigest() != consumer_hash:
        raise RuntimeError("consumer executable changed during copy")
    receipt = {"schema": 1, "kind": "credentials-native-helper-input",
        "approvedScratchRoot": str(scratch),
        "sourceHelperApp": str(source), "copiedHelperApp": str(helper),
        "name": NAME, "teamId": TEAM, "requirement": REQUIREMENT,
        "helperTree": before, "consumerExecutable": str(executable),
        "consumerSha256": consumer_hash, "signatureVerified": True,
        "nativeConsumerVerified": False}
    # Exclusive creation also refuses an alias introduced after preflight.
    descriptor = os.open(receipt_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        stream.write(json.dumps(receipt, indent=2) + "\n")
    print(str(receipt_path))


if __name__ == "__main__":
    main()
