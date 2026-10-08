---
name: faculties
description: Use the Erlkonig faculties tools to keep durable memory across conversations. Use when the user asks to remember, track or look up goals, decisions, notes, papers, people, files or messages, when they ask what was decided or done before, or when work should outlive this conversation.
---

# Faculties

The `faculties` tools come from the Erlkonig app on this computer. Data lives
in append-only piles selected through explicitly configured query workspaces.
Each workspace binds an enrolled pile, its signing identity and exact roots
for configured faculty roles. Writes use that selected signer, not necessarily
the app's node key. Faculty tool names include `compass_add` and `wiki_search`.

## Choose the data workspace

Call `list_workspaces` first. It reads app configuration only and returns
available workspaces with human labels, exact revision IDs, public signers and
configured role/root handles, plus unavailable entries with reasons. Choose
the workspace appropriate to the user's request; ask when the destination is
ambiguous. Do not guess from a label alone.

Pass its exact returned ID as `workspace` on **every faculty tool call**.
There is no default, node-pile fallback or connection-global selection.
Do not supply arbitrary pile paths, signing-key paths or replacement roots.
The app resolves and freezes the binding for that call. Normal UI edits
supersede the old request ID; re-list after a binding changes or becomes
unavailable. IDs are opaque selectors, not immutable manifests: native facts
can change the value resolved under an existing ID on later calls.

Omitted roles are unavailable, including auxiliary roles a tool needs. Report
the missing configuration; do not synthesize a private root or switch
workspaces silently. Workspace reads are local-only and may report missing
payloads. Selection never activates sync, maintenance, notifications or network
acquisition; separately configured managed sync is independent.

Reading workspace A and writing workspace B require separate explicit calls
and the user's authority for that destination. Selection does not automatically
copy or union data, and there is no cross-pile transaction. Workspace routing
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
