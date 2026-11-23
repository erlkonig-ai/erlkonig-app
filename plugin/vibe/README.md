# Erlkonig with Mistral Vibe CLI

Use the running headless Erlkonig node from Vibe in the same Podman pod.
The two containers share loopback. Only Erlkonig needs its data volume.
See the [container setup](../../container/README.md) for starting the node
and passing its bearer to the Vibe container as `ERLKONIG_MCP_TOKEN`.
Use a persistent Vibe home if you want the setup to survive container replacement.

## Connect

Run this once in the Vibe environment. Use the node's configured port if it
differs from the default.

```sh
vibe mcp add erlkonig --url http://127.0.0.1:41773/ \
  --transport streamable-http --api-key-env ERLKONIG_MCP_TOKEN
```

Vibe saves the environment variable's **name** in its user configuration and
reads the bearer when it connects. Supply the variable to the Vibe process
each time it starts. Vibe 3.1.4's plugin HTTP manifest supports literal headers
only, so the MCP connection uses Vibe's credential-aware CLI configuration.

## Install the Faculties skill

From the root of this checkout, in the environment that owns your Vibe home:

```sh
plugin_dir="${VIBE_HOME:-$HOME/.vibe}/plugins/erlkonig"
mkdir -p "$plugin_dir/skills/faculties"
install -m 644 plugin/vibe/plugin.json "$plugin_dir/plugin.json"
install -m 644 plugin/skills/faculties/SKILL.md "$plugin_dir/skills/faculties/SKILL.md"
```

Start Vibe, or reload the plugin with `/reload-plugins`. Use `/plugins` to
find `erlkonig` and `/erlkonig:faculties` to open its skill. Use `/mcp erlkonig`
to see the server's tools. To verify an actual read, ask Vibe to call
`erlkonig_list_workspaces`. A fresh node can return an empty workspace list.

## Update or remove

To update the skill, repeat the two `install` commands and reload the plugin.
If the port or environment variable name changes, remove the MCP registration
and repeat the connection command with the new settings.

To uninstall both pieces, run:

```sh
vibe mcp remove erlkonig
plugin_dir="${VIBE_HOME:-$HOME/.vibe}/plugins/erlkonig"
rm "$plugin_dir/plugin.json" "$plugin_dir/skills/faculties/SKILL.md"
rmdir "$plugin_dir/skills/faculties" "$plugin_dir/skills" "$plugin_dir"
```

Restart Vibe to refresh the session. To reinstall, repeat **Connect** and
**Install the Faculties skill**.

[Vibe MCP configuration](https://docs.mistral.ai/vibe/code/cli/mcp-servers)
and [Vibe plugin discovery](https://docs.mistral.ai/vibe/code/cli/plugins).
