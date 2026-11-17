"""Explicitly run a candidate against a new synthetic-only Mac fixture.

No open(1), installed app, live identity, real client CLI or client registry.
Migration modes require explicit native UI choices; --decline-first also records
the operator's observed Not now click separately from machine-verified stopped state. Credential
popups remain covered by separate synthetic core tests.
All owned processes are stopped, including on failure; fixture evidence remains.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import queue
import signal
import socket
import stat
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request

PROTOCOL = "2025-06-18"


def check(condition, message):
    if not condition:
        raise AssertionError(message)


def wait_until(probe, timeout, message):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = probe()
        if result:
            return result
        time.sleep(0.05)
    raise AssertionError(message)


def listening(pid):
    result = subprocess.run(
        ["/usr/sbin/lsof", "-nP", "-a", "-p", str(pid), "-iTCP", "-sTCP:LISTEN", "-Fn"],
        capture_output=True, text=True, timeout=5,
    )
    check(result.returncode in (0, 1), "cannot inspect the owned candidate listener")
    return [line[1:] for line in result.stdout.splitlines() if line.startswith("n")]


def owns_storage(pid, paths):
    result = subprocess.run(
        ["/usr/sbin/lsof", "-nP", "-a", "-p", str(pid), "-Fn"],
        capture_output=True, text=True, timeout=5,
    )
    check(result.returncode in (0, 1), "cannot inspect the owned candidate descriptors")
    names = {line[1:] for line in result.stdout.splitlines() if line.startswith("n")}
    return any(str(path) in names for path in paths)


class HttpMcp:
    def __init__(self, url, token):
        self.url, self.token, self.session, self.counter = url, token, None, 0
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))

    def post(self, message, token=True, origin=None):
        headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream"}
        if token:
            headers["Authorization"] = "Bearer " + self.token
        if origin:
            headers["Origin"] = origin
        if self.session:
            headers.update({"MCP-Session-Id": self.session, "MCP-Protocol-Version": PROTOCOL})
        request = urllib.request.Request(self.url, json.dumps(message).encode(), headers)
        with self.opener.open(request, timeout=10) as response:
            body = response.read()
            self.session = response.headers.get("mcp-session-id", self.session)
            return json.loads(body) if body else None

    def initialize(self):
        result = self.post(initialize())
        check(result["result"]["serverInfo"]["name"] == "faculties", "unexpected native server")
        self.post({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def call(self, tool, arguments):
        self.counter += 1
        return self.post({"jsonrpc": "2.0", "id": self.counter, "method": "tools/call",
                          "params": {"name": tool, "arguments": arguments}})


def initialize():
    return {"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {
        "protocolVersion": PROTOCOL, "capabilities": {},
        "clientInfo": {"name": "isolated-native-acceptance", "version": "1"}}}


def text_result(response):
    check("error" not in response and not response["result"].get("isError"), "native tool call failed")
    return "".join(part["text"] for part in response["result"]["content"] if part["type"] == "text")


ERP_TOOLS = (
    "erp_account_add", "erp_amex_import", "erp_amex_migrate", "erp_invoice_add",
    "erp_review", "erp_classify", "erp_match",
)


def check_public_catalog(client, workspace):
    client.counter += 1
    response = client.post({"jsonrpc": "2.0", "id": client.counter,
                            "method": "tools/list", "params": {}})
    check("error" not in response, "public tool discovery failed")
    result = response.get("result", {})
    tools = result.get("tools", [])
    check(isinstance(tools, list) and len(tools) > 100 and not result.get("nextCursor"),
          "public tool discovery is incomplete")
    names = [tool.get("name") for tool in tools]
    check(all(isinstance(name, str) for name in names), "invalid public tool name")
    check(not any(name.startswith("erp_") for name in names), "private ERP tool advertised")
    for name in ERP_TOOLS:
        response = client.call(name, {"workspace": workspace})
        check(response.get("error", {}).get("code") == -32602
              and response["error"].get("message") == "Unknown tool",
              "private ERP call was not rejected as an unknown tool")
    return len(names)


class Forwarder:
    def __init__(self, executable, fixture, url, env, log, children):
        self.process = subprocess.Popen(
            [str(executable), "mcp-stdio", "--data-dir", fixture["app_data"], "--url", url],
            env=env, cwd=env["HOME"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=log, text=True,
        )
        children.append(self.process)
        self.messages = queue.Queue()

        def read():
            for line in self.process.stdout:
                try:
                    self.messages.put(json.loads(line))
                except ValueError:
                    self.messages.put({"invalid": True})

        threading.Thread(target=read, daemon=True).start()

    def send(self, message):
        self.process.stdin.write(json.dumps(message) + "\n")
        self.process.stdin.flush()

    def response(self, identity):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                message = self.messages.get(timeout=0.1)
            except queue.Empty:
                continue
            check(not message.get("invalid"), "forwarder emitted a non-protocol line")
            if message.get("id") == identity:
                return message
        raise AssertionError("forwarder response deadline")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("candidate", type=Path, help="new candidate executable or .app; never --reuse-app")
    parser.add_argument("--fixture-binary", type=Path, required=True,
                        help="accept-fixture built from the same candidate source/dependency closure")
    parser.add_argument("--root", type=Path, required=True, help="new absolute fixture/evidence directory")
    parser.add_argument("--old-framing", action="store_true",
                        help="exercise supported framing upgrade with retained source and the public node locator; excludes /32 conversion")
    parser.add_argument("--legacy32-writer", type=Path,
                        help="independently built historical 3ca5c1cb example for synthetic Config/workspace /32 acceptance")
    parser.add_argument("--decline-first", action="store_true",
                        help="acknowledge observed native Not now click; machine-check startup drain and unchanged storage, then relaunch for Migrate")
    quit_mode = parser.add_mutually_exclusive_group()
    quit_mode.add_argument("--operator-quit", action="store_true",
                           help="operator clicks native Quit and acknowledges via stdin; excludes operator latency from timing claims")
    quit_mode.add_argument("--signal-quit", action="store_true",
                           help="send owned candidate SIGTERM for shutdown/drain checks; no native UI Quit claim")
    args = parser.parse_args()
    check(not (args.old_framing and args.legacy32_writer), "choose one historical fixture mode")
    needs_upgrade = args.old_framing or args.legacy32_writer is not None
    check(not args.decline_first or needs_upgrade, "--decline-first requires a migration fixture")
    check(sys.platform == "darwin", "this acceptance lane requires macOS")
    candidate = args.candidate.resolve(strict=True)
    executable = candidate / "Contents/MacOS/erlkonig" if candidate.suffix == ".app" else candidate
    executable = executable.resolve(strict=True)
    fixture_binary = args.fixture_binary.resolve(strict=True)
    check(os.access(executable, os.X_OK) and os.access(fixture_binary, os.X_OK), "candidate/helper is not executable")
    check(args.root.is_absolute(), "fixture root must be absolute")
    root = args.root.resolve()
    check(not root.exists(), "refuse any existing fixture root; no reuse")
    os.umask(0o077)
    root.mkdir(mode=0o700)
    for directory in ["home", "codex", "claude", "tmp", "bin"]:
        (root / directory).mkdir(mode=0o700)
    env = {"HOME": str(root / "home"), "CODEX_HOME": str(root / "codex"),
           "CLAUDE_CONFIG_DIR": str(root / "claude"), "TMPDIR": str(root / "tmp"),
           "PATH": str(root / "bin") + ":/usr/bin:/bin:/usr/sbin:/sbin", "LANG": "C.UTF-8"}
    # Inert sentinel configs suppress onboarding. Fake CLIs admit inventory
    # only, so an accidental install/update/removal fails rather than touching
    # any real host client or account. No live environment is inherited.
    (root / "codex/config.toml").write_text('[mcp_servers.erlkonig]\nurl = "http://127.0.0.1:9/"\n')
    (root / "claude/.claude.json").write_text(json.dumps({"mcpServers": {"erlkonig": {"type": "http", "url": "http://127.0.0.1:9/"}}}))
    shim = f'''#!{sys.executable}
import json, os, pathlib, sys
root = pathlib.Path({str(root)!r})
name = pathlib.Path(sys.argv[0]).name
expected = ["--disable", "remote_plugin", "plugin", "list", "--marketplace", "erlkonig-local", "--json"] if name == "codex" else ["plugin", "list", "--json"]
with (root / "client-commands.jsonl").open("a") as log:
    log.write(json.dumps({{"client":name,"args":sys.argv[1:]}}) + "\\n")
if sys.argv[1:] != expected:
    sys.exit(78)
print('{{"installed":[]}}' if name == "codex" else '[]')
'''
    for name in ["codex", "claude"]:
        path = root / "bin" / name
        path.write_text(shim)
        path.chmod(0o700)
    fixture_mode = ["--old-framing"] if args.old_framing else (["--legacy32-input"] if args.legacy32_writer else [])
    fixture_command = [str(fixture_binary), str(root)] + fixture_mode
    prepared = subprocess.run(fixture_command, env=env, cwd=env["HOME"],
                              capture_output=True, text=True, timeout=45, check=True)
    fixture = json.loads(prepared.stdout)
    (root / "fixture.json").write_text(json.dumps(fixture, indent=2) + "\n")
    if args.legacy32_writer:
        writer = args.legacy32_writer.resolve(strict=True)
        produced = subprocess.run([str(writer), str(root)], env=env, cwd=env["HOME"],
                                  capture_output=True, text=True, timeout=120, check=True)
        provenance = json.loads(produced.stdout)
        check(provenance["historical_core"] == "3ca5c1cb5504d8ff60730edfe5bf641b01f44913", "wrong historical writer")
        check(provenance["node_id"] == fixture["node_id"] and provenance["pile"] == fixture["pile"], "historical writer changed the fixture binding")
        provenance["writer_sha256"] = hashlib.sha256(writer.read_bytes()).hexdigest()
        provenance["source_sha256"] = hashlib.sha256(Path(fixture["pile"]).read_bytes()).hexdigest()
        (root / "historical-writer.json").write_text(json.dumps(provenance, indent=2) + "\n")
    pile, key = Path(fixture["pile"]), Path(fixture["key"])
    check(pile.is_relative_to(root) and key.is_relative_to(root), "helper selected storage outside the fixture")
    check(stat.S_IMODE(key.stat().st_mode) == 0o600, "synthetic key is not private")
    key_stat = (key.stat().st_ino, key.stat().st_size, key.stat().st_mtime_ns)
    original_pile = pile
    original_digest = hashlib.sha256(pile.read_bytes()).hexdigest()
    locator_path = Path(fixture["app_data"]) / "node.json"
    original_locator = locator_path.read_bytes() if needs_upgrade else None
    original_siblings = set(pile.parent.iterdir())
    activated_pile = None
    children = []
    logs = []

    def spawn(label):
        log = (root / (label + ".stderr")).open("wb")
        logs.append(log)
        process = subprocess.Popen([str(executable)], env=env, cwd=env["HOME"],
                                   stdin=subprocess.DEVNULL, stdout=log, stderr=log)
        children.append(process)
        return process

    def start(label):
        process = spawn(label)
        if needs_upgrade and label == "first":
            print("In the isolated candidate, choose Migrate in 'Update local data?'. This fixture contains no live data.", flush=True)

        def ready():
            check(process.poll() is None, "candidate exited before readiness")
            addresses = listening(process.pid)
            if not addresses:
                return None
            check(len(addresses) == 1 and addresses[0].startswith("127.0.0.1:"), "unexpected candidate listeners")
            token_file = Path(fixture["app_data"]) / "mcp.token"
            if not token_file.exists():
                return None
            token = token_file.read_text().strip()
            url = "http://" + addresses[0] + "/"
            client = HttpMcp(url, token)
            client.initialize()
            return process, client

        return wait_until(ready, 120 if needs_upgrade and label == "first" else 20,
                          "candidate readiness deadline (migration consent may still be pending)")

    def stopped(process, url):
        process.wait(timeout=15)
        check(process.returncode == 0, "candidate did not exit cleanly")
        address = urllib.parse.urlsplit(url)
        try:
            connection = socket.create_connection((address.hostname, address.port), timeout=0.2)
        except OSError:
            pass
        else:
            connection.close()
            raise AssertionError("candidate listener survived Quit")

    def same_identity(client):
        inspected = json.loads(text_result(client.call("config_inspect", {})))
        check(inspected["node_id"] == fixture["node_id"], "identity changed")
        check(inspected["settings"]["mcp_port"] == 0, "fixture port is not ephemeral")
        check(not inspected["settings"]["sync_enabled"] and not inspected["settings"]["maintenance_enabled"], "fixture services enabled")
        check(not inspected["runtime"]["sync_running"] and not inspected["runtime"]["maintenance_running"], "fixture started network or maintenance services")
        check(inspected["mcp_url"] == client.url, "candidate endpoint does not match its own listener")
        check((key.stat().st_ino, key.stat().st_size, key.stat().st_mtime_ns) == key_stat, "key file changed")
        if needs_upgrade:
            locator = json.loads((Path(fixture["app_data"]) / "node.json").read_text())
            selected = Path(locator["pile"])
            check(selected != original_pile and selected.parent == original_pile.parent, "upgrade did not select a fresh sibling")
            check(locator["key"] == str(key) and locator["expected_signer"] == fixture["node_id"], "upgrade changed identity binding")
            check(hashlib.sha256(original_pile.read_bytes()).hexdigest() == original_digest, "upgrade changed the retained source")
            if activated_pile is not None:
                check(selected == activated_pile, "restart repeated the framing upgrade")

    try:
        if args.decline_first:
            declined = spawn("declined")
            print("In the isolated candidate, click Not now in 'Update local data?'. Enter declined here only after observing that click. The harness will separately verify startup has drained and services remain stopped; it does not claim a visual stopped-state check.", flush=True)
            check(input().strip() == "declined", "explicit observed native Not now acknowledgment required")
            custody = [*original_siblings, Path(fixture["app_data"]) / "node.lock"]

            def startup_drained():
                check(declined.poll() is None, "declined candidate exited unexpectedly")
                return not owns_storage(declined.pid, custody)

            wait_until(startup_drained, 10, "declined candidate still holds startup custody handles")
            check(declined.poll() is None, "declined candidate exited unexpectedly")
            check(not listening(declined.pid), "declined candidate opened a listener")
            check(not (Path(fixture["app_data"]) / "mcp.token").exists(), "declined candidate created a service credential")
            check(locator_path.read_bytes() == original_locator, "decline changed the locator")
            check(hashlib.sha256(original_pile.read_bytes()).hexdigest() == original_digest, "decline changed the original pile")
            check(set(pile.parent.iterdir()) == original_siblings, "decline created a migration candidate")
            check((key.stat().st_ino, key.stat().st_size, key.stat().st_mtime_ns) == key_stat, "decline changed the key")
            declined.send_signal(signal.SIGTERM)
            declined.wait(timeout=15)
            check(declined.returncode == 0, "declined candidate did not quit cleanly")
            (root / "decline-result.json").write_text(json.dumps({
                "status": "passed", "native_ui_observation": "operator acknowledged clicking Not now",
                "stopped_state_verification": "machine-verified after startup custody handles released; visual stopped-state not checked",
                "candidate_pid": declined.pid,
                "checks": ["startup custody handles released", "original unchanged", "locator unchanged", "key unchanged", "no candidate", "no service credential", "no listener", "clean Quit"]
            }, indent=2) + "\n")
        first, client = start("first")
        same_identity(client)
        if needs_upgrade:
            activated_pile = Path(json.loads((Path(fixture["app_data"]) / "node.json").read_text())["pile"])
            pile = activated_pile
        for authorization, origin, expected in [(False, None, 401), (True, "https://untrusted.invalid", 403)]:
            try:
                HttpMcp(client.url, client.token).post(initialize(), token=authorization, origin=origin)
                raise AssertionError("unsafe HTTP request was admitted")
            except urllib.error.HTTPError as error:
                check(error.code == expected, "unexpected HTTP authorization result")
        check(client.call("compass_list", {})["error"]["code"] == -32602, "implicit workspace admitted")
        listed = json.loads(text_result(client.call("list_workspaces", {})))
        check(any(workspace["workspace"] == fixture["workspace"] for workspace in listed["workspaces"]), "fixture workspace absent")
        selection = {"workspace": fixture["workspace"]}
        public_tool_count = check_public_catalog(client, fixture["workspace"])
        text_result(client.call("compass_list", selection))
        check("(no structured items)" in text_result(client.call("secrets_items", selection)), "unexpected native item")
        duplicate = spawn("duplicate")
        time.sleep(0.5)
        check(duplicate.poll() is None and not listening(duplicate.pid), "duplicate acquired an endpoint")
        check(not owns_storage(duplicate.pid, [pile, key]), "duplicate opened synthetic storage")
        same_identity(client)
        duplicate.send_signal(signal.SIGTERM)
        duplicate.wait(timeout=10)
        check(duplicate.returncode == 0, "duplicate did not quit cleanly")
        forward_log = (root / "forwarder.stderr").open("wb")
        logs.append(forward_log)
        forwarding = Forwarder(executable, fixture, client.url, env, forward_log, children)
        forwarding.send(initialize())
        check("result" in forwarding.response(0), "forwarder initialize failed")
        forwarding.send({"jsonrpc": "2.0", "method": "notifications/initialized"})
        forwarding.send({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                         "params": {"name": "compass_list", "arguments": selection}})
        text_result(forwarding.response(1))
        check(not listening(forwarding.process.pid), "forwarder became a listener owner")
        check(not owns_storage(forwarding.process.pid, [pile, key]), "forwarder opened storage or key")
        token = client.token
        first.send_signal(signal.SIGTERM)
        stopped(first, client.url)
        forwarding.process.wait(timeout=5)
        second, client = start("second")
        same_identity(client)
        check(client.token == token, "local MCP credential changed on restart")
        check(forwarding.process.poll() is not None, "old forwarder reconnected to a replacement owner")
        address = urllib.parse.urlsplit(client.url)
        pending = socket.create_connection((address.hostname, address.port), timeout=1)
        pending.sendall(("POST / HTTP/1.1\r\nHost: " + address.netloc + "\r\nAuthorization: Bearer " + client.token
                         + "\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nContent-Length: 1000\r\n\r\n{").encode())
        time.sleep(0.1)
        before = time.monotonic()
        if args.signal_quit:
            second.send_signal(signal.SIGTERM)
        elif args.operator_quit:
            print("Stalled synthetic HTTP request is ready. Click Quit in the isolated candidate's native menu, then enter quit here after observing the action.", flush=True)
            check(input().strip() == "quit", "explicit observed native Quit acknowledgment required")
        else:
            command = 'ObjC.import("AppKit"); $.NSRunningApplication.runningApplicationWithProcessIdentifier(' + str(second.pid) + ').terminate'
            accepted = subprocess.run(["/usr/bin/osascript", "-l", "JavaScript", "-e", command],
                                      env=env, capture_output=True, text=True, timeout=10)
            check(accepted.returncode == 0 and "true" in accepted.stdout, "AppKit termination was not accepted")
        stopped(second, client.url)
        pending.close()
        if not args.operator_quit:
            check(3 < time.monotonic() - before < 10, "stalled HTTP body did not use the bounded Quit drain")
        final, client = start("final")
        same_identity(client)
        final.send_signal(signal.SIGTERM)
        stopped(final, client.url)
        commands = [json.loads(line) for line in (root / "client-commands.jsonl").read_text().splitlines()]
        check({entry["client"] for entry in commands} == {"codex", "claude"}, "fake client inventory was not observed")
        for entry in commands:
            expected = (["--disable", "remote_plugin", "plugin", "list", "--marketplace", "erlkonig-local", "--json"]
                        if entry["client"] == "codex" else ["plugin", "list", "--json"])
            check(entry["args"] == expected, "candidate attempted an unrequested client mutation")
        (root / "result.json").write_text(json.dumps({"status": "passed", "node_id": fixture["node_id"],
            "public_tool_count": public_tool_count, "unknown_erp_calls_rejected": list(ERP_TOOLS),
            "native_decline_observed_and_verified": args.decline_first,
            "operator_native_quit_observed": args.operator_quit,
            "signal_shutdown_tested": args.signal_quit,
            "supported_framing_upgrade": args.old_framing, "canonical_32_conversion_tested": args.legacy32_writer is not None,
            "checks": ["authenticated HTTP", "explicit workspace", "public catalog excludes ERP and rejects ERP calls", "native item tools", "duplicate refusal",
                       "stdio forwarding only", "shutdown closes listener and forwarder",
                       "bounded signal shutdown drain" if args.signal_quit else ("native Quit with bounded cleanup wait" if args.operator_quit else "bounded AppKit drain"), "same identity on relaunch"]}, indent=2) + "\n")
        print("isolated candidate acceptance passed; no fixture app left running")
    finally:
        for process in reversed(children):
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=5)
        for log in logs:
            log.close()


if __name__ == "__main__":
    main()
