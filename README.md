# Erlkonig

Erlkonig includes the Faculty catalog, workspace-scoped ERP tools, and the
Revolver importer. From a clean checkout in an empty parent directory,
fetch the pinned public source dependencies with
`python3 scripts/ci/release_inputs.py --app "$PWD" --fetch-development-sources`.
Install the formatter's compilation target with
`rustup target add --toolchain 1.98.0 wasm32-unknown-unknown`.
Build with `cargo build --locked -p erlkonig-mac` on macOS or
`cargo build --locked -p erlkonig-linux` on Linux. For the headless CLI, use
`cargo build --locked -p erlkonig-cli`. Run the shared checks with
`cargo test --locked -p faculties-erp -p erlkonig-core -p erlkonig-app -p erlkonig-cli`.
Toolchain and platform requirements are recorded in `ci/source-inputs.json`.

[Run the headless owner in a rootless Podman pod](container/README.md) to give
a sandboxed AI client access to a dedicated node.

The desktop shells share their UI and backend with the crates in this workspace.
Desktop packaging targets macOS ARM64 and Linux x86-64 and ARM64.
The [Rust iOS client](ios/README.md) runs a foreground replica with
collection selection and storage and connection status. Build its unsigned
arm64 simulator app on an Apple silicon Mac with `./ios/build-simulator.sh`.

## Bookkeeping and Revolver imports

Use `list_workspaces` and `config_inspect` on the running app to select a
workspace. Pass its current revision and configuration observation to
`config_enable_workspace_erp`. It returns the new workspace revision and
creates a bookkeeping collection accessible to the owner. Use that revision
with `erp_review`, `erp_invoice_add`, and the other ERP tools. Sharing uses
the workspace’s existing grant and enrollment controls.

The macOS, Linux and headless CLI binaries include the Revolver operator command. Run
`erlkonig erp source revolver import --help` for its source options and
`erlkonig erp --help` for the destination pile and signing key options.
The importer reads a Revolver PostgreSQL database through `psql` or replays
an ordered PostgreSQL COPY text capture from `--from-dir`. Select
an independent destination pile for these direct CLI operations. Use the
authenticated workspace tools for the running app’s collections.

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

## Protected local 1Password import (candidate, not deployed)

`secrets_import_1password_local` requires an exact `workspace` ID. The only
other argument is optional `previous_import`, an opaque receipt ID returned by
a previous protected import. No path, export bytes, credentials, passphrase or
ordinary-protection option is accepted. The native file picker selects one
regular `.1pux` file; the app opens it once and locally collects and confirms an
independent app passphrase. Export contents never go through chat/tool arguments,
logs, clipboard or temporary plaintext files. Existing importer limits apply.

Omitting `previous_import` explicitly creates a **new** import, without global
content deduplication. Supplying it verifies only that exact previous protected
import against the locally selected export and passphrase; failure never falls
back to creating a replacement. Keep the returned receipt ID for this purpose.
The whole original archive, imported items/attachments and receipt have user-only
protection, not node-key recipient wraps. Public labels are neutral, not imported
titles: model metadata listing cannot discover account/vendor titles before
approval. This is not yet a named account search/catalog or local selection UX.
Older ordinary node-readable copies are **not revoked or converted**.
Touch ID, recovery and signing/keychain enrollment remain disabled.

The workspace route is frozen for the call. No store lease is held during file
selection or passphrase entry; expensive parsing/encryption also runs outside
the publication lease. Cancel/shutdown/120-second expiry before Save admission
publishes nothing. An admitted batch runs once and may take longer than the
prompt deadline; disconnection or shutdown does not promise rollback. Results
contain only status, receipt ID, counts and maintenance/reuse flags. A typed
uncertain publication returns `outcome_unknown` plus its planned receipt ID;
keep it and inspect metadata before any explicit verification. No operation is
automatically retried. The source-read buffer and passphrase are zeroizing;
complete erasure of parser, AppKit and OS copies is not promised, and the original
local export is untouched.

Prepublication rejection reports `not_saved` with a static `stage` and `code`
(format/limit validation, destination observation, encryption, receipt
verification or other preparation), and optional positive numeric `os_error_code`.
These are classifications, not raw parser/IO messages: no archive names, paths,
field labels, values or passphrases are returned. An unclassified prepublication
error retains the generic `import_rejected` code. Publication uncertainty still
returns `outcome_unknown` and its receipt, never a no-publication claim.

## One node, named workspaces

Startup inspects the selected pile before opening services. When an update is
needed, the native app asks **Update local data?** with **Migrate** and **Not now**.
Not now leaves the original and locator unchanged, with local services stopped.
Noninteractive startup cannot answer on the person's behalf. After approval,
supported older framing is converted on one background startup task into a fresh sibling file;
the original pile and existing signing key are retained. Erlkonig checks the
candidate through its settings and workspace readers, then atomically updates
the existing `node.json` locator. A restart uses that locator and does not
repeat conversion merely because retained legacy branch pins remain.

Legacy branches require an explicit native choice naming the collection and
this node's read/write authority. Importing a collection does not infer its
Faculty role or enroll it in a workspace. The separate settings migration
handles old application identities and configuration. Unsupported formats and
canonical signed-record migrations that need original authors' refreshes leave
the original and locator untouched and explain the required next step.
After migration approval, records signed by this node use its existing key for
the canonical update. Foreign signatures are never replaced with this node's.
Quit cancels startup at the converter's checkpoints and waits for its current
native operation to finish before releasing the owner lock.
Cancellation is checked again after the locator's temp file is durable and
immediately before selection. Once selected, the updated copy stays selected
even if the remaining startup is cancelled. Existing unavailable Config data
and unresolved choices retain their native read outcomes for normal repair.

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

With a quorum policy, each authorized signer uses the same share operation
for the same recipient and collections. The response reports `pending` until
local native evidence admits every requested action. Each collection’s READ
and WRITE alternatives report `have` distinct contributing roots and the
required `threshold`. The recipient can inspect these counts with
`config_list_pending_shares`. Repeating a share after evidence arrives
reports the updated counts. Native peer exchange delivers proofs; sync and
repair acquire descriptors and their names. A grant alone neither enrolls a
workspace nor selects its roots for Full synchronization. On the recipient's
own node, inspect native evidence with `config_list_pending_shares` and local
roots with `config_list_collections`.
The pending listing groups resident evidence by issuer workspace and reports
exact roots, descriptor names, role hints, proof claims and native admission
separately. Names and declared roles inform the model's choice of faculty
routes; they do not dictate that choice or grant access. Automatic enrollment
uses only an unambiguous understood role declared on the descriptor, never a
display-name suffix. Unknown declarations coexist with understood roles;
conflicting understood declarations require explicit routes.

For the requested work, the model chooses explicit `{role, root}` routes over
the existing admitted collections. A deliberate subset is valid. Use
`config_create_workspace` with those routes for a new local binding, or
`config_edit_workspace` for an existing binding. Creation without routes
creates fresh private collections instead; editing with routes replaces the
complete binding, so retain the routes the user still wants. These operations
check the known faculty role, resident named descriptor and independent native
READ or WRITE admission. They require neither matching descriptor names nor
the entire issuer bundle, and do not change roots or grant permissions.

`config_list_pending_shares` reports `enrollable` for its automatic helper,
not the recipient's general ability to use a root. When it is false, inspect
the reported metadata and reason. Unknown role hints can be handled by
explicit model-selected routes. Missing descriptor
bytes and missing native admission are separate conditions that explicit
routing does not fix. A root already used by a local workspace is disclosed
in `routed_in`; inspect that binding before creating another one.

`config_enroll_pending_share` remains an optional convenience for a complete
group whose inferred mappings already match the intended binding. Only that
helper requires every currently unrouted granted root and rejects a set that
changed since listing; explicit create/edit supports the chosen subset.
Its later extensions union the given roots with its existing binding, while
a changed or withdrawn binding is never overwritten. Listing alone fetches,
grants and selects nothing. Discovery reads the evidence native exchange and
repair have already made resident.

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

Native swarm-health reporting starts disabled. To enable it, call
`config_set_health_reporting` with the observation from `config_inspect` and
the exact `root` of an existing writable source collection. Pass `null` to
disable it. Readers of that root can see this node's endpoint, collection and
peer identities, and store and DHT measurements across the node. The choice is
node-wide and independent of workspace routes, grants and replication selection.
The sync owner publishes a startup observation before opening its endpoint, then
records its peer's measurements every 60 seconds. A destination edit takes
effect at the next sample. Readers determine freshness from report timestamps.
`config_inspect` shows the configured destination and whether a write failed.

Aggregate sync telemetry also starts disabled. Pass the latest `observation`
and `destination: {"root":"blake3:…","worker":"app-sync"}` to
`config_set_sync_telemetry`, or pass `destination: null` to turn it off.
Choose an existing source collection that admits the node as a writer.
Readers can see the node identity, worker label, hydration, serving, repair,
publication and process measurements. The sync loop samples before its first
reconcile and every 60 seconds at a loop boundary. It applies destination or
label changes at that boundary and starts a fresh measurement session for
the new pair. Conflicting choices suspend publication until reconciled.
`config_inspect` shows the destination, worker and publication failure flag.
Workspace routes, native sync selection and swarm-health reporting each have
their own configuration.

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
`erlkonig-app` owns the platform-neutral menu state and command-line dispatch.
It re-exports the `erlkonig-crown` geometry shared with iOS. `erlkonig-mac` (the macOS menu and AppKit crown) and
`erlkonig-linux` (the Linux tray and status window) are the platform binaries;
both are installed as the `erlkonig` command. `faculties`, `trible` and
`triblespace-net` are linked libraries, not launched commands. The node's
single sync owner provides its durable Peer and enabled maintenance. With
sync paused, maintenance is local-only and starts no acquisition networking.
An explicit workspace Faculty operation may acquire exact missing payloads,
independently of background sync and maintenance settings. Each used workspace
retains one lazy, non-serving client: resident operations start no endpoint;
a cold read contacts the bound node first and may follow authenticated provider
and discovery hints. It neither selects collections nor activates replication.
Its frozen collection observation remains unchanged while exact bytes arrive.
Workspace replacement/withdrawal and app shutdown close those clients and their
safe pile handles.

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

Every new occurrence separately records `error_site = maintenance`: this names
the handler observing failure, **not** the subsystem causing descriptor pressure.
Snapshots also carry `mcp_http_accepted_streams` from this app's exact MCP HTTP
endpoint. This allocation-free atomic gauge follows accepted I/O lifetimes:
it increments only after accept succeeds and decrements after I/O closes, before
releasing admission. It does not count reserved pre-accept permits, requests,
sessions, the listener, kernel backlog, outbound sockets or other subsystems.
A cloned observer does not keep the endpoint alive. Missing instrumentation is
absent, not zero. The gauge and OS census are separate observations; do not
subtract them to manufacture an "unattributed socket count". They can disagree
under concurrent activity, and a recent sampled high-water selects by total FD
count, not by the HTTP gauge's maximum.

The explicit read-only `inspect-fd-events` report exposes `error_site_subsystems`,
`pressure_owner: "unknown"`, and each snapshot's `mcp_http_accepted_streams`.
Older/unknown error-site facts report `unknown`; arbitrary labels are never
echoed. OS socket ownership and the cause of exhaustion remain unknown: the
separate MCP gauge is corroborating lifetime evidence, not syscall attribution.

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
2026-10-21 (original census) and 2026-10-26 (subsystem evidence), and are recorded
verbatim beside their declarations.

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
cadence, with observed failures and shutdown flushed sooner. Successful maintenance hops with no publications emit no sample; observations
are work evidence, not an idle heartbeat. Upkeep of this node's metrics root,
its native derived collections and their attached indexes still runs and counts
in live status. Targets classified from the current discovery snapshot emit no
ordinary sample into their own sink, including when that upkeep fails. Newly
resident descriptors are classified on the next changed snapshot. These failures remain visible in status and existing bounded
FD diagnostics; failures for other targets retain ordinary error samples. A
later reportable observation carries the cumulative counters for all upkeep.
Local-only maintenance does not claim a live network listener. Failed metrics
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
meter. Payload counters report the background service's measured bytes; exact
workspace-client acquisition is not included in that counter. There is no
animation-preview mode.

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

## Faculties data viewer

Choose **View data** from the crown menu, then select a workspace from the dropdown.
Choosing it again brings the same window forward; after closing it, the command
opens a new viewer on the existing app owner.
Duplicate names have distinct revision suffixes; selection binds the exact
current workspace value. The window reuses the existing Faculties notebook and
its ordered cards, including Wiki, Compass, Messages, Files and Memory. The
Wiki graph, search and Typst pages retain their existing layout and exact
revision links. Expand the sections you want to inspect.

The viewer shows **local data** and updates as relevant content arrives. The
app owner's existing refresh cycle checks the observation's native dependencies,
including payload reads performed later by widgets. It queues one replacement
observation on the reader worker; completion wakes the window. Unrelated appends
do not reset the view. **Refresh** requests an explicit observation and resets
card navigation. Existing widget queries over frozen data and Typst rendering
still run on the window thread. Activity, Gauge, Compass and Headspace defer their
queries while collapsed; opening a large section can still be expensive. Each view uses its explicitly configured workspace collection.
Missing roles are omitted, and unavailable views do not hide independent readable
views. Ordinary collections retain their READ admission; Secrets retains its
local delivered-envelope possession semantics. No role falls back to a default
collection. The resident app's existing sync/maintenance supplies local content;
the viewer does not acquire data from peers or start another service.

The window lives inside the app's existing main-thread event loop. It embeds the
same GORBIE paint renderer and `compose_hosted(Target::Dashboard)` composition;
there is no viewer child or second event loop. Its Runtime-registered worker
borrows the already-loaded `SharedNode`/`NodeStore.pile` for Config, frozen widget
observations and actions. It never opens a pile path or creates a Faculty Storage,
peer, key or independent storage owner. The borrowed pile's durable signer is
the app's signer. Selection and refresh recheck the complete workspace value.
A transient request generation rejects old completions and resets card navigation
after Refresh or choosing A → B → A. Automatic presentation updates keep that
generation and navigation while invalidating workspace presentation caches once
per relevant update. Native snapshots remain frozen; a missing resident payload
that arrives through sync triggers a fresh observation without a viewer fetch.

Wiki links and Files preview actions run on the worker against the exact clicked
Files observation. Every export gets a fresh owned preview folder. Actions check
selection and cancellation before filesystem effects and again immediately before
launching the system viewer. Failed or cancelled exports remove their own partial
folder; successful previews remain available to the external viewer. Requests are
never replayed automatically. Config changes refresh the same dropdown choices.
A changed binding clears the old view and invalidates its queued actions. A
removed or unreadable selected workspace stays selected and unavailable; it
never silently falls back to another workspace.

Priority answers use the same factored Compass preparation, cycle checks,
authoring and publication operation through the app's owner. READ/WRITE admission
and a final cancellation/generation check precede publication; successful writes
refresh the observation. Skip publishes nothing.

Only one viewer is open per app process; its menu row is disabled while open.
Close the window to cancel its worker and release the UI observations. Once the
worker settles, **Browse Faculties…** is enabled again; the app-owned pile stays
open. Runtime owns every worker join and joins them before closing NodeStore.
Quit also cancels the viewer. The existing 15-second final process-termination
fallback does not claim that an unfinished native operation drained cleanly.

Enabling widgets also makes the already-registered **viewer_capture** operation
available. This is a named capability change: explicit calls still select an exact
workspace and target through the existing dispatcher, validate bounded image
output before rendering, and return PNGs. No GUI/GPU is opened during discovery or
startup. Capture remains synchronous; disconnect does not cancel an admitted
call, and the app's existing shutdown grace applies. Its image-byte bounds do not
bound all GPU allocations or total render time. The viewer window adds no MCP tool. The pinned Faculty upgrade also brings upstream catalog changes, including Alarm; release acceptance compares tool names and schemas against the preceding app.

Mac and Linux route the new window through their existing application loops.
Linux's status window remains independent. No desktop adapter is added to iOS.
This in-process source candidate has not been compiled or exercised on either
desktop platform; native dashboard, GPU callback, input/repaint and close/reopen
acceptance remain required. There is no Windows executable in this workspace.

## Native command line candidate

`erlkonig-cli` is a separate core-dependent crate whose installed binary is
`erlkonig`. It does not depend on the GUI crate or require AppImage/FUSE. The
existing core dependency closure still includes WGPU through TribleSpace/Jerky;
this candidate does not claim a GPU-free binary. Running
without arguments shows help; only `erlkonig serve` starts an owner, in the
foreground. It uses the desktop application's data directory, node locator,
identity, workspaces and existing exclusivity locks. Ctrl-C and SIGTERM cancel
controlled startup or drain the running owner through its normal shutdown.
Storage upgrades needing interactive approval are refused. Local secret entry,
protected access and import dialogs return unavailable promptly in this shell.

Clients connect to an existing owner and need its exact URL; `serve` reports the
actual bound URL. Supply `--data-dir` when the owner uses a nondefault app
directory. Clients do not open the pile or signing key, create a token, scan
ports, launch an owner, reconnect, retry a call or persist workspace selection.

```sh
erlkonig serve
erlkonig workspaces list --url http://127.0.0.1:PORT/
erlkonig tools list --url http://127.0.0.1:PORT/ --output json
erlkonig call wiki_list --url http://127.0.0.1:PORT/ --workspace 'My workspace' --json '{}'
```

`call` accepts the owner's advertised tools and schemas, with no local Faculty
command registry. `--json -` reads one JSON object from stdin. `--workspace`
resolves an exact current label or revision through `list_workspaces`; ambiguous
labels fail. This convenience applies only when the tool advertises a string
`workspace` argument, including applicable Config tools. A conflicting JSON
workspace fails rather than being overwritten. An explicit JSON workspace can
also be supplied unchanged. Tool errors exit unsuccessfully; `--output json`
preserves the complete MCP tool result. An unknown mutation outcome is never
retried, and connection loss does not cancel admitted work.

Native CLI distribution contract: `erlkonig-cli-${VERSION}-${TARGET}.tar.gz`,
with exactly two flat regular members: executable `erlkonig` and
`THIRD_PARTY_NOTICES.txt`. Initial targets are `aarch64-apple-darwin`,
`x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`. Each release records
archive and both member sizes and SHA-256 hashes, selected source identities and
retained lock hash. This source candidate adds only a local CLI package entry
to the prior lock, without changing external dependency versions. Native build,
runtime acceptance, complete license notices and publishing are separate release
steps; source/fixture tests do not establish them.

## License

Erlkonig is available under the [MIT License](LICENSE-MIT) or the [Apache License, Version 2.0](LICENSE-APACHE).
