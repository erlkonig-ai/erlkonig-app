"""Probe installed native HTTP clients using only a newly created scratch HOME.

Usage: host_clients.py lifecycle-test-binary scratch-directory
The fixture is a test-harness mode, never a production executable feature.
Credentials are private local files and are never included in output.
"""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

test_binary, scratch = sys.argv[1:3]
root = Path(scratch).resolve()
root.mkdir(mode=0o700)  # Refuse reuse; never erase a pre-existing profile.
home = root / "home"
data = root / "data"
codex_home = home / ".codex"
claude_home = home / ".claude"
for directory in (home, data, codex_home, claude_home):
    directory.mkdir(mode=0o700)

def private(path, text):
    with open(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "w") as file:
        file.write(text)

env = {k: v for k, v in os.environ.items()
       if not k.startswith("TRIBLESPACE_") and k not in ("PILE", "PERSONA", "ANTHROPIC_API_KEY", "OPENAI_API_KEY")}
env.update(HOME=str(home), CODEX_HOME=str(codex_home), CLAUDE_CONFIG_DIR=str(claude_home),
           ERLKONIG_FORCE_QUIT_FIXTURE=str(data))
codex = "/Applications/ChatGPT.app/Contents/Resources/codex-cli/bin/codex"
claude = str(Path.home() / ".local/bin/claude")
with open(root / "fixture.log", "w") as log:
    fixture = subprocess.Popen([test_binary, "--exact", "force_quit_fixture", "--nocapture"],
                               env=env, cwd=home, stdout=log, stderr=subprocess.STDOUT)
try:
    deadline = time.monotonic() + 20
    while not (data / "fixture-ready").exists():
        if fixture.poll() is not None or time.monotonic() > deadline:
            raise RuntimeError("native test fixture did not become ready")
        time.sleep(0.05)
    url = (data / "fixture-ready").read_text()
    token = (data / "mcp.token").read_text().strip()
    private(codex_home / "config.toml", "[mcp_servers.erlkonig]\nurl = " + json.dumps(url)
            + '\n[mcp_servers.erlkonig.http_headers]\nAuthorization = ' + json.dumps("Bearer " + token) + "\n")
    # Claude Code stores user-scope config alongside its config directory.
    config = {"mcpServers": {"erlkonig": {"type": "http", "url": url,
              "headers": {"Authorization": "Bearer " + token}}}}
    private(home / ".claude.json", json.dumps(config))
    private(claude_home / ".claude.json", json.dumps(config))
    for client in (codex, claude):
        result = subprocess.run([client, "mcp", "list"], env=env, cwd=home,
                                capture_output=True, text=True, timeout=45)
        output = (result.stdout + result.stderr).replace(token, "[redacted]")
        print(Path(client).name + " mcp list:", output.strip())
        assert result.returncode == 0 and "erlkonig" in output
        if client == claude:
            assert "Connected" in output, "Claude Code did not connect over HTTP"
    subprocess.run([sys.executable, str(Path(__file__).with_name("appserver_tools.py")),
                    codex, str(home), str(codex_home)], env=env, cwd=home, check=True, timeout=120)
    print("PASS: scratch Claude Code HTTP connection and ChatGPT app-server native tool call")
finally:
    fixture.kill()
    fixture.wait(timeout=10)
