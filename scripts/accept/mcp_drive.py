"""Run the plugin's MCP command exactly as .mcp.json declares it, send
initialize + tools/list, and summarize. Usage: mcp_drive.py <plugin dir> <HOME>"""
import json, os, subprocess, sys
plugin, home = sys.argv[1], sys.argv[2]
spec = json.load(open(os.path.join(plugin, ".mcp.json")))["mcpServers"]["faculties"]
env = {k: v for k, v in os.environ.items() if not k.startswith("TRIBLESPACE_") and k not in ("PILE", "PERSONA")}
env["HOME"] = home
env["TRIBLESPACE_KEY"] = "/planted/should/not/reach/faculties"
env["PERSONA"] = "planted"
reqs = [
    {"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "erlkonig-test", "version": "0"}}},
    {"jsonrpc": "2.0", "method": "notifications/initialized"},
    {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
]
print("$", spec["command"], " ".join(json.dumps(a) for a in spec["args"]))
p = subprocess.run([spec["command"], *spec["args"]], input="".join(json.dumps(r) + "\n" for r in reqs),
                   capture_output=True, text=True, env=env, cwd="/", timeout=60)
print("exit", p.returncode)
lines = [l for l in p.stdout.splitlines() if l.strip()]
bad = 0
for l in lines:
    try:
        m = json.loads(l)
    except ValueError:
        bad += 1; continue
    if m.get("id") == 1:
        print("initialize ->", json.dumps(m.get("result", m.get("error")))[:200])
    if m.get("id") == 2:
        tools = m["result"]["tools"]
        fac = sorted({t["name"].split("_")[0] for t in tools})
        print(f"tools/list -> {len(tools)} tools across {len(fac)} faculties, {len(json.dumps(tools))} bytes of tool JSON")
print(f"stdout lines {len(lines)}, non-JSON {bad}")
print("stderr:", p.stderr.strip() or "(empty)")
