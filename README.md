# Erlkönig app (milestone 1)

A macOS menu-bar app that owns the person's pile, key, sync and maintenance,
and installs thin plugins into ChatGPT and Claude.

- `crates/erlkonig-core`: data directory, `node.toml`, `node.status`,
  supervision with backoff, plugin install logic.
- `crates/erlkonig-node`: the supervisor (LaunchAgent), and `mcp` mode, which
  the plugins run to start `faculties mcp` on the app's pile.
- `crates/erlkonig`: the menu-bar UI (tray-icon + muda on winit).
- `plugin/`: the plugin folder bundled as `Contents/Resources/plugin`
  (manifests, `.mcp.json`, skill; no binary).
- `macos/`: `Info.plist` and the node's LaunchAgent plist.
- `scripts/bundle.sh`: assembles and ad-hoc signs `Erlkonig.app`.

The app links neither triblespace nor faculties; it runs the bundled `trible`
and `faculties` executables. See `erlkonig-node help` for the node's commands
and `crates/erlkonig-core/src/config.rs` for `node.toml`.
