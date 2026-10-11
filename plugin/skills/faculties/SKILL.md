---
name: faculties
description: Use Erlkonig faculties for durable goals, notes, memories, files and messages, or to configure and share Erlkonig workspaces and node services through its local integration.
---

# Faculties

The `faculties` tools come from the Erlkonig app on this computer. One node
identity and one append-only local store serve every workspace. A workspace
is a named bundle of collections for the tools, not another pile or key.
Collection capabilities determine access. Faculty tool names include
`compass_add` and `wiki_search`.

## Agent-led setup

For requested setup, call `config_inspect`: it needs no workspace and returns
the public node ID, typed settings, workspace revisions and an opaque
observation. These node-scoped tools use the existing authenticated local
integration while Erlkonig is open, even with zero workspaces.

Use `config_create_workspace` with a friendly label for a fresh private bundle.
To organize existing data, use `config_list_collections`, then supply explicit
role/root choices. Explain ambiguous choices in ordinary language; use returned
IDs yourself. `config_edit_workspace` preserves omitted routes; supplied routes
replace the entire bundle. `config_remove_workspace` withdraws one exact
revision without deleting its collections or choosing a concurrent winner.
`config_set_settings` takes complete typed port, sync, maintenance and public
peer-ID settings. A port change requires app restart and integration refresh.

Pass the inspected observation on each edit. If stale, inspect again and
reconsider the intended change; do not blindly replay it. Conflicted settings
need an explicit complete reconciliation. Configure only what the user asked
to change. Bundle/settings edits do not grant collection access, change identity
or choose a workspace for another call. Sharing uses the explicit sharing
operations below. Never paste secrets into these tools or
send a public identity to another person without a request to do so.

## Share a workspace

For a requested share, inspect the current workspace and ask the intended
recipient to obtain their **public node identity** from their own
`config_inspect`. No secret key, pile path or replacement identity is needed.
Use `config_share_workspace` with the exact current workspace revision,
recipient identity and the user's read-only or read/write choice. The native
grant covers only the currently resolved roots and actions, not older or future
workspace membership. A workspace edit does not expand or revoke an issued
share. Grants are valid immediately, additive and non-expiring; removing a local workspace
does not withdraw another node's authority. A narrower share does not
remove permissions the recipient already holds.

Leave `delegate` false unless the user explicitly opts in after you explain
the consequence: **the recipient may invite additional participants within
these exact roots/actions without the original user choosing each person**.
Invocation alone never permits onward delegation. Also explain that disclosed
or copied data cannot be made unread, whether or not delegation is enabled.
Ask about the consequence in ordinary language, not capability jargon.

Hand the opaque returned `bootstrap` unchanged to the intended recipient only
through the user-requested channel. It contains signed role/root metadata and
exact descriptor/name blobs, not capability proofs, definition bodies, keys or
member payloads. Current native sync
has no identity-wide cold grant discovery: this is a one-way bootstrap, not a
request/approve/acknowledge protocol. Do not print its internal handles
unnecessarily or reconstruct it.

On the recipient's node, call `config_probe_workspace_share` first. This is
read-only: it verifies the native signature and recipient binding, NOT actual
collection admission. It reports the issuer's declared access choice and
reports the current exact collection names/roles/roots without importing,
activating repair, downloading bodies or starting upkeep. Explain what is
available and the disk/network/maintenance consequences. Let the user/agent
choose whether to enroll it and a friendly **local label**. Do not infer roles
by parsing names or expand membership from common descriptor tags.

For that chosen enrollment, inspect and call `config_enroll_workspace_share`
with the bootstrap, local label and latest observation. It imports descriptor/
name metadata and configures the **same roots and role routes under the existing
node key**. Rights already existed: no acknowledgement or approval goes to the
source. Use the returned local workspace ID on subsequent faculty calls;
there is no global default. Repeat unchanged enrollment returns the same
workspace, never overwriting a later edit or resurrecting a withdrawal.

Enrollment selects known roots for Full bootstrap even while local READ
evidence is pending. The server's resident native evidence admits the endpoint;
AUTH repair supplies proofs, descriptor warmup supplies policy definitions,
and Full held(C) hydration supplies custom delegate definitions. Native
evidence still governs local reads/writes and upkeep. Existing all-accessible
node selection remains unchanged: do not claim arbitrary resident unenrolled
grants are globally resource-gated. Off-store probing selects nothing.

Share/enroll responses confirm local publication, **not replica synchronization
or payload availability**. Enrollment keeps service settings and peers unchanged
but selects its roots for existing enabled sync/maintenance. If paused, explain
that data may remain unavailable. Issuer identity is only a routing hint;
native descriptor/contact/provider discovery can remain pending. Never silently
edit peer settings. Native grant and metadata validation precede publication;
the final Config workspace is one native commit, but the append-only metadata
publication is not a rollback transaction. Interrupted I/O may retain metadata.
Removing enrollment does not promise immediate native topic deactivation.
Reinspect after an error; do not invent a key-copy, shell-grant or second-service workaround.

## Choose the data workspace

Call `list_workspaces` first. It reads app configuration only and returns
available workspaces with human labels, opaque revision IDs and configured
collection routes, plus unavailable entries with reasons. Choose
the workspace appropriate to the user's request; ask when the destination is
ambiguous. Do not guess from a label alone. Use the returned identifiers
yourself; do not ask the person to transcribe hexadecimal IDs. Use the Config
tools above for requested bundle creation or editing.

Pass its exact returned ID as `workspace` on **every ordinary faculty call**.
Only node-scoped Config tools and `list_workspaces` omit that argument.
There is no default, node-pile fallback or connection-global selection.
Do not supply arbitrary pile paths, signing-key paths or replacement roots.
The app resolves and freezes the binding for that call. Normal Config edits
supersede the old request ID; re-list after a binding changes or becomes
unavailable. IDs are opaque selectors, not immutable manifests: native facts
can change the value resolved under an existing ID on later calls.

Omitted roles are unavailable, including auxiliary roles a tool needs. Report
the missing configuration; do not synthesize a private root or switch
workspaces silently. Workspace reads are local-only and may report missing
payloads. Workspace selection does not change the node's sync or maintenance
settings or start a second service. Node synchronization includes permitted
collections independently of the current request's workspace.

If access is missing, explain that a node with the appropriate native grant
authority must invite the existing node identity with the required access.
Use the sharing workflow above when requested. Other Config tools and public
peer bootstrap hints do not grant authority. Never ask for a secret key or
create another identity as a workaround.

Reading workspace A and writing workspace B require separate explicit calls
and the user's authority for that destination. Selection does not automatically
copy or union data, and there is no cross-workspace transaction. Workspace routing
is not an execution sandbox for tools that accept host paths or commands.

## Where things go

| The user has | Put it in | Start with |
|---|---|---|
| A paper, PDF, dataset or other document | files | `files_add`, `files_search`, `files_show` |
| Knowledge: a finding, a method, a written-down belief, a how-to | wiki (Typst text) | `wiki_search`, `wiki_show`, `wiki_create`, `wiki_edit` |
| A task, goal or piece of work that outlives this chat | compass | `compass_list`, `compass_add`, `compass_move`, `compass_note` |
| A journal entry: what happened and what it was like | memory | `memory_create`, `memory_search`, `memory_context` |
| People, organisations and groups | relations | `relations_list`, `relations_show`, `relations_add` |
| A choice with options and factors | decide | `decide_propose`, `decide_factor`, `decide_resolve` |
| A message to another person or agent | message | `message_send`, `message_list`, `message_ack` |
| A question about the current situation | orient | `orient_show` |

A paper goes into `files` first, and what the user learns from it goes into
the `wiki`, linking the file. A decision about what to do next is a `compass`
goal, not a wiki page.

## Rules

- **Never ask for a secret in chat.** Some tools need an API key, a token or
  a model (web search, mail, discord, linkedin, voice, hear, triage and
  others). When one answers that it is not set up, tell the user in one
  sentence that it needs to be set up in the Erlkonig app or by their admin,
  and continue without it. Do not ask them to paste a key here.
- **Read before you write.** Search the wiki and list goals before creating
  an entry, so you add to existing work instead of duplicating it.
- **First use.** Empty results describe the selected roots, not the entire
  pile. If the user wants starter content in this workspace, ask once whether
  to run `bootstrap_import`, which adds a guided tour and seven starter goals.
  Its required roles must be configured; the import is idempotent.
- **Attribution.** A `persona` or `from` argument must name someone who
  already exists in relations, or the call fails with `no persona matches`.
  Leave it out unless the user wants entries attributed.
- **Wiki text is Typst.** Put identifiers containing `_` or `<...>` in
  backticks, or the parser reports `unclosed delimiter`.
- **Tool errors are answers.** A tool that returns an error has said what is
  missing. Tell the user in one sentence and continue; do not retry the same
  call in a loop.
- **Keep Erlkonig open.** The plugin forwards to the already-running app; it
  never opens the pile or starts a background backend itself. If the connection
  fails, ask the user to open Erlkonig on this computer. After the app is moved
  or updated, use its plugin menu's Refresh package action (or upload the new
  Cowork ZIP), then reload the client. Never start another service to repair it.
- **Shared spaces are shared.** Content written to a team space reaches
  everyone in it. Do not copy local files or private notes into a shared
  space unless the user asked for exactly that.
