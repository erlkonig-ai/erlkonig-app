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
threading.Thread(target=lambda: [q.put(json.loads(l)) for l in p.stdout], daemon=True).start()
def send(m): p.stdin.write(json.dumps(m) + "\n"); p.stdin.flush()
def call(i, method, params, timeout=90):
    send({"id": i, "method": method, "params": params})
    end = time.time() + timeout
    while time.time() < end:
        try: m = q.get(timeout=1)
        except queue.Empty: continue
        if m.get("id") == i: return m
    raise TimeoutError(method)
r = call(1, "initialize", {"clientInfo": {"name": "erlkonig-test", "version": "0"}})
print("initialize:", "ok" if "result" in r else r)
send({"method": "initialized"})
r = call(2, "mcpServerStatus/list", {"serverName": "erlkonig"})
if "error" in r:
    print("mcpServerStatus/list ERROR", r["error"])
else:
    for s in r["result"]["data"]:
        tools = s.get("tools") or {}
        print(f"mcpServerStatus/list: server={s['name']} plugin={s.get('pluginId')} serverInfo={json.dumps(s.get('serverInfo'))} "
              f"tools={len(tools)} toolsError={s.get('toolsError')}")
        print("  sample tools:", sorted(tools)[:6])
    assert any(s["name"] == "erlkonig" and s.get("tools") for s in r["result"]["data"]), "native HTTP server absent"
r = call(3, "thread/start", {"ephemeral": True, "cwd": home})
if "error" in r:
    print("thread/start ERROR", json.dumps(r["error"])[:300])
else:
    tid = r["result"]["thread"]["id"]
    c = call(4, "mcpServer/tool/call", {"server": "erlkonig", "threadId": tid, "tool": "goals_list", "arguments": {"workspace": args.workspace}})
    print("goals_list in a thread:", json.dumps(c.get("result", c.get("error")))[:200])
    assert "result" in c and not c["result"].get("isError"), "native tool call failed"
p.stdin.close()
p.wait(timeout=30)
print("app-server exit:", p.returncode)
