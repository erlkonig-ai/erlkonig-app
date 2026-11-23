# Run a headless node with Podman

If you received an OCI archive for your Linux architecture, check it against
the supplied checksum file before loading it as your regular user.

```sh
sha256sum --check erlkonig-linux-amd64.oci.tar.gz.sha256
podman load -i erlkonig-linux-amd64.oci.tar.gz
image=localhost/erlkonig:amd64
```

For an ARM64 archive, use `arm64` in both the filename and tag.

To build from source, use a Git checkout on Linux with rootless Podman. On macOS, run the
commands in a Linux guest with rootless Podman. The build script uses the
current Git commit, the repository's pinned public source inputs and
`Cargo.lock`. The default target is the Podman server's native architecture.
An ARM64 host can build for x86-64 with
`./container/build.sh localhost/erlkonig:amd64 amd64`. The compiler runs
natively and cross-links the binary. Running the final image check on a
different architecture requires working guest emulation.

```sh
./container/build.sh
image=localhost/erlkonig:local
```

Start the owner independently of the client.

```sh
podman pod create --name erlkonig --share=net
podman volume create erlkonig-data
podman run -d --pod erlkonig --name erlkonig-owner \
  --read-only --cap-drop=all --security-opt=no-new-privileges \
  -v erlkonig-data:/var/lib/erlkonig "$image"
podman exec erlkonig-owner erlkonig workspaces list --data-dir /var/lib/erlkonig \
  --url http://127.0.0.1:41773/ --output json
```

The last command succeeds once the owner is ready. A previously configured
volume can use a different loopback port, reported in the owner log. The owner runs as UID
10001. Its application directory and home share the named volume, which holds
the signing key, pile, settings and local MCP credential. Keep the volume
when replacing the owner. Stop the original owner gracefully before starting
its replacement with the same volume. The pod shares loopback with its
members and publishes no host port.

To connect Vibe, copy only the owner's MCP credential into a Podman secret.
Run this pipeline in a shell with `pipefail` enabled.

```sh
set -o pipefail
podman exec erlkonig-owner cat /var/lib/erlkonig/mcp.token \
  | podman secret create erlkonig-mcp -
```

Add `--pod erlkonig --secret erlkonig-mcp,type=env,target=ERLKONIG_MCP_TOKEN`
to the Vibe container's `podman run` options. Configure Vibe to use the
owner's reported URL and that environment variable. See the
[Vibe integration](../plugin/vibe/README.md). Give this secret only to
clients trusted with the owner's advertised tools. Workspace selection routes
a call, and shared collections determine which data this particular node can
access. A fresh volume creates a new identity. If you replace or rotate its
credential, re-create the Podman secret and its consumer containers.

For a read-only check using the same image, mount the secret as a private
file for the CLI.

```sh
podman run --rm --pod erlkonig \
  --secret erlkonig-mcp,target=/run/secrets/mcp.token,uid=10001,gid=10001,mode=0600 \
  "$image" tools list --data-dir /run/secrets \
  --url http://127.0.0.1:41773/
```

Run `python3 container/smoke.py` to test a built image with an isolated pod
and a fresh volume. It checks authenticated discovery, writes a disposable
workspace, replaces the owner and reads the workspace back through MCP.
