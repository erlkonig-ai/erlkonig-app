# Interactive Erlkonig checks

Use only the explicitly selected native candidate. The previous signed native
bundle remains rollback, not evidence for this implementation. Do not overwrite
an app while it is running. Stop any legacy Erlkonig node before migration;
the app deliberately refuses to share its data-owner lock with it.

These are pending interactive checks, not an already-passed checklist:

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
   Cancel a dialog and confirm AppKit's temporary check toggle is restored.
   During a CLI operation the menu/crown must remain responsive; Quit must
   cancel/reap its own child. Remove must preserve unrelated plugins/settings.
   Move the candidate and Refresh package; verify the cached command points to
   the new absolute executable path. Test failure between remove and reinstall
   without mistaking it for rollback. Existing direct MCP settings remain
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
