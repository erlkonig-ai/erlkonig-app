# Erlkönig

One macOS menu-bar application, one executable, one resident backend. The
main thread owns the menu; native Rust workers own replication, maintenance
and the authenticated local Faculty MCP endpoint. No node executable,
LaunchAgent or KeepAlive restarter is bundled. A client may start the same
executable in `mcp-stdio` mode: this connection-scoped transport only forwards
to the running app, and never owns a pile or starts a backend.

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
require a machine-local bearer token. The stdio transport reads the existing
private token only when connecting; no credential is put in a plugin or client
profile. Quit disconnects the transports, which never relaunch the app.

The **Integrations** submenu shows detected clients: one OpenAI row labelled
for the installed ChatGPT/Codex clients, a **Claude Code plugin** row when its
CLI is available, and a separate Cowork action when Claude Desktop is found.
Desktop detection uses public macOS bundle lookup, not running processes or
account state. An OpenAI app without a management CLI gets a plain “CLI
unavailable…” action. See [detection policy](plugin/README.md) for details.
The local controls' checkmarks mean installed, not enabled or connected. Disabled
and load-error states are labelled separately; unavailable status preserves
the last-known check and disables mutation until a successful refresh.
Inventory runs on startup, after changes, or on **Refresh plugin status**—not
on the crown's display timer. CLI commands run off the main thread, with a
30-second limit each, bounded output and cancellation/child cleanup on Quit.

The OpenAI package targets supported ChatGPT Work/Codex local hosts; its status
is read from the selected Codex CLI and does not certify ChatGPT UI refresh.
Claude Code installation is user-scoped. Neither is an account-install claim.
Removal verifies our exact marketplace source and plugin ID, leaves other
plugins/settings untouched, and keeps the marketplace registered. Existing
direct MCP settings named `erlkonig` are warned about, never removed by guess.

After moving or updating the app, select an installed row and **Refresh
package** to reinstall the current absolute executable path, then enable/reload
the plugin in the client if needed. Refresh is a remove/install sequence; if a
client step fails, the menu reports uncertainty, not a successful rollback.

**Manage Cowork plugin** creates a ZIP of the same Claude package used by Code
and opens the documented account Plugins page after confirmation. Upload and
remove it there manually; there is no checked Cowork state or private account
API. A synced Claude plugin is identified as account-managed and routes here.
This is not a standalone Desktop Chat extension. Cowork's local plugin server
runs through Claude Desktop, not in its shell VM; cloud Cowork can also access
local connectors through the open Desktop app. Actual interactive Cowork
loading remains a validation gate, not a claim made by an installed checkbox.
See [Claude's cross-platform guide](https://support.claude.com/en/articles/15520349-use-claude-cowork-on-web-desktop-and-mobile)
and [plugin lifecycle reference](https://code.claude.com/docs/en/plugins/cli-reference).

Package staging allows only the intended manifest, shared skill and `.mcp.json`;
unexpected files or symlinks are refused rather than executed or distributed.
The ZIP contains exactly those three files and no top-level `bin/` directory.

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
mode. `node scripts/accept/plugin_lifecycle/probe.mjs --codex /path/to/codex
--claude /path/to/claude` exercises plugin lifecycle and stdio in fresh scratch
profiles. Add `--binary /path/to/erlkonig` to test the compiled native transport
against its disposable authenticated HTTP owner. No model request or account
credential is required. See the probe's own output for the precise evidence;
mock transport results are not native binary acceptance. No validation step
requires changing real client profiles or opening the GUI.

The adjacent native dependency repositories must be the frozen source cohort
recorded by the validation run. See `TESTING-for-JP.md` for the remaining
interactive checks; automated success is not a claim those checks occurred.
