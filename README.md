# Erlkönig

One macOS menu-bar application, one executable, one resident process. The
main thread owns the menu; native Rust workers own replication, maintenance
and the authenticated local Faculty MCP endpoint. No node executable,
stdio launcher, LaunchAgent or KeepAlive restarter is bundled.

`erlkonig-core` owns configuration, credentials, lifecycle and client setup.
`erlkonig` owns the macOS menu and crown. `faculties`, `trible` and
`triblespace-net` are linked libraries, not launched commands. A durable Peer
serves configured collections; maintenance uses that Peer. Faculty reads use
an existing lazy, non-serving ephemeral Leech when an exact payload is missing.
These native owners and their safe pile handles all end with the app.
Existing user-invoked Faculty commands (including Git and Habit shell
predicates) can still launch external processes. They are not resident app
sidecars, but arbitrary descendants are outside the native shutdown guarantee.

The app uses `~/Library/Application Support/Erlkonig`, with its own empty pile,
signing key and private MCP credential. Explicit `pile`/`key` paths in
`node.toml` must exist; there is no fallback to the person's other stores.
Replication and maintenance are off unless collections are configured.
Peer routes are explicit and never inherited from `TRIBLESPACE_PEERS`.

Quit stops accepting requests, cancels async work and joins workers while the
menu reports shutdown. An admitted synchronous Faculty operation can need time
to drain. After a 15-second grace period, the app terminates its own process;
this final fallback has Force Quit semantics, including no promise to finish
an in-flight write. Open at Login starts this same application, not a daemon.

The local MCP endpoint defaults to `http://127.0.0.1:41773/`. All requests
require a machine-local bearer token. Connect ChatGPT and Connect Claude Code
write private HTTP client configuration only after an explicit menu action.
The bundled plugin template contains no credential and no launcher. Claude
chat's cloud connectors cannot reach loopback. Local Cowork plugin support is
documented, but this app's authenticated HTTP integration is not yet verified
in Cowork and is not claimed by the setup menu.

The crown is static at rest and with macOS Reduce Motion. Animation follows
payload-transfer counters, not connection state or QUIC keepalives. Current
receive progress is published after a reconcile completes; lazy Faculty fetches
are not included, so this is not yet a continuous in-flight byte meter.

## Validation and packaging

Use `scripts/validate-native.sh` for the scoped, reserved Mac release test/build
lane. It uses Rust 1.98, four nice'd jobs and a disposable target directory.
`scripts/bundle.sh --release-dir DIR --out DIR` assembles and signs a bundle
containing only `Contents/MacOS/erlkonig`. It does not launch or install it.

`scripts/accept.sh APP` checks that signed bundle's inventory. Native lifecycle
tests use temporary piles and a test-harness fixture, not a production headless
mode. `scripts/accept/host_clients.py TEST_BINARY SCRATCH_DIR` probes installed
ChatGPT/Codex and Claude Code clients with scratch HOME and client directories.
No validation step requires changing real client profiles or opening the GUI.

The adjacent native dependency repositories must be the frozen source cohort
recorded by the validation run. See `TESTING-for-JP.md` for the remaining
interactive checks; automated success is not a claim those checks occurred.
