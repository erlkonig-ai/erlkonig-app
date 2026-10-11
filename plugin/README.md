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

An installed but disabled plugin remains checked and says “disabled.” An
inventory failure remains visible, disabled, and says “status unavailable,”
retaining the last observed check. Not detecting any supported client leaves
a small “No supported clients detected” entry and the refresh action. Detection
never removes an integration or changes client settings.

Node-scoped `config_*` tools share the same authenticated local endpoint and
remain available with zero workspaces. They inspect the public identity and
typed settings, discover local collection choices, and create/edit/remove
bundles. Ordinary faculties still require an exact workspace on each call.
Configuration grants no sharing authority and exposes no private key, token
or arbitrary path editor. The app must be open; this is not remote admin.
