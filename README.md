# Erlkonig

One macOS menu-bar application, one executable, one resident backend. The
main thread owns the menu; native Rust workers own replication, maintenance
and the authenticated local Faculty MCP endpoint. No node executable,
LaunchAgent or KeepAlive restarter is bundled. A client may start the same
executable in `mcp-stdio` mode: this connection-scoped transport only forwards
to the running app, and never owns a pile or starts a backend.

## One node, named workspaces

One node identity, one logical pile and one local MCP endpoint serve every
workspace. A workspace is a named bundle of collections, not a separate
identity, key or physical data store. Collection capabilities define what the
node can read, write and share.

Ask the assistant to help set up Erlkonig. The node-scoped `config_*` tools
remain available through the authenticated local integration even when no
workspace exists. `config_inspect` shows the public node identity, settings,
current workspace bundles and unavailable revisions. People do not need to
type hexadecimal IDs, select pile/key files or operate a configuration wizard.

`config_create_workspace` creates a named private bundle for the ordinary
faculties. Alternatively, `config_list_collections` discovers existing local
roots and independent READ/WRITE admission; explicit role/root choices create
a bundle over those collections. Names are labels, never authority or an
implicit selection. Discovery does not download data, grant access or scan
every blob; unindexed descriptor-only roots are not yet discoverable.
`config_edit_workspace` changes a bundle and returns its successor request ID.
`config_remove_workspace` withdraws exactly one current revision, including an
unavailable one, without deleting collection data or choosing a concurrent winner.

`config_set_settings` saves the complete typed port, sync, maintenance and
peer-public-ID settings. Peer IDs are bootstrap hints, not sharing permissions.
Port changes require an app restart and client integration refresh; service
settings apply while running. Responses report the actual running `mcp_url`
and whether restart is required. After restart, choose **Integrations → your
client → Refresh package**, then reload the client (or refresh the Cowork
archive); an old-port profile cannot reconnect. Port zero is ephemeral and
requires refreshed integration after every restart. Conflicting service
settings leave the configuration endpoint available on the agreed current
port; if the port itself is missing or conflicting, the app uses local port
41773 for recovery. This does not select or save a settings fork, and node
services remain stopped until explicitly reconciled. A busy recovery port or
damaged/unreadable store still requires local operator recovery.
Fresh nodes enable sync of permitted collections
and maintenance of supported attached collections. **Pause sync / Resume sync**
is also available from the crown menu; it leaves maintenance settings alone.
There is no active-workspace selector, keyring or per-workspace service mode.

Every configuration edit passes the opaque observation returned by
`config_inspect`. The token fingerprints native Config foundations without
exposing their blob handles. Stale edits fail; concurrent native revisions
remain visible for explicit reconciliation. The existing typed Config
operations publish the changes, with no separate configuration store.
Missing access still requires the collection owner to grant the existing node
identity the appropriate capability; configuring a bundle or peer issues no
grant. No configuration tool accepts private keys, tokens or arbitrary paths.

Fresh installations use
`~/.local/share/triblespace/custody/self.pile` and `self.key`.
An existing legacy app pile/key requires an explicit settings migration and
`bind-node`; the app never silently swaps its identity. The application folder
holds only the private `mcp.token`, process lock and optional `node.json`
locator (pile/key paths plus expected public signer). Settings remain in the
pile. A validated binding is captured once at startup; workspaces cannot change it.
**Show storage folder** in the crown menu opens that running node's storage
directory in Finder (the bound pile's parent directory). It does not select,
move or change storage, and is unavailable before node startup or during Quit.
App configuration is typed facts in the node pile's private Faculty `config`
collection; measured metrics live in a separate private collection in that
same pile. There is no parallel mutable workspace database, and raw credentials
are never stored as facts.

`erlkonig-core` owns configuration, credentials, lifecycle and client setup.
`erlkonig` owns the macOS menu and crown. `faculties`, `trible` and
`triblespace-net` are linked libraries, not launched commands. The node's
single sync owner provides its durable Peer and enabled maintenance. With
sync paused, maintenance is local-only and starts no acquisition networking.
Workspace Faculty reads are local-only: missing payloads fail instead of
starting acquisition peers. Native owners and safe pile handles end with the app.

Maintenance uses a dedicated four-worker CPU pool, dependency-first ordering,
and value-change gating at a one-second cadence. Sync owns its startup/periodic
held-blob walker (four workers, 30-minute interval). The in-process resource
guard pauses services at 48 GiB process RSS or below 20% kernel memory headroom
(`kern.memorystatus_level` on macOS, not the separate `memory_pressure`
estimate). The UI remains responsive; service threads use Darwin background
priority. After recovery, use `config_set_settings` to disable both services,
then restore the desired settings to retry. Ordinary workspace edits do not
restart paused services.

Passive operator commands never start transport:

```sh
erlkonig public-key --key /absolute/existing/self.key
erlkonig inspect-node --pile /absolute/existing/self.pile --public PUBLIC_HEX
erlkonig migration-plan --request /absolute/prepared-request.json
```

`migration-plan` preserves source config/metrics facts and conflicting heads.
The explicit request carries source collection handles, target locator,
approved old workspace bindings, service defaults and optional exact existing
role routes. Each added bundle requires an anchor minted once with
`trible genid` and retained in the request. The plan does not import another
pile or infer routes from labels. Unavailable or unapproved old workspaces stay
reported and retained, not silently redirected. Quiesce the old app before
preparing the final request; `migrate-settings` applies only to the target and
rejects target config changes since preparation. Keep the original pile/key
for rollback. `bind-node --data-dir APP_DIR --pile EXISTING_PILE --key
EXISTING_KEY --expect-public PUBLIC_HEX` then writes only the validated locator.

Code's local Git operations use linked, vendored libgit2; no Git executable,
external diff/filter/credential helper, or SSH/HTTPS transport is used. Its
rename-similarity and clock-skewed history ordering can differ from Git CLI.
User-invoked Habit shell predicates remain an explicit subprocess exception.
They are not resident app sidecars, but arbitrary descendants are outside the
native shutdown guarantee.

## Workspace requests

Tell your assistant the workspace to use. Clients call `list_workspaces` to
obtain its opaque request ID; people do not need to copy or transcribe it.
Listing returns labels and configured collection routes, alongside unavailable
entries with reasons. Every ordinary faculty call requires an explicit
`workspace` ID; only node configuration and workspace listing are unscoped.
Switching workspace means choosing another bundle for that request, not changing
a global selection shared with other clients.

The app freezes the resolved bundle for each admitted call. IDs are opaque,
not cryptographic manifests: native additive facts can change a revision's
interpretable value, and subsequent calls use the new value. Normal Config edits
publish a successor and reject the previous ID instead of redirecting it.
Unavailable revisions can be removed with `config_remove_workspace` without deleting data.

Each call uses the node's one signer and only the selected collection routes,
including auxiliary faculty collections. Omitted roles are unavailable; they
never fall back to private or default roots. Requests cannot supply replacement
pile paths, keys or root mappings. Routing is not an execution sandbox for
tools that support host paths or user-invoked commands. Reading workspace A
and writing workspace B are separate explicit operations, not an automatic
copy, union or cross-workspace transaction. Existing collection admission
still applies.

Discord authentication is resolved by the native faculty, not by a token in
the app's environment. Include both the Discord and Secrets roles in the
chosen workspace, provision the encrypted credential through Secrets, and bind
its exact version with `discord_auth_set`. `discord_auth_status` reports only
revision/version references. Concurrent auth revisions require explicit
reconciliation; adding a newer same-named secret does not silently rotate the
bot. Credential plaintext is never an MCP argument or response.

The controller retains native config observations and task ownership.
Metrics writes do not invalidate the config observation. Node configuration
changes cancel and join the old service session before replacement; malformed
control observations stop services instead of guessing. A conflicting workspace
remains unavailable without hiding other workspaces. Removing a workspace
withdraws its configuration, never deletes collection data or changes sync scope.

Nonparticipating standalone daemons do not honor app-private coordination.
An existing node owner must be handed off explicitly before this app starts
owning that same store and endpoint.

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

A stored MCP-port change takes effect on the next launch. Metrics contain cumulative
observed bytes/publications/failures by node, worker and session;
absent counters remain unknown. Measurements are coalesced at a ten-second
cadence, with observed failures and shutdown flushed sooner. Local-only
maintenance does not claim a live network listener. Failed metrics
appends are visible in the menu and are never reported as persisted success.

The crown is static at rest and with macOS Reduce Motion. Animation follows
payload-transfer counters, not connection state or QUIC keepalives. Current
receive progress is published after a reconcile completes, so this is not yet
a continuous in-flight byte meter. Local-only workspace reads add no network traffic.

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
