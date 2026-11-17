#!/usr/bin/env python3
"""Synthetic orchestration checks; never invoke Apple signing/network tools."""
import json
import hashlib
import plistlib
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

SOURCE = Path(__file__).resolve().parents[1] / "release-dmg.sh"
DRIVER = r'''#!/usr/bin/env python3
import json, os, pathlib, shutil, sys
tool, args = pathlib.Path(sys.argv[0]).name, sys.argv[1:]
trace = pathlib.Path(os.environ["RELEASE_TEST_TRACE"])
previous = [json.loads(line)["event"] for line in trace.read_text().splitlines()] if trace.exists() else []
mode = os.environ.get("RELEASE_TEST_MODE", "")
if tool == "uname":
    print("Darwin"); sys.exit(0)
if tool == "ditto":
    shutil.copytree(args[0], args[1]); sys.exit(0)
if tool == "package-dmg.sh":
    event = "package"
elif tool == "codesign":
    kind = "app" if args[-1].endswith(".app") else "dmg"
    action = "display" if "--display" in args else "verify" if "--verify" in args else "sign"
    event = action + "-" + kind
    if event == "verify-dmg" and "validate" in previous:
        event = "verify-final"
else:
    assert tool == "xcrun", tool
    event = args[1]
with trace.open("a") as log:
    log.write(json.dumps({"event": event, "args": args}) + "\n")
if mode == event:
    sys.exit(2)
if event == "display-app":
    print("Authority=" + ("Apple Development: Fixture" if mode == "wrong-identity" else "Developer ID Application: Fixture"), file=sys.stderr)
    if mode != "no-timestamp": print("Timestamp=fixture", file=sys.stderr)
    if mode != "no-runtime": print("CodeDirectory flags=0x10000(runtime)", file=sys.stderr)
elif event == "sign-app":
    assert "--timestamp" in args and "--options" in args and "runtime" in args
    assert "--deep" not in args
    entitlements = pathlib.Path(args[args.index("--entitlements") + 1]).read_text()
    assert "<dict/>" in entitlements
    (pathlib.Path(args[-1]) / "signed-fixture").write_text("signed copy")
elif event == "package":
    app = pathlib.Path(args[args.index("--app") + 1])
    assert (app / "signed-fixture").exists()
    out = pathlib.Path(args[args.index("--out") + 1])
    (out / "Erlkonig-0.1.1-44-macos-arm64-development.dmg").write_text("fixture DMG")
elif event == "sign-dmg":
    assert "--timestamp" in args and "--options" not in args and "--deep" not in args
elif event == "submit":
    assert args[0:2] == ["notarytool", "submit"]
    assert args[args.index("--keychain-profile") + 1] == "synthetic-existing-profile"
    assert "--wait" in args and "--output-format" in args
    assert not any(a in args for a in ["--password", "--key", "--apple-id", "--force"])
    if mode == "malformed": print("not JSON")
    else:
        response = {"id": "00000000-0000-4000-8000-000000000000", "status": {"invalid": "Invalid", "pending": "In Progress"}.get(mode, "Accepted")}
        if mode == "missing-id": del response["id"]
        if mode == "empty-id": response["id"] = "   "
        if mode == "invalid-id": response["id"] = 42
        print(json.dumps(response))
elif event in ["staple", "validate"]:
    assert args[0] == "stapler"
    print("Synthetic stapler success")
    if event == "staple" and mode == "race":
        pathlib.Path(os.environ["RELEASE_TEST_DEST"]).write_text("competitor")
'''


def run_case(root, mode):
    case = root / (mode or "success")
    scripts, bin_dir, out = case / "repo/scripts", case / "bin", case / "out"
    for directory in (scripts, bin_dir, out):
        directory.mkdir(parents=True)
    script = scripts / "release-dmg.sh"
    shutil.copyfile(SOURCE, script)
    for path in [scripts / "package-dmg.sh", *[bin_dir / n for n in ("uname", "ditto", "codesign", "xcrun")]]:
        path.write_text(DRIVER)
        path.chmod(0o755)
    app = case / "Erlkonig.app"
    app.mkdir()
    (app / 'Contents').mkdir()
    (app / 'Contents/Info.plist').write_bytes(plistlib.dumps({'CFBundleShortVersionString': '0.1.1', 'CFBundleVersion': '44'}))
    (app / "original").write_text("unchanged input")
    trace = case / "trace.jsonl"
    destination = out / "Erlkonig-0.1.1-44-macos-arm64.dmg"
    if mode == "existing":
        destination.write_text("existing release")
    env = dict(os.environ, PATH=str(bin_dir) + os.pathsep + os.environ["PATH"],
               RELEASE_TEST_TRACE=str(trace), RELEASE_TEST_MODE=mode,
               RELEASE_TEST_DEST=str(destination))
    command = ["/bin/sh", str(script), "--app", str(app), "--out", str(out),
               "--identity", "Developer ID Application: Synthetic Fixture (TEST)",
               "--notary-profile", "synthetic-existing-profile",
               "--source-commit", "a" * 40, "--run-id", "123", "--run-attempt", "1"]
    if mode == "missing-input":
        command = command[:-2]
    if mode == "inside-app":
        command[command.index("--out") + 1] = str(app)
    result = subprocess.run(command, env=env, capture_output=True, text=True, timeout=10)
    rows = [json.loads(line) for line in trace.read_text().splitlines()] if trace.exists() else []
    events = [row["event"] for row in rows]
    assert sorted(p.name for p in app.iterdir()) == ["Contents", "original"], (mode, "input modified")
    assert (app / "original").read_text() == "unchanged input"
    if not mode:
        assert result.returncode == 0, result.stderr
        assert events == ["sign-app", "verify-app", "display-app", "package", "sign-dmg",
                          "verify-dmg", "submit", "staple", "validate", "verify-final"], events
        assert destination.read_text() == "fixture DMG"
        receipts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith('{"notarization":')]
        assert receipts == [{"notarization": {"id": "00000000-0000-4000-8000-000000000000", "status": "Accepted"}}], result.stdout
        receipt = json.loads(Path(str(destination) + '.receipt.json').read_text())
        assert receipt['artifact_sha256'] == hashlib.sha256(destination.read_bytes()).hexdigest()
        assert receipt['artifact_size'] == destination.stat().st_size
        assert receipt['package_version'] == '0.1.1' and receipt['bundle_build'] == '44'
        assert receipt['bundle_plist_sha256'] == hashlib.sha256((app / 'Contents/Info.plist').read_bytes()).hexdigest()
        assert receipt['source_commit'] == 'a' * 40 and receipt['run_id'] == 123 and receipt['run_attempt'] == 1
        assert receipt['notarization']['status'] == 'Accepted'
        assert receipt['stapler_validation'] == {'exit_code': 0, 'output': 'Synthetic stapler success\n'}
        assert 'Authority=Developer ID Application: Fixture' in receipt['codesign_display']
        assert not list(out.glob(".erlkonig-release.*"))
    else:
        assert result.returncode != 0, (mode, result.stdout)
        if mode in ("existing", "race"):
            assert destination.read_text() == {"existing": "existing release", "race": "competitor"}[mode]
        else:
            assert not destination.exists(), (mode, "published failed candidate")
        if mode in ("submit", "invalid", "pending", "malformed", "missing-id", "empty-id", "invalid-id"):
            assert "staple" not in events, (mode, events)
            assert '{"notarization":' not in result.stdout, (mode, result.stdout)
        if mode in ("wrong-identity", "no-timestamp", "no-runtime", "package", "existing"):
            assert "submit" not in events, (mode, events)
        if "submit" in events:
            assert len(list(out.glob(".erlkonig-release.*"))) == 1, (mode, "lost submission evidence")
        else:
            assert not list(out.glob(".erlkonig-release.*")), (mode, "left pre-submission staging")


with tempfile.TemporaryDirectory(prefix="erlkonig-release-stubs-") as temp:
    modes = ["", "missing-input", "inside-app", "sign-app", "verify-app", "display-app",
             "wrong-identity", "no-timestamp", "no-runtime", "package", "existing",
             "sign-dmg", "verify-dmg", "submit", "invalid", "pending", "malformed",
             "missing-id", "empty-id", "invalid-id", "staple", "validate", "verify-final", "race"]
    for mode in modes:
        run_case(Path(temp), mode)
print(f"PASS: {len(modes)} stub-only release ordering/failure/input-preservation/no-clobber cases")
