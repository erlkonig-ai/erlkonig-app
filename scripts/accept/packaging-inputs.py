#!/usr/bin/env python3
"""Real packaging scripts with synthetic executables and stub signing/disk tools.
Requires macOS PlistBuddy for read-only fixture metadata. Never builds or launches an app.
"""
from pathlib import Path
import hashlib
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile

SOURCE = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("provenance", SOURCE / "scripts/bundle-provenance.py")
helper = importlib.util.module_from_spec(spec)
sys.dont_write_bytecode = True
spec.loader.exec_module(helper)
DRIVER = r'''#!PYTHON
import json, os, pathlib, shutil, subprocess, sys
name, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
root = pathlib.Path(os.environ["FIXTURE"])
with (root / "calls.jsonl").open("a") as out:
    out.write(json.dumps({"tool": name, "args": args}) + "\n")
if name == "uname": print("Darwin")
elif name == "cargo": raise SystemExit("unexpected Cargo invocation")
elif name == "git":
    if os.environ.get("FAIL_GIT_DIFF") and "diff" in args: raise SystemExit(91)
    raise SystemExit(subprocess.call(["/usr/bin/git", *args]))
elif name == "codesign":
    if "--force" in args:
        signatures = pathlib.Path(args[-1]) / "Contents/_CodeSignature"
        signatures.mkdir(exist_ok=True)
        (signatures / "CodeResources").write_text("synthetic signature")
elif name in {"xattr", "plutil", "mount"}: pass
elif name == "lipo": print("arm64")
elif name == "otool": print("LC_BUILD_VERSION\n minos 14.0")
elif name == "renderer":
    p = pathlib.Path(args[0]); p.mkdir(parents=True, exist_ok=True)
    (p / "icon.png").write_text("synthetic icon")
elif name == "iconutil": pathlib.Path(args[args.index("--output") + 1]).write_text("synthetic icns")
elif name == "ditto": shutil.copytree(args[0], args[1], symlinks=True)
elif name == "hdiutil":
    if args[0] == "create":
        pathlib.Path(args[-1]).write_text(json.dumps({"stage": args[args.index("-srcfolder") + 1]}))
    elif args[0] == "attach":
        stage = json.loads(pathlib.Path(args[-1]).read_text())["stage"]
        shutil.copytree(stage, args[args.index("-mountpoint") + 1], dirs_exist_ok=True, symlinks=True)
    elif args[0] == "detach":
        for p in pathlib.Path(args[-1]).iterdir():
            if p.is_dir() and not p.is_symlink(): shutil.rmtree(p)
            else: p.unlink()
    elif args[0] != "verify": raise SystemExit(92)
else: raise SystemExit(93)
'''
checks = []
with tempfile.TemporaryDirectory(prefix="erlkonig-packaging-inputs-") as directory:
    root = Path(directory)
    app = root / "erlkonig-app"
    shutil.copytree(SOURCE, app)
    for name in helper.REPOSITORIES - {"erlkonig-app", "soma"}:
        (root / name).mkdir()
        (root / name / "source.txt").write_text("synthetic " + name)
    # Use a synthetic copy of the pinned crate layout so this test stays offline.
    manifest_path = app / "ci/source-inputs.json"
    manifest = json.loads(manifest_path.read_text())
    soma = next(item for item in manifest["siblings"] if item["path"] == "soma")
    for name in soma["files"]:
        path = root / "soma/soma-client" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(b"synthetic crate fixture")
        soma["files"][name] = hashlib.sha256(path.read_bytes()).hexdigest()
    manifest_path.write_text(json.dumps(manifest))
    stubs = root / "stubs"
    stubs.mkdir()
    driver = stubs / "driver"
    driver.write_text(DRIVER.replace("#!PYTHON", "#!" + sys.executable))
    driver.chmod(0o755)
    for tool in ["uname", "cargo", "git", "codesign", "xattr", "plutil", "mount", "lipo", "otool", "renderer", "iconutil", "ditto", "hdiutil"]:
        (stubs / tool).symlink_to(driver)
    release = root / "release"
    release.mkdir()
    binary = release / "erlkonig-mac"
    binary.write_text("#!/bin/sh\nexit 94 # must never be launched\n")
    binary.chmod(0o755)
    env = {"PATH": str(stubs) + ":/usr/bin:/bin:/usr/sbin:/sbin", "HOME": str(root / "home"),
           "FIXTURE": str(root), "LC_ALL": "C", "COPYFILE_DISABLE": "1"}
    (root / "home").mkdir()
    inventory = {name: {"files": helper.inventory(root / name)} for name in helper.REPOSITORIES}
    inventory_path = root / "inputs.json"
    inventory_path.write_text(json.dumps(inventory))
    pin = hashlib.sha256(inventory_path.read_bytes()).hexdigest()
    options = ["--source-inventory", str(inventory_path), "--source-inventory-sha256", pin]

    def calls():
        path = root / "calls.jsonl"
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def invoke(arguments, expected=None, extra_env=None):
        result = subprocess.run([str(a) for a in arguments], env={**env, **(extra_env or {})},
                                capture_output=True, text=True, timeout=30)
        if expected is None:
            assert result.returncode == 0, (arguments, result.stderr)
        else:
            assert result.returncode != 0 and expected in result.stderr, (arguments, result.returncode, result.stderr)
        return result

    def bundle(label, supplied=options, expected=None, reuse=None, extra_env=None):
        out = root / label
        before = len(calls())
        command = ["sh", app / "scripts/bundle.sh", "--out", out, "--icon-renderer", stubs / "renderer"]
        command += ["--reuse-app", reuse] if reuse else ["--release-dir", release]
        result = invoke(command + supplied, expected, extra_env)
        if expected:
            assert not out.exists(), "failure created staging"
            assert not any(c["tool"] in {"renderer", "iconutil"} or (c["tool"] == "codesign" and "--force" in c["args"]) for c in calls()[before:])
        checks.append(label)
        return out / "Erlkonig.app", result

    bundle("inventory-release-signing-rejected", expected="ad-hoc development staging only",
           extra_env={"ERLKONIG_SIGN_IDENTITY": "Developer ID Application: Synthetic"})
    bundle("erlkonig-app/dist.noindex", expected="outside selected sources")
    bundle("missing-provenance", [], "source provenance verification failed")
    bundle("missing-inventory-pin", options[:2], "explicit SHA256")
    bundle("wrong-inventory-pin", options[:3] + ["0" * 64], "SHA256 mismatch")
    old = inventory_path.read_bytes()
    for label, data, error in [("malformed-inventory", b"{", "source provenance verification failed"),
                               ("incomplete-inventory", b'{}', "repository set")]:
        inventory_path.write_bytes(data)
        bundle(label, options[:3] + [hashlib.sha256(data).hexdigest()], error)
    inventory_path.write_bytes(old)
    source_file = app / "Cargo.lock"
    original = source_file.read_bytes()
    source_file.write_bytes(original + b"\n")
    bundle("changed-source", expected="source inventory mismatch")
    source_file.write_bytes(original)
    original_mode = source_file.stat().st_mode & 0o777
    source_file.chmod(original_mode ^ 0o100)
    bundle("changed-source-mode", expected="source inventory mismatch")
    source_file.chmod(original_mode)
    (app / "unexpected.txt").write_text("unexpected")
    bundle("extra-source", expected="source inventory mismatch")
    (app / "unexpected.txt").unlink()
    bundled, _ = bundle("inventory-bundle")
    provenance = (bundled / "Contents/Resources/provenance.txt").read_text()
    assert "source-provenance local-inventory\n" in provenance and "development-staging-only true\n" in provenance
    assert "source-inventory-sha256 " + pin in provenance
    assert not any(line.startswith("app ") for line in provenance.splitlines())
    reused, _ = bundle("inventory-reuse", reuse=bundled)
    assert (reused / "Contents/Resources/binary-provenance.txt").read_text() == provenance

    # Real local Git fixtures prove root ownership and error propagation; no
    # source repository, network remote, hook or global Git setting is changed.
    git_env = {**env, "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": "/dev/null"}
    def git(cwd, *arguments):
        subprocess.run(["/usr/bin/git", "-C", str(cwd), *arguments], env=git_env,
                       check=True, capture_output=True)
    git(root, "init", "-q")
    git(root, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "--allow-empty", "-qm", "fixture")
    bundle("parent-git-rejected", [], "Git root does not match selected source")
    for name in helper.REPOSITORIES - {"soma"}:
        git(root / name, "init", "-q")
        git(root / name, "add", ".")
        git(root / name, "-c", "user.name=Fixture", "-c", "user.email=fixture@example.invalid", "commit", "-qm", "fixture")
    git_bundle, _ = bundle("git-bundle", [])
    git_provenance = (git_bundle / "Contents/Resources/provenance.txt").read_text()
    assert "source-provenance git-tracked-and-registry\n" in git_provenance and "source-inventory-sha256" not in git_provenance
    assert all(("app" if name == "erlkonig-app" else name) + " " in git_provenance for name in helper.REPOSITORIES - {"soma"})
    assert "soma-client 0.1.0 registry-sha256 " in git_provenance
    bundle("git-diff-failure", [], "source provenance verification failed", extra_env={"FAIL_GIT_DIFF": "1"})

    def package(label, fixture=bundled, expected=None):
        before = len(calls())
        result = invoke(["sh", app / "scripts/package-dmg.sh", "--app", fixture, "--out", root / label], expected)
        if expected:
            assert not any(c["tool"] == "hdiutil" for c in calls()[before:]), "invalid input reached disk-image tool"
        checks.append(label)
        return result

    package("notices-package")
    assert helper.inventory(bundled / "Contents/Resources/notices") == helper.inventory(app / "notices")
    notice = bundled / "Contents/Resources/notices/faculties/LICENSE-MIT"
    data = notice.read_bytes()
    notice.write_bytes(data + b"tampered")
    package("altered-notice", expected="not the source notice")
    notice.unlink()
    package("missing-notice", expected="missing bundle files")
    notice.write_bytes(data)
    extra = bundled / "Contents/Resources/notices/extra.txt"
    extra.write_text("extra")
    package("extra-notice", expected="unexpected bundle file")
    extra.unlink()
    extra.mkdir()
    package("extra-notice-directory", expected="unexpected bundle directory")
    extra.rmdir()
    extra.symlink_to(notice)
    package("notice-symlink", expected="unexpected bundle symlink")
    extra.unlink()
    manifest = bundled / "Contents/Resources/notices/manifest.json"
    original_manifest = manifest.read_bytes()
    manifest.write_bytes(b"{}")
    package("altered-bundled-manifest", expected="not the source notice")
    manifest.write_bytes(original_manifest)
    source_notice = app / "notices/faculties/LICENSE-MIT"
    source_notice.write_bytes(data + b"tampered")
    notice.write_bytes(data + b"tampered")
    package("source-notice-hash-mismatch", expected="source notice hash mismatch")
    source_notice.write_bytes(data)
    notice.write_bytes(data)
    source_extra = app / "notices/extra.txt"
    source_extra.write_text("unexpected source")
    package("extra-source-notice", expected="unexpected source notice file")
    source_extra.unlink()
    source_extra.symlink_to(source_notice)
    package("source-notice-symlink", expected="unexpected source notice symlink")
    source_extra.unlink()
    for relative in ["README.txt", "asset-evidence.json"]:
        supporting = bundled / "Contents/Resources/notices" / relative
        saved = supporting.read_bytes()
        supporting.write_bytes(saved + b"tampered")
        package("altered-" + relative, expected="not the source notice")
        supporting.write_bytes(saved)
    source_manifest = app / "notices/manifest.json"
    content = json.loads(source_manifest.read_text())
    content["files"].append(content["files"][0])
    source_manifest.write_text(json.dumps(content))
    package("duplicate-manifest-entry", expected="duplicate notice path")
    source_manifest.write_bytes(original_manifest)
    for name in ["self.key", "plugin/.mcp.json"]:
        extra = bundled / "Contents/Resources" / name
        extra.write_text("synthetic forbidden file")
        package("forbidden-" + extra.name, expected="unexpected bundle file")
        extra.unlink()
    package("final-notices-package")
    assert binary.read_text().startswith("#!/bin/sh\nexit 94")
    assert not any(c["tool"] == "cargo" for c in calls())

print(json.dumps({"status": "passed", "checks": checks, "count": len(checks),
                  "scope": "Real bundle/package scripts; synthetic binary, signing, icon and disk-image tools; isolated temporary profiles and Git fixtures. No build, app runtime, real signing, mounts, install or publication."}, indent=2))
