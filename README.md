# Erlkonig

One macOS menu-bar application, one executable, one resident backend. The
main thread owns the menu; native Rust workers own replication, maintenance
and the authenticated local Faculty MCP endpoint. No node executable,
LaunchAgent or KeepAlive restarter is bundled. A client may start the same
executable in `mcp-stdio` mode: this connection-scoped transport only forwards
to the running app, and never owns a pile or starts a backend.

## Local secret entry

`secrets_enter_local` takes the exact `workspace` ID from `list_workspaces`,
`service` and `name` (each 1–160 visible characters), and optional `kind`
(`login`, the default, or `secret`). These are presentation metadata, not
credentials. Do not put a username, password, token or seed in tool arguments
or chat. On macOS the running app shows the selected workspace label and exact
ID, service and storage name, with local Save/Cancel fields. The selected
Secrets route and native WRITE admission are checked before collecting input.

Login uses a username field and a native secure password field. Save creates a
native structured encrypted item with separate username/password semantic roles,
not a JSON body. The storage name is its explicit public label; field values
stay encrypted. Existing raw secret versions are not rewritten or reinterpreted.
Generic secret entry uses one native secure field and encrypts its UTF-8 bytes
without JSON framing. Both login fields, or the generic value, must contain at
least one character. An empty-field Save returns a fixed error asking for a new
local entry and publishes nothing; values are not trimmed or echoed in errors.
No TOTP generation, password change, credential registry
or grant operation is added. Existing collection and Secrets delivery semantics
are unchanged; keep credentials out of an invoice-only shared workspace bundle.

The call waits on the existing app connection. Keep it open until the person
answers. A successful response is only `{"status":"saved","secret":"<ID>"}`;
Cancel returns `{"status":"cancelled"}` and publishes nothing. An unsupported
shell or stopped bridge returns `{"status":"unavailable"}`. The in-process
bridge accepts one prompt at a time; another simultaneous entry fails busy.
The HTTP worker remains sequential and can queue calls behind that prompt.
No prompt job or receipt is persisted, and nothing is retried automatically.

Shutdown closes queued and visible entry before HTTP drain. A Save already
admitted before shutdown may complete. Disconnecting or sending MCP cancellation
does not undo an admitted call; if the response is lost, its outcome is unknown.
`outcome_unknown` likewise means storage did not report success, possibly after
commit. Inspect `secrets_items` metadata for login or `secrets_list` for generic
entry before deciding whether to enter again; never decrypt values to verify Save.
Owned Rust input buffers are zeroized where feasible and controls are cleared;
AppKit/editor/intermediate copies are not guaranteed to be erased.

For metadata-only inspection, use `secrets_item_inspect` with an exact version
and ciphertext body; `secrets_item_open` without a body lists body alternatives
only. With an exact body, `secrets_item_open` exports a PRIVATE FACTS archive,
including plaintext inline integer/flag values. That is an explicitly authorized
body disclosure, not metadata-only inspection. Out-of-line text and attachment
children remain ciphertext handles. Explicit `secrets_item_export` additionally
selects an exact ciphertext child handle and encoding and reveals that child's
plaintext to the caller. Never export private facts or values to verify Save.
Unknown/repeated roles and body alternatives remain native relations, not a
single chosen login or JSON export.
Legacy consumers of raw `secrets_get` continue to receive the original bytes,
but must explicitly adopt native item APIs to consume new local login entries.

## Protected local Secrets candidate (not deployed)

`secrets_enter_local` additionally accepts `protected: true`. The native form
collects and confirms an independent app passphrase (1–1024 UTF-8 bytes), never
the Mac login password. A new protected version has its own random DEK, wrapped
by an Argon2id-derived passphrase key; there is no node/agent-key recipient wrap.
Existing versions are not converted, deleted, or made retrospectively private.
Keep the passphrase separately: this candidate has no recovery reset, and losing
it loses access. Omit the flag for unchanged ordinary agent-accessible entry.

Protected explicit Secrets get/open/inspect/export/TOTP operations detach their
exact encrypted request, release the store lease, and ask locally. The prompt
names the operation and its disclosure. Approval returns a request-bound key,
not an unlocked vault; the operation reopens and rechecks its immutable binding.
Plaintext output is available to the calling integration after approval. The
shared MCP bearer token does not authenticate an individual named agent. Direct
Mail/Discord/other low-level consumers have not been adapted: protected reads
there fail closed, without silently using ordinary agent-key access.

Both entry and unlock prompts expire after 120 seconds. Shutdown aborts pending
and visible modals. The deadline gates admission, not already-admitted work.
Transport disconnect/MCP cancellation is not yet propagated to these synchronous
requests; it must not be described as revoking a pending approval. No automatic
retry occurs. Rust-owned passphrases/keys are zeroized; AppKit and immutable
Apple framework buffer copies cannot promise complete erasure.

Touch ID remains **not configured and not UI-enabled** in this candidate. The
concrete source backend creates P-256 Secure Enclave keys with private-key-use
and current-biometry access control, wraps an independent DEK binding with ECIES,
and uses a new LAContext per read. It cancels/invalidate-drops that context and
never caches authentication between requests. It provides no OS-password,
software-key, or agent-key fallback. Hardware loss, missing hardware/enrollment,
biometry changes/lockout, headless use, signing failure and missing key deny the
device path; an independently configured passphrase wrap remains the recovery
path. Source compilation/mock tests do not constitute hardware acceptance.

The existing app is ad-hoc signed with no team identity/entitlements. Activating
the device path requires a provisioned app identity, appropriate data-protection
keychain access, explicit enrollment UI, and synthetic hardware tests across
cancel, repeated access, app upgrade and missing-key scenarios. No keychain item,
profile or signing account was created during implementation. Apple references:
[Secure Enclave keys](https://developer.apple.com/documentation/security/protecting-keys-with-the-secure-enclave),
[macOS keychains](https://developer.apple.com/documentation/technotes/tn3137-on-mac-keychains),
[authentication context reuse](https://developer.apple.com/documentation/security/ksecuseauthenticationcontext).

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

For requested sharing, `config_share_workspace` takes the exact current
workspace revision, recipient public node identity, `read_only` or `read_write`
access and optional `delegate` (false by default). Native capability policies
enforce grants separately for every collection. Invocation is not delegation:
before opting in, the assistant must explain that the recipient may invite
additional participants within the granted roots/actions without the original
user choosing each person. Disclosed or copied data cannot be made unread.
The grant covers only the currently resolved bundle, never old or future
membership. Grants are additive and non-expiring; editing/removing a workspace
does not revoke another node's authority. A narrower share does not remove
permissions the recipient already holds.

Grants are valid immediately. The returned bounded, opaque `bootstrap` is a
one-way signed workspace-metadata handoff, not a request/approve/acknowledge protocol.
Native grant exchange delivers capability evidence independently of the
workspace-metadata handoff. A grant alone neither enrolls nor selects its roots.
On the recipient's own node, `config_probe_workspace_share` verifies the signed native manifest,
recipient binding in memory, and reports exact descriptor names and typed
role/root routes plus the issuer's declared access choice. It does not confirm
native collection admission. It imports nothing and starts no repair,
hydration or upkeep. Names are presentation, never parsed to infer roles or
future bundle members.

After choosing whether to enroll and a local alias, the recipient calls
`config_enroll_workspace_share` with that bootstrap, label and current Config
observation. It imports descriptor/name metadata and publishes a local
workspace with the same roots/role routes and existing key. It copies no keys,
creates no replacement data roots, sends no reply to the issuer and sets no
global default. Repeat unchanged enrollment returns the same local revision;
a later local edit or withdrawal is never overwritten. The bootstrap contains
descriptor/name blobs and a native signed COMMIT, never capability proofs,
definition bodies, keys or member data. It is metadata discovery, not authority
delivery: native grant exchange and collection repair supply proofs, and
ordinary exact-handle acquisition supplies their definitions.

Without a bootstrap, the same enrollment follows the native grant exchange.
When the issuer's node connects, the grant's proofs reach the recipient's
node and land once their descriptors are resident. `config_list_pending_shares`
lists, read-only, the collections resident proofs grant to this node that no
current local workspace routes, grouped by the issuing workspace anchor found
among each descriptor's tags, with the descriptor name, faculty role (declared
on descriptors created since 2026-10-13, otherwise read from the name's role
suffix, and `role_source` says which), issuer identities, the actions the
proof edges claim (`claimed`, `claimed_access`, `claimed_delegate`) and,
separately, what native admission under the resident descriptor grants this
node (`admitted` per root, `admitted_access` and `admitted_any` per group);
a claim is never presented as effective, so a chain no policy admits reads
claimed read_write with admitted_access none. A root whose descriptor is not
yet resident is listed by handle as descriptor pending, with nothing
inferred; a resident descriptor whose name blob has not arrived, or whose
tags offer no single anchor, keeps its entry and says why in `reason`;
granted resources that are resident but no collection descriptor are counted
under `skipped`; and roots the sync daemon holds grants for in memory only
appear under `sync_memory_pending`. `config_enroll_pending_share` then
publishes a local workspace over exactly the roots given as `roots`, the
consent unit, which are the roots the listing showed: if a root arrived under
the anchor since the listing, or a given root is not granted under it, the
call refuses and names the difference instead of widening or narrowing
silently; the published routes are exactly the roots given, never the whole
granted group. A granted root that another local workspace already routes
stays listed, with that workspace in `routed_in`, may be left out of
`roots`, and is routed here only when given. A first enrollment needs a
label; roots that arrive later under an enrolled anchor are listed beside it
(`local_workspace` enrolled) and enrolling them extends the workspace with a
new revision over the union of routes, label preserved unless given; the
identical already-routed set is idempotent, and a changed or withdrawn local
workspace is never overwritten. A proof edge whose definition is not
resident or does not decode contributes no claim and is counted under
`definitions_pending`. The bootstrap tools remain the fallback when the
nodes have not connected yet.

Sync peering and Full hydration follow the pile's native collection-selection
registers, not all admitted record/proof roots. Creating a workspace, adding
new routes or accepting an enrollment publishes the workspace first, then
separately selects its newly requested roots, even while local READ evidence
is pending. Native evidence still governs admission. Startup initializes only
current understood saved/enrolled roots with an `Unset` native selection;
`Selected` is a no-op, and `Unselected` or `Conflicted` is preserved. No arbitrary
grant forest or local Config/metrics control root is automatically selected.
Manual native selections outside workspace grouping remain authoritative.
Removing a workspace withdraws routing, not grants or native sync selections.

Maintenance retains its independently all-accessible policy, including local
Config, metrics and admitted attached targets. With sync enabled, it may fetch
exact missing payloads for admitted roots outside the native sync selection.
This is not global resource gating. With sync paused, maintenance stays local-only.

Share/enroll responses mean **local publication**, not multi-replica sync
completion or payload hydration. Enrollment keeps node service settings unchanged.
Discovery uses native grant-chain keys, admitted owners and the descriptor
blob's H-authorized providers; issuer identity alone is not authority.
Reachability and evidence acquisition can remain pending.
Source grant preparation and recipient signature/binding checks finish in
memory before writes; the final workspace revision is one native Config
COMMIT. Metadata precedes it and native selection follows it: these separate
publications are not a rollback transaction. A selection error leaves the
workspace published; reinspect before retrying. Interrupted I/O may leave
valid evidence resident. No remote-delivery or revocation receipt is implied.

`config_set_settings` saves the complete typed port, sync and maintenance settings.
The obsolete bootstrap-peer setting has been removed. Historical peer facts
stay inert in the append-only pile; old JSON requests containing `peers` are
unsupported, not silently accepted as no-ops. At the coordinated upgrade,
refresh the client package/schema and reload the client (or upload the refreshed
Cowork archive) before using the new settings tools.
Port changes require an app restart; service settings apply while running.
Responses report the actual running `mcp_url` and whether restart is
required. After the restart Erlkonig reinstalls installed plugins for the new
port by itself (see [Integrations](#integrations)); reload the client, and
upload a fresh Cowork archive. An old-port profile cannot reconnect. Port zero
is ephemeral, so every restart refreshes the installed plugins. Conflicting service
settings leave the configuration endpoint available on the agreed current
port; if the port itself is missing or conflicting, the app uses local port
41773 for recovery. This does not select or save a settings fork, and node
services remain stopped until explicitly reconciled. A busy recovery port or
damaged/unreadable store still requires local operator recovery.
Fresh nodes enable sync of locally selected, permitted collections
and maintenance of their supported attached collections. **Pause sync / Resume sync**
is also available from the crown menu; it leaves maintenance settings alone.
There is no active-workspace selector, keyring or per-workspace service mode.

Every configuration edit passes the opaque observation returned by
`config_inspect`. The token fingerprints native Config foundations without
exposing their blob handles. Stale edits fail; concurrent native revisions
remain visible for explicit reconciliation. The existing typed Config
operations publish the changes, with no separate configuration store.
Missing access requires a node with appropriate native authority to invite
the existing node identity. Creating/editing a bundle or changing service settings
issues no grant; sharing uses the explicit sharing tool above. No
configuration tool accepts private keys, tokens or arbitrary paths.

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
`erlkonig-app` owns the platform-neutral menu state, crown geometry and
command-line dispatch. `erlkonig-mac` (the macOS menu and AppKit crown) and
`erlkonig-linux` (the Linux tray and status window) are the platform binaries;
both are installed as the `erlkonig` command. `faculties`, `trible` and
`triblespace-net` are linked libraries, not launched commands. The node's
single sync owner provides its durable Peer and enabled maintenance. With
sync paused, maintenance is local-only and starts no acquisition networking.
Workspace Faculty reads are local-only: missing payloads fail instead of
starting acquisition peers. Native owners and safe pile handles end with the app.

Maintenance uses a dedicated four-worker CPU pool, dependency-first ordering,
and value-change gating at a one-second cadence. Native held sets are maintained
at the store snapshot boundary; there is no separate held-walker worker.
The daemon records its actual local bound addresses on startup/rebinding,
normalizing wildcard sockets to loopback and superseding stale hints. As in the
native CLI, its last addresses remain soft hints after shutdown, not a liveness
promise. The in-process resource
guard pauses services at 48 GiB process physical footprint on macOS (RSS elsewhere)
or below 20% kernel memory headroom
(`kern.memorystatus_level` on macOS, not the separate `memory_pressure`
estimate). The UI remains responsive; service threads use Darwin background
priority. Stopped services retain their initiating stop reason separately from
session/cleanup errors; the local MCP stays available. After recovery, choose
**Retry services** in the crown or call `config_retry_services`. This requests
one fresh resource-guarded attempt with unchanged settings, after the previous
owner has fully joined; it does not restart a live owner or retry automatically.
`config_inspect.runtime` reports starting/running/stopping/stopped/disabled,
the stop reason and whether a session failed, not raw errors that can contain
paths or capability handles. A queued retry is not a running
service: inspect again for the outcome. Ordinary workspace edits do not restart
stopped services, and Quit rejects retries while draining owned work.
Healthy operation has no status row in the crown menu. An error adds a short
**Sync paused**, **Services paused** or **Attention needed** action; click it
for the full local diagnostics. Diagnostic text never becomes the menu title.
On macOS, long reports use a bounded, selectable scrolling text area; the
native dismissal button stays outside it and Return keeps its default action.
Short alerts retain their usual native layout. The hidden AppKit layout probe
(`cargo run --release -p erlkonig-mac --example alert-layout`) checks full-text
retention, selection, scrolling, button bounds, and small-screen sizing without
opening a dialog or starting services; it is not a visible interaction test.
The same attention condition gives the crown a mildly sharper static outline;
it remains distinct with Reduce Motion, without inventing activity.

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

### Bounded FD failure evidence (macOS)

Maintenance failures matching the fixed EMFILE/ENFILE messages now capture
diagnostics before the app handler formats status or sends ordinary metrics.
This is **handler-time evidence**, not the failed allocation's syscall snapshot:
the upstream error is already a string and temporary descriptors may already
have closed. Matching examines at most 8 KiB of its prefix/causal suffix, accepts
wrapped `tempfile` errors without relying on the outer `raw_os_error`, and stores
neither error text nor paths, descriptor numbers, contents, or handles.

Capture reads this process's actual `getrlimit(RLIMIT_NOFILE)`, realtime and
monotonic clocks, and `proc_pidinfo(PROC_PIDLISTFDS)` into a fixed 8192-entry
stack buffer. Returned descriptors are counted by allowlisted native type,
including an unknown-type bucket. A full buffer is incomplete and its count a
lower bound. A failed/malformed census is absent, never zero; clocks and limits
also remain absent when unavailable. The list, limit and clock calls are not
one atomic observation. Other platforms currently retain limits/clocks but
report the FD census as unavailable.

The controller keeps at most 60 in-memory samples at a nominal one-second
cadence; store work can delay that cadence. A failure carries the maximum
**complete** sample no older than 60 monotonic seconds and its timestamp, plus
the number of eligible samples. Stale, partial, future and unknown-clock samples
do not qualify. This is a recent sampled high-water mark, never an exact peak;
sampling alone creates no durable writes.

Sixteen preallocated occurrence slots use a nonblocking capture lock. Full or
contended capture increments a saturating loss count; a cumulative loss fact is
published even without a subsequent successful capture. Capture opens no file,
allocates no dynamic buffer, takes no node lock and never calls the writer.
The existing controller publishes bounded batches to the private
`erlkonig-metrics` collection at most once per ten seconds (plus shutdown).
Commit/flush failure retains the same occurrence identities and clocks for
retry, including commit-success/flush-failure ambiguity. Writer failure does
not recursively trigger capture. Pending memory is not crash-durable; final
shutdown failure or process exit can lose it. The ordinary UI `metrics_problem`
is not a durable failure latch and may be replaced by other metrics outcomes.
Ordinary metric cadence and detailed error reporting are unchanged.

The facts use the public typed attributes in `erlkonig_core::node_metrics::fd`,
joining occurrence -> handler/recent-sample snapshot -> type/count aggregates
inside the existing metrics collection. There is no new JSON body, secondary
store, dedicated UI or MCP reader. `node_metrics` tests demonstrate direct
`find!` queries, sparse/unknown values, reopening and idempotent retry.
Attribute anchors and event-kind IDs were minted with `trible genid` on
2026-10-21 and are recorded verbatim beside their declarations.

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

A stored MCP-port change takes effect on the next launch. Metrics contain cumulative
observed bytes/publications/failures by node, worker and session;
absent counters remain unknown. Measurements are coalesced at a ten-second
cadence, with observed failures and shutdown flushed sooner. Local-only
maintenance does not claim a live network listener. Failed metrics
appends are visible in the menu and are never reported as persisted success.
Service failures append and flush separate typed events in that existing
`erlkonig-metrics` collection: initiating cause, startup/session failure and each
cleanup failure share an attempt ID allocated before startup. Their phase and
safe code are queryable facts, never reparsed diagnostic text. Successful
settings changes and Quit are quiet; failed cleanup retains their initiating
cause. These typed service events contain only allowlisted codes/descriptions;
existing detailed session samples retain the metrics collection's access/sync policy.
Failure to persist an event remains visible
independently of later successful counter writes, without recursive logging.
An explicitly routed Orient owner can present these saved occurrences to the
model using the shared safe vocabulary and its existing Presented receipts;
this does not install a restart policy. **Retry services** appears only while
services are stopped with valid configuration; a requested retry is disabled
and labelled **Retry requested…** until the existing attempt progresses.

The crown is static at rest and with macOS Reduce Motion. It moves during
app-owned reconciliation passes (including waiting for a peer), actual index
maintenance and native Faculty tool execution. After work ends it completes the
current 3.6-second ripple cycle, landing exactly on the resting outline; new work
before that landing continues the same phase without a restart. Overlapping
work shares one activity count; enabling services, idle waits, status sampling
and rendering do not count. It is an activity indicator, not a bytes-per-second
meter. Payload counters still report measured bytes, and local-only workspace
reads add no network traffic. There is no animation-preview mode.

## Integrations

The **Integrations** submenu lists the clients Erlkonig found: one OpenAI row
labelled for the installed ChatGPT/Codex clients, a **Claude Code plugin** row
when its CLI is available, and a separate Cowork action when Claude Desktop is
found. Desktop detection uses public macOS bundle lookup, not running
processes or account state. See [detection policy](plugin/README.md).

**At startup**, once the first plugin inventory is in, Erlkonig asks one
question if it can connect a client and nothing is connected yet (no Erlkonig
plugin in any client, no account-managed Cowork plugin, no separate MCP server
named `erlkonig` in a client's settings): "Integrate Erlkonig", listing only
the clients it found. **Install** does exactly what
checking those rows does; **Not now** does nothing. The answer is not stored:
an Erlkonig without any integration has no purpose, so the next start asks
again while nothing is connected. It asks at most once per run.

**The check mark is the consent.** Clicking an unchecked row installs the
plugin; clicking a checked row removes it. There is no confirmation dialog.
The mark shows what the client's own CLI reports after the change, so it
flips only once the client confirms; until then the row keeps its previous
state. Nothing pops up on success. If the client refuses, one short message
says so, and the client's error text is kept in Erlkonig's status details
(the **Plugin problem** item in the menu, or the details in the Linux status
window).

What a check mark does and does not mean:

- It is **this computer's** plugin only. For Claude Code it is the
  user-scope plugin (`claude plugin install --scope user`); it neither
  installs nor removes a Cowork plugin on your Claude account. For the OpenAI
  row it is the local package for ChatGPT Work/Codex clients, read from the
  Codex CLI.
- It is the CLI's **inventory**, not proof that a desktop app has refreshed
  its UI, and not proof of an active MCP connection. Reload or restart the
  client to pick up a newly installed plugin; disabled and load-error states
  are labelled separately.
- If the status cannot be read, the row keeps its last-known mark, says
  "status unavailable" and cannot be clicked until **Refresh plugin status**
  succeeds.
- **Keep Erlkonig open.** The plugin's tools forward to the running app and
  never start a backend themselves.
- **No credentials are copied.** The plugin names the app's executable, data
  directory and local endpoint; the private token stays in the app's folder.
- **Removing touches only Erlkonig's plugin.** Erlkonig runs the client's own
  remove command for the exact plugin ID `erlkonig@erlkonig-local` at user
  scope. Other plugins, the marketplace registration and unrelated settings
  are left alone, and Erlkonig itself deletes no file or directory to do it.
  An existing direct MCP server named `erlkonig` in a client's settings is
  reported in the status details, never removed or rewritten.

Inventory runs at startup, after a change, and on **Refresh plugin status**,
never on the crown's display timer. CLI commands run off the main thread, with
a 30-second limit each, bounded output, and cancellation and child cleanup on
Quit.

**Moving or updating the app refreshes the plugin by itself.** A client
installs a copy of the plugin package, and that copy names the app's absolute
executable path, data directory and local endpoint. Each confirmed install
records a digest of the package in the app's data directory
(`plugins/installed-claude`, `plugins/installed-openai`). After each
inventory, Erlkonig compares each client's record with what the running copy
would stage. If anything differs (the app was moved or updated, the package
version changed, or the MCP port changed), it re-stages the package and
reinstalls it, quietly, for every client that reports the plugin installed.
A plugin you unchecked is never reinstalled, and one that was never installed
is never installed. Reload the client afterwards. A refresh is a
remove-then-install sequence; if a client step fails, you get one short
message and the row shows the uncertain state rather than a rollback. A
client whose refresh failed keeps its old record, so the next start (or
**Refresh plugin status**) tries that client again.

**Manage Cowork plugin** prepares a ZIP of the same Claude package used by
Claude Code and opens the account Plugins page. Cowork plugins live on your
Claude account, separately from local Claude Code installs: upload the ZIP
there (Customize → Plugins → Add → Upload plugin) and remove it there too.
Erlkonig cannot see or check the account installation, so this action has no
check mark. The package contains no secret and works on this computer only
while Erlkonig is open; after moving the app, prepare and upload the ZIP
again. A synced Claude plugin is identified as account-managed and routes
here. This is a Cowork/Code plugin, not a standalone Desktop Chat extension.
Cowork's local plugin server runs through Claude Desktop, not in its shell VM;
cloud Cowork can also reach local connectors through the open Desktop app.
Actual interactive Cowork loading remains a validation gate. See
[Claude's cross-platform guide](https://support.claude.com/en/articles/15520349-use-claude-cowork-on-web-desktop-and-mobile)
and the [plugin lifecycle reference](https://code.claude.com/docs/en/plugins/cli-reference).

An OpenAI desktop app without the Codex CLI gets a plain "CLI unavailable…"
row; Erlkonig manages that plugin only through the CLI.

Package staging allows only the intended manifest, shared skill and `.mcp.json`;
unexpected files or symlinks are refused rather than executed or distributed.
The ZIP contains exactly those three files and no top-level `bin/` directory.

## Validation and packaging

Use `scripts/validate-native.sh` for the scoped, reserved Mac release test/build
lane. It uses Rust 1.98, four nice'd jobs and a disposable target directory.
`scripts/bundle.sh --release-dir DIR --out DIR` assembles and signs a bundle
containing only `Contents/MacOS/erlkonig`, copied from the `erlkonig-mac` build
in DIR. It does not launch or install it. On Linux,
`scripts/linux/package-tarball.sh` packs the `erlkonig-linux` build as
`erlkonig` with `install-user.sh`.

`scripts/accept.sh APP` checks that signed bundle's inventory. Native lifecycle
tests use temporary piles and a test-harness fixture, not a production headless
mode. `node scripts/accept/plugin_lifecycle/probe.mjs --codex /path/to/codex
--claude /path/to/claude` exercises plugin lifecycle and stdio in fresh scratch
profiles. Add `--binary /path/to/erlkonig` to test the compiled native transport
against its disposable authenticated HTTP owner. No model request or account
credential is required. See the probe's own output for the precise evidence;
mock transport results are not native binary acceptance. No validation step
requires changing real client profiles or opening the GUI.

For an explicitly approved candidate-only GUI lifecycle check, build
`cargo build -p erlkonig-core --example accept-fixture` from the same frozen
source/dependency closure, then run `python3 scripts/accept/isolated_app.py
/absolute/candidate/Erlkonig.app --fixture-binary /absolute/target/debug/examples/accept-fixture
--root /absolute/new-fixture-directory`. This directly executes the candidate,
never `open -n` or an installed app. The new fixture has its own HOME, client
profiles, temporary directory and inventory-only fake CLIs; native settings are
port 0, sync off and maintenance off before launch. It checks authenticated MCP,
explicit workspace routing, forwarding without another owner, Quit/drain and
relaunch with the same fixture identity, then stops all fixture processes.
It does not open credential-entry UI: invented native Save/Cancel and plaintext
output guards belong to the core local-entry/dispatch tests. Do not reuse a
previous app executable or fixture directory as evidence for a new candidate.

To package an already signed candidate without rebuilding or changing it:

```sh
scripts/package-dmg.sh --app /absolute/path/Erlkonig.app --out /absolute/output/directory
```

The default output directory is `dist.noindex`; the selected directory is
created if needed, but its parent must already exist. The filename includes the
bundle version, build and actual architecture, for example
`Erlkonig-0.1.0-1-macos-arm64-development.dmg`. Only arm64 is currently supported.
Existing outputs are refused unless `--overwrite` is explicit; a failed run
leaves the previous image intact. The command uses `ditto` to preserve the
bundle, adds an `/Applications` link, creates a read-only compressed UDZO image,
then mounts it read-only without opening Finder. It checks the root inventory,
byte equality with the input bundle and its signature, and detaches before
publishing. Temporary staging and owned mounts are cleaned up on failure.
Nothing is launched, installed or configured. Run the narrow packaging checks
with `scripts/accept/dmg.sh /absolute/path/Erlkonig.app`.

This is **local development packaging**, not an external release gate. It
preserves the app's existing signature, does not sign the DMG, and does not
upload, notarize or staple anything. An ad-hoc app is not a Developer ID release;
do not promise testers a frictionless Gatekeeper experience or bypass quarantine.
The allowlisted bundle contains the executable, icon, source plugin templates
and build provenance, not a pile, key, config, token or prepared personal plugin
cache. Provenance can contain non-secret absolute build/source paths; inspect it
before external distribution if that information matters.

For an explicitly authorized direct release, follow
[Apple's packaging guidance](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution):
sign code inside-out with Developer ID and the appropriate runtime/timestamp
settings, sign the outer DMG, notarize that outermost container and staple its
ticket. Test the downloaded artifact on a clean supported Mac, including fresh
installation and upgrade, rather than treating a local mount check as those
tests. Signing credentials and external submission are separate release work.

The adjacent native dependency repositories must be the frozen source cohort
recorded by the validation run. See `TESTING-for-JP.md` for the remaining
interactive checks; automated success is not a claim those checks occurred.
