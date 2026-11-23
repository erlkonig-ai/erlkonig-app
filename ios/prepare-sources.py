#!/usr/bin/env python3
"""Materialize the pinned public sources of the iOS native workspace."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
from urllib.request import urlopen

MODULE = Path(__file__).resolve().parent


def tree_digest(directory):
    digest = hashlib.sha256()
    for path in sorted(directory.rglob("*")):
        if path.is_symlink():
            kind, data = b"L", os.fsencode(os.readlink(path))
        elif path.is_file():
            kind = b"X" if path.stat().st_mode & 0o111 else b"F"
            data = path.read_bytes()
        else:
            continue
        digest.update(path.relative_to(directory).as_posix().encode() + b"\0")
        digest.update(kind + hashlib.sha256(data).digest())
    return digest.hexdigest()


def acquire(input_spec, output):
    name = input_spec["name"]
    dest = output / name
    expected = input_spec["tree_sha256"]
    if dest.exists():
        if tree_digest(dest) != expected:
            raise SystemExit(f"{dest} differs from the pinned tree. Move it aside and retry.")
        print(f"Verified {name}")
        return
    with tempfile.TemporaryDirectory(dir=output, prefix=f".{name}-") as temp:
        work = Path(temp)
        archive = work / "source.tar.gz"
        with urlopen(input_spec["url"], timeout=90) as response, archive.open("wb") as target:
            shutil.copyfileobj(response, target)
        with archive.open("rb") as stream:
            if hashlib.file_digest(stream, "sha256").hexdigest() != input_spec["archive_sha256"]:
                raise SystemExit(f"Archive checksum mismatch for {name}")
        extracted = work / "extract"
        extracted.mkdir()
        with tarfile.open(archive, "r:gz") as source:
            source.extractall(extracted, filter="data")
        [source] = extracted.iterdir()
        if patch := input_spec.get("patch"):
            subprocess.run(["git", "apply", "--whitespace=nowarn", str(MODULE / "upstream-patches" / patch)],
                           cwd=source, env={**os.environ, "GIT_CEILING_DIRECTORIES": str(extracted)}, check=True)
        if tree_digest(source) != expected:
            raise SystemExit(f"Patched source tree checksum mismatch for {name}")
        source.rename(dest)
        print(f"Prepared {name}")


def main():
    output = MODULE / "upstream"
    output.mkdir(exist_ok=True)
    for input_spec in json.loads((MODULE / "upstream-lock.json").read_text()):
        acquire(input_spec, output)


if __name__ == "__main__":
    main()
