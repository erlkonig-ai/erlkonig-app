"""Ask `codex app-server` (the engine inside ChatGPT) for the faculties
server's tool list, and call one native tool in a scratch thread.
Usage: appserver_tools.py <codex> <HOME> <CODEX_HOME> <workspace-revision>

Manual client probe: supply an existing exact workspace revision from the
configured app. This probe neither chooses nor creates a workspace.
"""
import argparse, json, os, queue, re, subprocess, threading, time
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("codex")
parser.add_argument("home")
parser.add_argument("codex_home")
parser.add_argument("workspace")
args = parser.parse_args()
if not re.fullmatch(r"[0-9A-Fa-f]{32}", args.workspace):
    parser.error("workspace must be an exact 32-hex revision returned by list_workspaces")
codex, home, codex_home = args.codex, args.home, args.codex_home
env = {k: v for k, v in os.environ.items() if not k.startswith("TRIBLESPACE_") and k not in ("PILE", "PERSONA")}
env.update(HOME=home, CODEX_HOME=codex_home)
p = subprocess.Popen([codex, "app-server"], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                     stderr=open(os.path.join(codex_home, "appserver.err"), "w"), env=env, cwd=home, text=True)
q = queue.Queue()
def read_output():
    try:
        for line in p.stdout:
            q.put(json.loads(line))
    except (ValueError, OSError) as error:
        q.put(error)
    finally:
        q.put(None)
threading.Thread(target=read_output, daemon=True).start()
def send(m): p.stdin.write(json.dumps(m) + "\n"); p.stdin.flush()
def call(i, method, params, timeout=90):
    send({"id": i, "method": method, "params": params})
    end = time.time() + timeout
    while time.time() < end:
        try: m = q.get(timeout=1)
        except queue.Empty: continue
        if m is None:
            raise RuntimeError(f"app-server closed output before {method} completed")
        if isinstance(m, Exception):
            raise RuntimeError("invalid app-server output") from m
        if not isinstance(m, dict):
            raise RuntimeError("app-server response must be an object")
        if m.get("id") == i: return m
    raise TimeoutError(method)
def required_result(response, method):
    if "error" in response or not isinstance(response.get("result"), dict):
        raise RuntimeError(f"{method} did not return a successful result")
    return response["result"]
try:
    required_result(call(1, "initialize", {"clientInfo": {"name": "erlkonig-test", "version": "0"}}), "initialize")
    print("initialize: ok")
    send({"method": "initialized"})
    status = required_result(call(2, "mcpServerStatus/list", {"serverName": "erlkonig"}), "mcpServerStatus/list")
    if not isinstance(status.get("data"), list):
        raise RuntimeError("mcpServerStatus/list omitted its server list")
    for s in status["data"]:
        tools = s.get("tools") or {}
        print(f"mcpServerStatus/list: server={s['name']} plugin={s.get('pluginId')} serverInfo={json.dumps(s.get('serverInfo'))} "
              f"tools={len(tools)} toolsError={s.get('toolsError')}")
        print("  sample tools:", sorted(tools)[:6])
    if not any(s["name"] == "erlkonig" and "goals_list" in (s.get("tools") or {})
               and not s.get("toolsError") for s in status["data"]):
        raise RuntimeError("native goals_list tool absent or unavailable")
    started = required_result(call(3, "thread/start", {"ephemeral": True, "cwd": home}), "thread/start")
    tid = started.get("thread", {}).get("id")
    if not isinstance(tid, str) or not tid:
        raise RuntimeError("thread/start omitted its thread id")
    c = call(4, "mcpServer/tool/call", {"server": "erlkonig", "threadId": tid, "tool": "goals_list", "arguments": {"workspace": args.workspace}})
    result = required_result(c, "goals_list")
    if result.get("isError") or not isinstance(result.get("content"), list):
        raise RuntimeError("native goals_list call did not succeed")
    print("goals_list in a thread: successful result")
finally:
    p.stdin.close()
    p.wait(timeout=30)
if p.returncode != 0:
    raise RuntimeError(f"app-server exited with status {p.returncode}")
print("app-server exit:", p.returncode)
