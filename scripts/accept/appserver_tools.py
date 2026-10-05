"""Ask `codex app-server` (the engine inside ChatGPT) for the faculties
server's tool list, and call one tool in a thread.
Usage: appserver_tools.py <codex> <HOME> <CODEX_HOME>"""
import json, os, queue, subprocess, sys, threading, time
codex, home, codex_home = sys.argv[1:4]
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
r = call(2, "mcpServerStatus/list", {"serverName": "faculties"})
if "error" in r:
    print("mcpServerStatus/list ERROR", r["error"])
else:
    for s in r["result"]["data"]:
        tools = s.get("tools") or {}
        print(f"mcpServerStatus/list: server={s['name']} plugin={s.get('pluginId')} serverInfo={json.dumps(s.get('serverInfo'))} "
              f"tools={len(tools)} toolsError={s.get('toolsError')}")
        print("  sample tools:", sorted(tools)[:6])
r = call(3, "thread/start", {"ephemeral": True, "cwd": home})
if "error" in r:
    print("thread/start ERROR", json.dumps(r["error"])[:300])
else:
    tid = r["result"]["thread"]["id"]
    c = call(4, "mcpServer/tool/call", {"server": "faculties", "threadId": tid, "tool": "compass_list", "arguments": {}})
    print("compass_list in a thread:", json.dumps(c.get("result", c.get("error")))[:200])
p.stdin.close()
p.wait(timeout=30)
print("app-server exit:", p.returncode)
