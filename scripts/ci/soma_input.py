"""Materialize the published soma-client crate at the path used by Faculties."""
import hashlib
from io import BytesIO
from pathlib import Path
import re
import tarfile
import urllib.request


def validate_pin(item):
    if (item["crate"] != "soma-client" or not re.fullmatch(r"\d+\.\d+\.\d+", item["version"])
            or not re.fullmatch("[0-9a-f]{64}", item["sha256"])
            or set(item["files"]) != {"Cargo.toml", "Cargo.toml.orig", "Cargo.lock", "src/lib.rs"}
            or any(not re.fullmatch("[0-9a-f]{64}", value) for value in item["files"].values())):
        raise ValueError("invalid soma-client source pin")


def verify(root, item):
    validate_pin(item)
    expected = {Path("soma-client") / path for path in item["files"]}
    paths = list(root.rglob("*"))
    if (root.is_symlink() or any(path.is_symlink() for path in paths)
            or {p.relative_to(root) for p in paths if not p.is_dir()} != expected):
        raise ValueError("unexpected files in soma-client source")
    for path, digest in item["files"].items():
        if hashlib.sha256((root / "soma-client" / path).read_bytes()).hexdigest() != digest:
            raise ValueError("soma-client source differs from registry package")


def fetch(root, item):
    validate_pin(item)
    prefix = "soma-client-" + item["version"]
    url = f"https://static.crates.io/crates/soma-client/{prefix}.crate"
    with urllib.request.urlopen(url, timeout=60) as response:
        data = response.read()
    if hashlib.sha256(data).hexdigest() != item["sha256"]:
        raise ValueError("soma-client registry checksum mismatch")
    root.mkdir()
    with tarfile.open(fileobj=BytesIO(data), mode="r:gz") as archive:
        for path in item["files"]:
            member = archive.getmember(prefix + "/" + path)
            if not member.isfile():
                raise ValueError("expected regular source member in soma-client crate")
            target = root / "soma-client" / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(archive.extractfile(member).read())
    verify(root, item)
