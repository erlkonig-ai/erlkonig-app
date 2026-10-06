"""Launch a verified Mac candidate, exercise its native graceful quit, relaunch.

Usage: app_lifetime.py /path/to/Erlkonig.app
Requires explicit delivery authority: creates only the app's own default data.
Never edits client configuration or login items. Leaves the final app open.
"""
import json
import os
from pathlib import Path
import signal
import socket
import subprocess
import sys
import time
import urllib.error
import urllib.request

app = Path(sys.argv[1]).resolve()
executable = str(app / "Contents/MacOS/erlkonig")
data = Path.home() / "Library/Application Support/Erlkonig"

def processes():
    output = subprocess.check_output(["/bin/ps", "-axo", "pid=,comm="], text=True)
    return [(int(line.strip().split(None, 1)[0]), line.strip().split(None, 1)[1])
            for line in output.splitlines() if len(line.strip().split(None, 1)) == 2]

assert not any(Path(command).name in ("erlkonig", "erlkonig-node") for _, command in processes()), "another Erlkonig is already running"
assert not data.exists(), "first-run smoke requires an absent app data directory; never replace existing user data"
subprocess.run(["/usr/bin/codesign", "--verify", "--deep", "--strict", str(app)], check=True)
active = None
success = False

def start():
    global active
    subprocess.run(["/usr/bin/open", "-n", str(app)], check=True)
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        matches = [pid for pid, command in processes() if command == executable]
        assert len(matches) <= 1, "more than one candidate process"
        if matches:
            active = matches[0]
        if active and (data / "mcp.token").exists():
            try:
                with socket.create_connection(("127.0.0.1", 41773), timeout=0.2):
                    return active
            except OSError:
                pass
        time.sleep(0.05)
    raise RuntimeError("native Mac app did not become ready")

def request(token):
    message = {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-06-18", "capabilities": {},
        "clientInfo": {"name": "erlkonig-app-lifetime", "version": "1"}}}
    headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
    if token:
        headers["Authorization"] = "Bearer " + token
    return urllib.request.urlopen(urllib.request.Request("http://127.0.0.1:41773/", json.dumps(message).encode(), headers), timeout=5)

try:
    first = start()
    try:
        request(None)
        raise AssertionError("unauthenticated app endpoint accepted a request")
    except urllib.error.HTTPError as error:
        assert error.code == 401
    token = (data / "mcp.token").read_text().strip()
    with request(token) as response:
        assert json.load(response)["result"]["serverInfo"]["name"] == "faculties"
    key_stat = (data / "self.key").stat()
    assert key_stat.st_mode & 0o777 == 0o600
    ppid = subprocess.check_output(["/bin/ps", "-axo", "ppid="], text=True)
    assert str(first) not in (line.strip() for line in ppid.splitlines()), "idle app has helper children"
    print("first launch:", first, executable, "authenticated MCP ready; no children", flush=True)
    before = time.monotonic()
    os.kill(first, signal.SIGTERM)  # The app's own flag handler takes its normal Quit path.
    while any(pid == first for pid, _ in processes()) and time.monotonic() - before < 17:
        time.sleep(0.05)
    assert not any(pid == first for pid, _ in processes()), "native app did not quit"
    active = None
    try:
        connection = socket.create_connection(("127.0.0.1", 41773), timeout=0.2)
    except OSError:
        pass
    else:
        connection.close()
        raise AssertionError("MCP listener survived Quit")
    print("graceful SIGTERM/Quit path:", round(time.monotonic() - before, 3), "seconds; listener closed", flush=True)
    second = start()
    now = (data / "self.key").stat()
    assert (now.st_ino, now.st_size, now.st_mtime_ns) == (key_stat.st_ino, key_stat.st_size, key_stat.st_mtime_ns), "key file changed on restart"
    with request(token) as response:
        assert json.load(response)["result"]["serverInfo"]["name"] == "faculties"
    print("relaunch:", second, "same private key file and MCP credential; final app left running", flush=True)
    success = True
finally:
    if not success and active and any(pid == active and command == executable for pid, command in processes()):
        os.kill(active, signal.SIGTERM)
