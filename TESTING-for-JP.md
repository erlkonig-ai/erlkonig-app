# Interactive Erlkonig checks

Use only the explicitly selected native candidate. The previous signed native
bundle remains rollback, not evidence for this implementation. Do not overwrite
an app while it is running. Stop any legacy Erlkonig node before migration;
the app deliberately refuses to share its data-owner lock with it.

These are pending interactive checks, not an already-passed checklist:

Supported framing upgrade (synthetic-only, isolated candidate): run the same
`scripts/accept/isolated_app.py` candidate/helper command documented in README
with `--old-framing` and a new absolute fixture root. The helper preserves real
app settings and workspace routes, appends a public released-format fixture
record, and writes the public `node.json` locator with the same existing key.
The acceptance runner checks a fresh sibling is selected, source bytes and key
are retained, settings/routes remain usable, and restart keeps that selection.
On the first launch choose **Migrate** in the candidate's **Update local data?**
prompt; the runner allows two minutes for that explicit response. Separately
choose **Not now**: the source and locator must remain unchanged and local
services must remain stopped. Reopening asks again; a completed update's next
launch must not ask again. Noninteractive startup must never silently consent.
This exercises framing conversion, not the separately authored `/32` canonical
cutover. It must not be reported as acceptance of an actual old DMG until that
artifact's writer format has also been exercised.

For an old branch fixture, check that the native prompt displays its readable
label, exact ID and this node's public identity. Cancel leaves the source and
locator unchanged. Import explicitly adopts that collection only. Quit while
waiting must close the prompt and join startup; Quit during conversion waits
for a native-operation checkpoint. Current piles with retained branch pins
must start without repeating the import prompt.

Protected local 1Password importer (synthetic-only, isolated candidate):

The source-only helper extension is `protected-secrets-trial --interactive-import
NEW_ABSOLUTE_ROOT`. Launch only after explicit coordination: set
`HOME=NEW_ABSOLUTE_ROOT/home` and `TMPDIR=NEW_ABSOLUTE_ROOT/tmp` in a clean
environment; the root must not exist yet. The helper creates public synthetic
export/key/store files there, starts no normal runtime/services, and only returns
its pre-opened synthetic source when that exact fixture is selected. Other paths
are never opened. Five deliberate UI phases test picker Cancel, passphrase Cancel,
new Save, wrong-passphrase verification and correct receipt reuse. The entire
helper is bounded to 15 minutes; prompt admission remains 120 seconds. This is
not an automatically passed check, and it does not replace production deployment
acceptance. Existing `--interactive` password-trial mode is unchanged.

- Call `secrets_import_1password_local` with an exact workspace. Only the
  local picker may select the invented `.1pux`; no file path or data goes in
  the tool call. Cancel the picker, then separately Cancel the passphrase
  confirmation; neither publishes an import.
- Verify the bounded confirmation names the workspace, explicitly says NEW
  import (or prior receipt verification), no ordinary node-key access, older
  readable copies NOT revoked, no Touch ID/recovery, and once-only work after
  Save. Confirm it fits the screen without the long-details modal path.
- Wrong extension/non-regular file, changed source, empty/mismatched passphrase,
  shutdown and expiry before admission must save nothing. After admission a
  long import can drain beyond the UI deadline; never infer rollback from a
  closed connection. Check metadata only, not secret exports, for completion.
- Successful synthetic import returns an opaque receipt and counts only.
  Explicitly verify that receipt with the same local export/passphrase, then
  test wrong passphrase and wrong export: neither may create a replacement.
  Omitting the receipt intentionally requests another new import.
- Ordinary node-key reads of original/items/attachments/receipt must fail;
  titles, export path, values and passphrase must not appear in results/logs.
  Do not perform these checks with the real company export.

Protected Secrets candidate checks (only with invented data in an isolated
store/HOME; do not run against an existing credential):

- Create a protected generic item and a protected login. Confirm the separate
  app passphrase twice; mismatch, empty input, over 1024 UTF-8 bytes, Cancel,
  shutdown and the 120-second deadline must publish nothing.
- Request raw plaintext, private facts, structural inspection, one exact
  text/binary field and a TOTP code separately. Confirm each prompt describes
  its different disclosure and remains screen-bounded with long labels. A
  second request must ask again, not reuse the previous passphrase/unlock.
- Cancel a pending request, including after its native window is visible;
  verify no plaintext output. Disconnect is not currently a cancellation
  guarantee: allow only synthetic calls to reach the deadline.
- The current ad-hoc candidate must say Touch ID is not configured. No device
  key may be created merely by startup, metadata listing or drawing a prompt.
  Real Secure Enclave enrollment remains a separate, explicitly interactive
  acceptance after provisioned signing is available: verify fresh Touch ID per
  access, cancellation, lockout, enrollment changes, missing hardware/key,
  hardware loss recovery and key access across a signed app upgrade. Never
  report source/mock tests as having passed these hardware checks.

1. Launch the candidate. Expect one crown, no Dock window, and a clickable
   status line. A fresh app store enables sync of permitted collections and
   maintenance of supported attached collections. No background-node approval
   or resident helper backend should appear. A client can separately start the
   same executable's connection-scoped `mcp-stdio` forwarding mode.
2. Open the crown menu. It should have status, integrations, pause/resume sync,
   Show storage folder, login and Quit, with no animation-preview control, manual configuration wizard or
   node-identity copy action. Backend identity inspection remains available
   through `config_inspect`; confirm its public node identity,
   then Quit, reopen and confirm the same identity. Quit must remove the crown
   and local MCP listener, not leave a resident sync service. Force Quit must
   remove app-owned native threads/listeners as well.
   **Show storage folder** opens the running node's pile directory in Finder,
   not the application-support or key directory. Check a disposable node bound
   to an alternative pile location (including spaces); no storage chooser or
   configuration change should occur. The action is disabled before successful
   node startup and while quitting.
3. Enable Open at Login only when requested. This registers the main app, not
   an agent. Check logout/login separately; ad-hoc signing can require approval
   after a new build changes the code identity.
4. Confirm the static crown and Reduce Motion. In disposable fixtures, check
   movement during a slow reconciliation pass before any bytes complete, actual
   index maintenance, and a long native Faculty call. Overlapping work must
   remain active until its last scope completes; success, error, cancellation
   and shutdown must all release their own scopes. Idle service loops, no-op
   maintenance ticks, status sampling and display frames must not sustain it.
   Confirm that stopping completes the current ripple cycle to the neutral
   resting outline (at most 3.6 seconds), and new work before landing continues
   without a phase jump. Reduce Motion immediately shows exact rest.
   This is app-owned work, not
   throughput or unrelated CLI activity. No artificial network traffic is needed
   to test the deterministic activity/time renderer.
5. Client setup is a separate explicit action. Do not install/remove plugins in
   a real profile during validation. A scratch launch must isolate HOME as well
   as CODEX_HOME/CLAUDE_CONFIG_DIR. Check the two local plugin rows: installed,
   installed-but-disabled, not installed and unavailable must remain distinct.
   Clicking a row installs or removes with no dialog; confirm AppKit's
   temporary check toggle shows the observed state until the client confirms,
   and that success shows nothing while a client failure shows one short
   message. During a CLI operation the menu/crown must remain responsive; Quit
   must cancel/reap its own child. Remove must preserve unrelated
   plugins/settings. Move the candidate and relaunch it: the installed plugin
   must be reinstalled without a prompt and the cached command must point to
   the new absolute executable path; an unchecked plugin must stay absent.
   Test failure between remove and reinstall without mistaking it for
   rollback. Existing direct MCP settings remain
   untouched and are warned about rather than silently creating duplicate tools.
6. In a fresh scratch HOME/store, call `config_inspect` before a workspace
   exists. It must return the public node identity, settings and observation.
   Inspection alone must not create a workspace. Ask the assistant to create
   a friendly named workspace: the returned revision must immediately support
   normal Faculty tools, without hex entry or pile/key file dialogs. Create
   a second workspace and confirm its private roots are separate while the
   public node identity, physical store and network endpoint remain the same.
   Try a duplicate name: roots must not silently merge by label. Do not use
   a personal or water pile as a fixture or take over a known standalone owner.

   Configure service settings through `config_set_settings`: sync off must
   release its Peer while local-only maintenance can continue. Pause/resume
   sync in the menu must preserve maintenance settings and keep local Config
   tools reachable. Reopen must retain explicit pauses.
   No Managed Piles, keyring or per-workspace storage controls should remain.

   Use only port/sync/maintenance fields; old settings requests containing
   `peers` must fail even for an empty list. Historical peer facts must still
   read as ordinary settings and remain in the pile. At the coordinated
   upgrade, refresh the client package/schema and reload the client.
   In a legacy fixture, only current saved/enrolled roots with an unset native
   selection should be initialized. Repeat startup must append no selection
   states. Preserve native unselected/conflicted choices and honor manually
   selected roots outside workspace grouping. A new admitted record or grant
   must not automatically select collection peering. New workspace routes and
   enrollments publish first and select afterward; failure is not rollback.
   Withdrawal must not revoke grants or silently unselect native roots.
   The actual daemon socket addresses must be recorded as reachable local
   hints and superseded on rebinding. Maintenance remains independently
   all-accessible and can acquire exact missing bodies while sync is enabled;
   these assertions do not claim global fetch isolation.

Configuration, metrics and workspace collections share one node pile. A
malformed/unreadable control observation stops node services; a conflicting
workspace remains unavailable. MCP requires an explicit workspace on every ordinary faculty
call; data is neither silently unioned nor defaulted to the node pile.
App-private leases cannot police nonparticipating standalone daemons.

Normal Quit cooperatively drains; after 15 seconds a stuck native operation
causes final process termination with ordinary Force Quit durability limits.
Do not describe the library's five-second transport-close timeout as a bound
on all admitted work.

7. Manage Cowork plugin must reveal a three-file ZIP and the account Plugins
   page, with no checkbox or installation-success claim. With a separately
   authorized test account, upload that package and verify tools in Cowork;
   account-synced Code copies must point removal back to the account UI. The
   same Claude package serves Code and Cowork, not a separate Chat extension.
8. With a native stdio client connected, quit/force-quit Erlkonig: the owner
   listener and transport must disappear, no new pile should be opened, and no
   restart should occur. A new client attempt while the app is absent must fail
   visibly on stderr without printing a credential or bootstrapping a backend.
9. Use `config_list_collections` in a fixture with multiple locally named
   roots. Distinct choices must retain distinct exact handles, never a union.
   READ and WRITE must be reported independently. Create or edit a bundle with
   explicit role/root choices; missing admission must fail without adding a
   grant. Missing access needs an invitation for the existing identity, not a
   secret in chat or another identity.

   Invalid and stale configuration requests must append nothing. A rename
   preserves omitted routes; supplied routes replace the complete bundle.
   Unconfigured roles remain unavailable. Ordinary workspace changes must not
   toggle node services or create another owner. Discovery leaves bytes
   unchanged. Conflicted settings stay inspectable and need an explicit
   complete settings reconciliation. Settings tokens must expose no Config
   payload handles, private keys, paths or local transport credentials.
10. Call `list_workspaces` and check labels, exact IDs, public signers and
    role/root handles without key paths. Interleave explicit A and B requests
    in one client and across clients; omitted/unknown IDs must fail with no
    node/default fallback. A missing role or local payload must fail clearly.
    Both bundles must report the same node identity. A read from A followed by
    an authorized write to B is two operations, not
    an automatic transfer or atomic transaction; routing is not a sandbox for
    tools with host-path or command execution features.
11. Edit a workspace and confirm the old ID is rejected, not redirected. A
    malformed/conflicted entry must remain visible as unavailable without
    hiding healthy entries; `config_remove_workspace` must work even when the selected
    bundle cannot be decoded. Other concurrent revisions stay separately visible.
    A native additive route annotation under an existing opaque revision ID
    must affect later requests equally for warm and fresh owners, while an
    already admitted operation keeps its resolved binding. Quit must drain
    admitted operations and close the node's retained owners.

Native menu compilation and GUI interactions require a reserved Mac build/run;
Rust parser checks and Linux tests do not establish AppKit correctness. Local
MCP runs on the host through Desktop; Cowork's shell VM is a separate execution
environment. No remote Connector or public tunnel is configured by this app.
