# Erlkönig

One macOS menu-bar application, one executable, one resident process. The
main thread owns the menu; native Rust workers own replication, maintenance
and the authenticated local Faculty MCP endpoint. No node executable,
stdio launcher, LaunchAgent or KeepAlive restarter is bundled.

`erlkonig-core` owns configuration, credentials, lifecycle and client setup.
`erlkonig` owns the macOS menu and crown. `faculties`, `trible` and
`triblespace-net` are linked libraries, not launched commands. Each syncing pile
owns a durable Peer; enabled maintenance shares it. With sync off, maintenance
uses a resident-only Pile, with no network acquisition. Faculty reads use
an existing lazy, non-serving ephemeral Leech when an exact payload is missing.
These native owners and their safe pile handles all end with the app.
Code's local Git operations use linked, vendored libgit2; no Git executable,
external diff/filter/credential helper, or SSH/HTTPS transport is used. Its
rename-similarity and clock-skewed history ordering can differ from Git CLI.
User-invoked Habit shell predicates remain an explicit subprocess exception.
They are not resident app sidecars, but arbitrary descendants are outside the
native shutdown guarantee.

The app uses `~/Library/Application Support/Erlkonig/self.pile` as its one node
pile, retaining the existing `self.key` and private `mcp.token`. The only
bootstrap is these fixed paths. App configuration is typed facts in the node
pile's existing private Faculty `config` collection; measured metrics live in
a separate private collection in that same pile. There is no `node.toml` or
parallel configuration file, and raw credentials are never stored as facts.

Choose **Managed Piles → Add existing pile…** to explicitly pick an existing
pile and signing-key file. The editor accepts peer IDs and exact collection
handles, not ambiguous collection names. Each entry has independent **Sync**
and **Local maintenance** controls, both initially off, plus its status and
configuration editor. No other pile is discovered, created, attached or taken
over automatically. Routes never inherit `TRIBLESPACE_PEERS`.

The controller retains native config observations and task ownership, not a
second configuration database. Metrics writes do not invalidate the config
observation. Changes cancel and join the old session before replacement;
read faults stop managed services, while an individual conflicting entry stays
off without stopping unrelated entries. Concurrent config edits remain visible
conflicts; an editor opened on an older observation refuses to overwrite it.
Removing an entry appends a withdrawal revision and never deletes pile/key files.

Duplicate pile file identities (including hard links/symlinks) and simultaneous
sync sessions sharing a signing identity are refused. App-private advisory
leases complement the existing data-owner lock; they are coordination, not
configuration. Nonparticipating standalone daemons do not honor these leases:
their ownership must be handed off separately before enabling a managed pile.
The node pile itself is not a second sync session. Maintenance-only sessions on
different files may share a signer.

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

MCP remains exclusively attached to the node pile; managed-pile routing and
automatic unions across data worlds are deliberately not implemented. A stored
MCP-port change takes effect on the next launch. Metrics contain cumulative
observed bytes/publications/failures by managed entry, worker and session;
absent counters remain unknown. Measurements are coalesced at a ten-second
cadence, with observed failures and shutdown flushed sooner. The managed
signing identity and the collecting node are distinct telemetry subjects;
maintenance-only roles do not claim a live network listener. Failed metrics
appends are visible in the menu and are never reported as persisted success.

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
