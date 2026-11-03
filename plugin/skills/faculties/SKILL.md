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
`config_set_settings` takes complete typed port, sync and maintenance settings.
There is no peer-bootstrap field. Old requests containing `peers` are unsupported;
at the coordinated upgrade refresh the client package/schema and reload the
client (or upload the refreshed Cowork archive). A port change also requires
app restart and integration refresh.

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

The normal flow needs no handoff. The owner runs `config_share_workspace`;
native grant exchange delivers proofs to the recipient's node when the nodes
connect, and sync delivers descriptors. On the recipient's node, inspect
`config_list_pending_shares` and `config_list_collections` (both read-only,
no workspace needed). Keep proof claims (`claimed`, `claimed_access`)
separate from effective native access (`admitted`, `admitted_access`, and
the collection's independent `read`/`write` values).

The model chooses collection-to-faculty mappings for the requested work.
Descriptor names, declared roles and `role_source` are evidence for that
choice, not authority or a mandatory mapping. An unknown role or
`enrollable:false` does not mean the recipient cannot use an admitted root.
That field is a status for the automatic `config_enroll_pending_share`
helper, not the general ability to bind a collection. Inspect the reported
metadata and explain the actual reason. A missing
descriptor or missing native admission is a different condition; explicit
routing does not fetch the descriptor or grant permission.

Choose the exact roots and faculty roles needed within the user's scope;
an intentional subset of the shared bundle is valid. Explain those selected
roots and their disk/network/maintenance consequences, and choose a friendly
**local alias** with the person when creating a binding. With a fresh
`config_inspect` observation, use `config_create_workspace` with explicit
`routes: [{role, root}, ...]`, or `config_edit_workspace` for an existing
local binding. Supply `routes` when joining existing data: omitting it on
creation creates fresh private roots. On edit, supplied routes replace the
complete bundle, so include the existing routes the user wants to retain.
The tools check the known faculty role, exact resident named root and native
READ or WRITE admission; the model supplies the semantic mapping. Explicit
routing does not require every granted root or matching descriptor names.

The automatic `config_enroll_pending_share` helper remains available when
its inferred mappings and complete-root selection already match the intended
binding. Its exact-root checks apply to that helper, not to explicit
create/edit. `routed_in` identifies roots already used by another local
workspace; reuse or edit an intended existing binding rather than creating a
duplicate. Successful binding publication separately selects newly added
roots for native sync. Rights already existed: no acknowledgement, key copy
or new grant is involved, and a grant alone neither creates a binding nor
selects its collections.

The JSON handoff is the fallback for when the nodes have not connected yet.
Hand the opaque returned `bootstrap` unchanged to the intended recipient only
through the user-requested channel. It contains signed role/root metadata and
exact descriptor/name blobs, not capability proofs, definition bodies, keys or
member payloads. Native grant exchange discovers capability evidence separately;
this metadata handoff is not authority delivery, automatic enrollment, or a
request/approve/acknowledge protocol. Do not print its internal handles
unnecessarily or reconstruct it. On the recipient's node, call
`config_probe_workspace_share` first. This is read-only: it verifies the
native signature and recipient binding, NOT actual collection admission. It
reports the issuer's declared access choice and the current exact collection
names/roles/roots without importing, activating repair, downloading bodies or
starting upkeep. Explain what is available and the consequences, and let the
user choose whether to enroll it and a local label. Do not expand membership
from common descriptor tags. For that chosen enrollment, inspect and call
`config_enroll_workspace_share` with the bootstrap, local label and latest
observation. It imports descriptor/name metadata and configures the **same
roots and role routes under the existing node key**.

Either way, use the returned local workspace ID on subsequent faculty calls;
there is no global default. Repeat unchanged enrollment returns the same
workspace, never overwriting a later edit or resurrecting a withdrawal.

Sync peering and Full hydration follow native collection-selection registers,
not arbitrary admitted grants or the workspace chosen for one call. Startup
selects only current saved/enrolled roots whose native choice is `Unset`;
`Selected` is a no-op, while `Unselected` and `Conflicted` are preserved.
Manual native selections outside workspace grouping remain authoritative.
An explicit creation, new route or enrollment publishes its workspace first,
then separately selects newly requested roots, even if READ evidence is pending.
Native grant exchange and participant discovery deliver authority evidence;
native admission still governs reads/writes and upkeep. Off-store probing
selects nothing. Maintenance remains independently all-accessible and, with
sync enabled, can fetch exact missing bodies outside sync selection. Do not
claim global resource gating or silently tighten that maintenance policy.

Share/enroll responses confirm local publication, **not replica synchronization
or payload availability**. Enrollment keeps service settings unchanged.
If sync is paused, data may remain unavailable. Native grant-chain, owner and
exact descriptor-blob provider discovery can remain pending; an issuer identity
alone grants no authority. Native metadata validation precedes publication;
workspace and native selection use separate commits, not a rollback transaction.
A selection failure leaves the workspace published. Removing enrollment
withdraws local routing, not native sync selection or authority.
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
workspaces silently. An explicit Faculty operation may acquire exact missing
payloads through a lazy, non-serving client, even with background sync paused.
The bound node is its first contact; authenticated discovery can lead to other
providers. It creates no collection selection or replication interest. Workspace
selection does not change sync or maintenance settings or start a second sync
service. Node synchronization follows native-selected collections independently
of the current request's workspace.

If access is missing, explain that a node with the appropriate native grant
authority must invite the existing node identity with the required access.
Use the sharing workflow above when requested. Other Config tools and native
selection do not grant authority. Never ask for a secret key or
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
  sentence that it needs to be set up in the Erlkonig app or by their admin.
  For requested local credential entry, use `secrets_enter_local` with the exact
  selected workspace, service, storage name and `kind` (`login` or `secret`).
  Those arguments are metadata only: the person types the value in the app,
  never here. Login creates a native structured encrypted item with separate
  username/password semantic roles, not JSON; generic entry keeps its exact raw
  UTF-8 bytes. The storage name is a public label. Keep the call/connection open
  for Save or Cancel. Report only status/version ID; never export private facts
  or values to verify Save.
  Cancel saves nothing; a lost response or `outcome_unknown` requires metadata
  inspection (`secrets_items` for login, `secrets_list` for generic entry) before
  any new entry, never an automatic retry. No sharing grant is created;
  keep the Secrets route separate from an invoice-only shared bundle. If local
  entry is unavailable, continue without it. Do not ask them to paste a key here.
  For metadata-only inspection, use `secrets_item_inspect` with the exact version
  and body; `secrets_item_open` without a body lists body alternatives only.
  Supplying an exact body to `secrets_item_open` exports PRIVATE FACTS, including
  plaintext inline integer/flag values: this is not a metadata-only operation and
  requires explicitly authorized disclosure. Out-of-line text and attachment
  children remain ciphertext handles. `secrets_item_export` additionally requires
  the exact child handle and encoding and discloses that child's plaintext to the
  caller. Do not silently choose a body alternative or collapse repeated or
  unknown roles. Old raw versions retain the `secrets_get` byte contract.
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
  never opens the pile or starts a background backend itself, and it holds no
  credentials. If the connection fails, ask the user to open Erlkonig on this
  computer. After the app is moved or updated, Erlkonig reinstalls this plugin
  by itself when it starts; the user only reloads the client (a Cowork upload
  needs a freshly prepared ZIP). Never start another service to repair it.
- **Shared spaces are shared.** Content written to a team space reaches
  everyone in it. Do not copy local files or private notes into a shared
  space unless the user asked for exactly that.
