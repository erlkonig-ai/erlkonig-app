"""Exercise a rootless headless image using disposable Podman resources."""
from contextlib import ExitStack
import json
import os
import subprocess
import sys
import time
import uuid


def podman(*args, check=True):
    return subprocess.run(["podman", *args], text=True, stdout=subprocess.PIPE, check=check)


def main():
    image = sys.argv[1] if len(sys.argv) > 1 else "localhost/erlkonig:local"
    if os.getuid() == 0 or podman("info", "--format", "{{.Host.Security.Rootless}}").stdout.strip() != "true":
        sys.exit("Run this check with rootless Podman.")
    arch = podman("image", "inspect", "--format", "{{.Architecture}}", image).stdout.strip()
    name = "erlkonig-smoke-" + uuid.uuid4().hex
    owner, volume, secret = (name + "-" + s for s in ("owner", "data", "token"))
    url = "http://127.0.0.1:41773/"
    restrictions = ["--pull=never", "--platform", "linux/" + arch, "--read-only",
                    "--cap-drop=all", "--security-opt=no-new-privileges"]

    def start():
        podman("run", "-d", "--pod", name, "--name", owner, *restrictions,
               "-v", volume + ":/var/lib/erlkonig", image)
        deadline = time.monotonic() + 180
        while time.monotonic() < deadline:
            if podman("inspect", "--format", "{{.State.Running}}", owner).stdout.strip() != "true":
                break
            ready = subprocess.run(["podman", "exec", owner, "erlkonig", "workspaces", "list",
                                    "--data-dir", "/var/lib/erlkonig", "--url", url],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            if ready.returncode == 0:
                return
            time.sleep(1)
        raise RuntimeError("The disposable owner's authenticated loopback endpoint did not become ready.")

    def client(*args):
        result = podman("run", "--rm", "--pod", name, *restrictions, "--secret",
                        secret + ",target=/run/secrets/mcp.token,uid=10001,gid=10001,mode=0600",
                        image, *args, "--url", url, "--data-dir", "/run/secrets", "--output", "json")
        return json.loads(result.stdout)

    def call(tool, args):
        result = client("call", tool, "--json", json.dumps(args))
        return json.loads(result["content"][0]["text"])

    with ExitStack() as cleanup:
        podman("volume", "create", volume)
        cleanup.callback(podman, "volume", "rm", volume, check=False)
        podman("pod", "create", "--name", name, "--share=net", "--network=none")
        cleanup.callback(podman, "pod", "rm", "--force", name, check=False)
        start()
        assert podman("exec", owner, "id", "-u").stdout.strip() == "10001"
        with subprocess.Popen(["podman", "exec", owner, "cat", "/var/lib/erlkonig/mcp.token"],
                              stdout=subprocess.PIPE) as token:
            subprocess.run(["podman", "secret", "create", secret, "-"], stdin=token.stdout,
                           stdout=subprocess.DEVNULL, check=True)
            cleanup.callback(podman, "secret", "rm", secret, check=False)
            if token.wait():
                raise RuntimeError("Unable to read the disposable owner's MCP credential.")
        assert any(tool["name"] == "list_workspaces" for tool in client("tools", "list"))
        assert not client("workspaces", "list")["workspaces"]
        before = call("config_inspect", {})
        created = call("config_create_workspace", {"observation": before["observation"], "label": "Podman smoke"})
        workspace = created["workspace"]
        client("call", "goals_list", "--workspace", workspace)

        podman("stop", "--time", "45", owner)
        assert podman("inspect", "--format", "{{.State.ExitCode}}", owner).stdout.strip() == "0"
        podman("rm", owner)
        start()
        assert call("config_inspect", {})["node_id"] == before["node_id"]
        assert any(row["workspace"] == workspace for row in client("workspaces", "list")["workspaces"])
        client("call", "goals_list", "--workspace", workspace)
        print("Rootless UID 10001, authenticated MCP and workspace retention after owner recreation passed.")


if __name__ == "__main__":
    main()
