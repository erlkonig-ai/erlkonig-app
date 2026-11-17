#!/usr/bin/env python3
"""Verify selected packaging sources before staging; never infer binary build history.

Development mode takes a pinned retained inputs.json: repository name ->
{"files": {relative path: {"sha256": hex, "mode": "0o600"} or {"symlink": target}}}.
Other retained metadata is evidence, not a Git-verified release revision. The
inventory uses the native validation source boundary: no .git, target* or
__pycache__ directories. Output must stay outside those selected source trees.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess

REPOSITORIES = {
    "erlkonig-app", "faculties", "triblespace-rs", "anybytes", "jerky", "mary",
    "GORBIE", "cubecl-fork", "burn-fork", "cubek-fork", "soma", "songbird",
}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def inventory(root):
    if not root.is_dir():
        raise ValueError(f"missing source directory: {root.name}")
    result = {}
    for here, directories, names in os.walk(root):
        directories[:] = [d for d in directories if d != ".git"
                          and not d.startswith("target") and d != "__pycache__"]
        for name in names + [d for d in directories if (Path(here) / d).is_symlink()]:
            path = Path(here) / name
            relative = str(path.relative_to(root))
            if path.is_symlink():
                result[relative] = {"symlink": os.readlink(path)}
            else:
                if not path.is_file():
                    raise ValueError(f"not a regular source file: {relative}")
                result[relative] = {"sha256": sha(path.read_bytes()),
                                    "mode": oct(path.stat().st_mode & 0o777)}
    return result


def git(root, *arguments):
    return subprocess.check_output(["git", "-C", str(root), *arguments], stderr=subprocess.PIPE)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("repo", type=Path)
    parser.add_argument("--source-inventory", type=Path)
    parser.add_argument("--source-inventory-sha256")
    parser.add_argument("--packaging-only", action="store_true")
    parser.add_argument("--output", type=Path)
    args = parser.parse_args()
    repo = args.repo.resolve(strict=True)
    names = {"erlkonig-app"} if args.packaging_only else REPOSITORIES
    lines = ["source-provenance-role selected-packaging-sources; binary build history is separate"]
    if args.source_inventory is not None or args.source_inventory_sha256 is not None:
        if args.source_inventory is None or not re.fullmatch("[0-9a-f]{64}", args.source_inventory_sha256 or ""):
            raise ValueError("source inventory requires its explicit SHA256")
        data = args.source_inventory.read_bytes()
        if sha(data) != args.source_inventory_sha256:
            raise ValueError("source inventory SHA256 mismatch")
        inputs = json.loads(data)
        if set(inputs) != REPOSITORIES and not (args.packaging_only and set(inputs) == names):
            raise ValueError("unexpected source repository set")
        for name, item in sorted(inputs.items()):
            for relative in item["files"]:
                path = PurePosixPath(relative)
                if path.is_absolute() or ".." in path.parts or str(path) != relative:
                    raise ValueError("invalid inventory path")
            root = repo if name == "erlkonig-app" else repo.parent / name
            if args.output is not None and (args.output.resolve() == root.resolve()
                                            or root.resolve() in args.output.resolve().parents):
                raise ValueError("local inventory output must be outside selected sources")
            if inventory(root) != item["files"]:
                raise ValueError(f"source inventory mismatch: {name}")
        lines += ["source-provenance local-inventory", "development-staging-only true",
                  "source-inventory-sha256 " + sha(data),
                  "app-files-inventory-sha256 " + sha(json.dumps(inputs["erlkonig-app"]["files"],
                                                               sort_keys=True, separators=(",", ":")).encode())]
        # The pinned manifest remains external evidence; never relabel its claimed
        # base commits as Git-verified release revisions.
    else:
        lines.append("source-provenance git-tracked")
        for name in sorted(names):
            root = repo if name == "erlkonig-app" else repo.parent / name
            if Path(git(root, "rev-parse", "--show-toplevel").decode().strip()).resolve() != root.resolve():
                raise ValueError(f"Git root does not match selected source: {name}")
            head = git(root, "rev-parse", "HEAD").decode().strip()
            if not re.fullmatch("[0-9a-f]{40}|[0-9a-f]{64}", head):
                raise ValueError(f"invalid Git revision: {name}")
            label = "app" if name == "erlkonig-app" else name
            lines += [f"{label} {head}",
                      f"{label}-tracked-diff-sha256 {sha(git(root, 'diff', '--binary', 'HEAD'))}"]
    print("\n".join(lines))


if __name__ == "__main__":
    try:
        main()
    except (ValueError, KeyError, TypeError, OSError, subprocess.CalledProcessError) as error:
        raise SystemExit(f"bundle: source provenance verification failed: {error}")
