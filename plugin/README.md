# Erlkonig package sources

This directory is source material, not an installable mixed-provider package.
The app generates one OpenAI package and one Claude package with the same
`skills/faculties/SKILL.md`. Each receives only its provider manifest and an
`.mcp.json` command with the absolute current app executable, data directory,
and local endpoint. Paths are separate argument values, not a shell command.

Claude Code and the manually uploaded Cowork ZIP use the same Claude package.
There is no standalone Desktop Chat extension. Generated packages contain no
credential, private key, pile, top-level `bin/` folder, or bundled executable.
The client invokes the app binary's `mcp-stdio` mode; that transport only
forwards to an already-running app. Quit closes the owning backend, and the
transport must never relaunch it or acquire a pile itself.

HTTP sessions expire after idle periods. On the native backend's explicit
missing-session response, the stdio adapter initializes a fresh protocol session
on its existing connection. It reports the rejected request as not executed;
it does not replay it. Later requests use the renewed session. Network failures
and owner shutdown remain terminal because an admitted operation may already
have completed. Session renewal never reconnects to a replacement app owner.

The Mac's **Integrations** menu detects installed desktop apps through public
`NSWorkspace` bundle lookup (no app launch), and executable management CLIs
through the local resolver. Detection runs at startup and on **Refresh plugin
status**, not on the runtime status timer. The known OpenAI bundle ID is
`com.openai.codex`; its public display name determines whether the row says
ChatGPT or Codex. A CLI may also be resolved from the detected bundle's known
`Contents/Resources/codex-cli/bin/codex` resource, independent of where the app
was installed. Claude Desktop uses `com.anthropic.claudefordesktop`.

Only detected integrations appear. The one OpenAI row describes the detected
desktop app/Codex CLI; its check is the CLI's local plugin inventory, not a
desktop connection claim. An OpenAI app without a management CLI gets a plain
“CLI unavailable…” action, never a synthetic check. Claude Code has its own
local-inventory check. Claude Desktop adds the separate **Manage Cowork
plugin…** account action; detecting the app proves neither Cowork entitlement
nor account installation. Installing locally in Claude Code does not install
the account plugin. Account-synced-only inventory routes to account management.

The check mark is the consent: clicking an unchecked row runs the client's
own install command and clicking a checked row runs its remove command, with
no dialog. The mark follows the client's inventory, not the click. An installed
but disabled plugin remains checked and says “disabled.” An inventory failure
remains visible, disabled, and says “status unavailable,” retaining the last
observed check. Not detecting any supported client leaves a small “No
supported clients detected” entry and the refresh action. Detection never
removes an integration or changes client settings.

What the app installs is this computer's copy only: Claude Code at user scope,
the OpenAI package through the Codex CLI. A check is the CLI's inventory; it is
not proof that a desktop UI refreshed or that an MCP connection is active.
Removal runs only `claude plugin uninstall erlkonig@erlkonig-local --scope user`
or `codex plugin remove erlkonig@erlkonig-local --json`; other plugins, the
marketplace registration and unrelated settings are left alone, and the app
deletes no file or directory to do it. No credential is copied into a package.

A client installs a copy of the generated package, so a moved or updated app
(or a new MCP port) would leave the installed copy pointing at the old one.
Each confirmed install records a digest of that client's package in the app's
data directory. After every inventory the app compares each record with what
the running executable would stage, and when they differ it re-stages and
reinstalls the plugin for that client if the client reports it installed; a
failed reinstall keeps the old record and is retried next time. Unchecked
plugins are never reinstalled. Reload the client afterwards. The Cowork ZIP is
uploaded by hand, so after moving the app prepare and upload it again.

Node-scoped `config_*` tools share the same authenticated local endpoint and
remain available with zero workspaces. They inspect the public identity and
typed settings, discover local collection choices, and create/edit/remove
bundles. `config_share_workspace` contributes native grants to an exact current
bundle. For quorum admission it reports pending and the number of distinct roots
contributing to each policy until the requested actions are admitted. The default
grant permits invocation only. Onward delegation requires explaining
consequences and the user's opt-in. The recipient inspects
`config_list_pending_shares` and `config_list_collections`, keeping claimed
access separate from native admission. Descriptor names and declared
roles are evidence for the model's mapping, not mandatory routing or authority.
The model chooses exact existing roots and faculty roles for the requested
work, including an intentional subset, and uses `config_create_workspace`
with explicit routes or `config_edit_workspace` for an existing binding.
Creation without routes creates fresh roots; edit routes replace the complete
binding, so retain intended existing routes. Exact resident roots and native
READ or WRITE admission are checked independently of names.

The pending listing's `enrollable` is a status for the automatic helper,
not a general ability to use admitted collections; when false, inspect the
reported metadata and reason. `config_enroll_pending_share` is an
optional convenience when its inferred mappings and complete-root selection
already match the intended binding. Its exact-all-root checks do not apply
to explicit create/edit. Reuse an existing local binding rather than making
a duplicate. Workspace binding publication separately selects newly requested roots for
enabled native Full sync; selection failure does not roll back publication.
Native grant exchange discovers authority evidence without an app authorization
handshake; a grant alone neither enrolls nor selects a collection.
Peering follows native `Selected` registers, including manual selections outside
bundles. Startup fills only unset selections for current saved/enrolled roots;
unselected/conflicted choices remain intact. Withdrawal does not revoke grants
or unselect native roots. Maintenance remains independently all-accessible and
may acquire exact missing bodies outside sync selection while sync is enabled.
This is not global fetch isolation or a synchronization-completion receipt.
Typed service settings are port/sync/maintenance only. Old requests containing
`peers` are unsupported; historical peer facts remain inert. The app refreshes
installed packages itself; reload the client at the coordinated upgrade.
Sharing returns local native-grant publication metadata. Peer exchange delivers
proofs and sync/repair acquire descriptors; discovery and local enrollment use
the recipient's own app. No sharing file is needed.

Binary faculty inputs use host paths or returned file references. The local
integration is authenticated, but a workspace selects collections rather than
sandboxing host filesystem access. The calling agent must have the user's
authority both to read that local source and to write its chosen destination.
The protected local .1pux chooser/passphrase flow remains separate. The 1 MiB
MCP request cap is unchanged; original bytes stay outside model arguments.
The bundled skill describes native sharing and input limits. Ordinary faculties still require an exact workspace on
each call. Other configuration operations grant no sharing authority; no tool
exposes a private key, token or arbitrary path editor. The app must be open;
this is not remote admin.
