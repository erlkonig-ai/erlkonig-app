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
bundles. `config_share_workspace` grants native access immediately to an exact
current bundle, invoke-only by default; onward delegation requires explaining
consequences and the user's opt-in. The normal flow is then recipient-local:
`config_list_pending_shares` lists the collections resident native grants name
for this node, grouped by the issuing workspace, with the exact roots, the
claimed and the admitted access and the role (declared, or from the name
suffix, as `role_source` says), and `config_enroll_pending_share` publishes a
local label over exactly the roots shown, refusing a set that changed since
the listing; a later extension takes only roots the listing shows beside the
enrolled workspace. The JSON handoff is the fallback for nodes that have not
connected yet: `config_probe_workspace_share` verifies the bounded signed
role/root/name metadata without mutation (the signature/recipient binding,
not collection admission; no proof or definition payload travels in the
handoff) and `config_enroll_workspace_share` publishes a local label and the
same roots under the existing key. Either enrollment then separately selects
newly requested roots for enabled native Full sync; selection failure does
not roll back publication.
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
The bundled skill describes the handoff and limits. Ordinary faculties still require an exact workspace on
each call. Other configuration operations grant no sharing authority; no tool
exposes a private key, token or arbitrary path editor. The app must be open;
this is not remote admin.
