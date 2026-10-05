# Testing Erlkönig on the Mac (milestone 1)

These are the steps that need your GUI session. Everything that can be checked
without a screen has already passed against this exact bundle (see "Already
checked" at the end). Each step says what you should see and what to send CC.
CC follows along over ssh with `scripts/check-node.sh`, which only reads.

The bundle is `~/Erlkonig/build/dist.noindex/Erlkonig.app` (101 MB, app commit
`812d206`, faculties `0.20.2 (554b73309)`, trible `0.47.0` from triblespace-rs
`dad2ac6`, code hash `77ac6f47`). It is
signed ad hoc and not notarized, so it is for this Mac only.

Nothing here touches the colony pile. The app makes its own new, empty pile
and key in `~/Library/Application Support/Erlkonig`.

**About ad hoc signing.** macOS identifies an ad hoc signed app by the exact
hash of its code, so every rebuild looks like a new app to it: Login Items may
ask for approval again, and an old entry can stay behind. That is expected for
these test builds. A stable signing certificate would fix it; there is none on
this Mac yet (see "Not covered yet").

## 1. Put the app in Applications

In Terminal:

    ditto ~/Erlkonig/build/dist.noindex/Erlkonig.app /Applications/Erlkonig.app

You can also copy it in Finder: Go > Go to Folder, enter
`~/Erlkonig/build/dist.noindex`, then Option-drag Erlkonig.app onto Applications.

Use Applications rather than opening it from `~/Erlkonig/build`. The login item
and the `bin` link that the plugins use both remember where the app lives. A
rebuild would also replace the copy in `~/Erlkonig/build` while the app is
running. (Spotlight skips folders named `.noindex`, so the copy in
Applications is the only Erlkönig it should offer.)

If an older Erlkönig is already in Applications, follow "Updating to a newer
build" below instead of copying over it.

**Send CC:** "copied", or the error if the copy failed.

## 2. First launch

Double-click Erlkonig in Applications.

**You should see:**
- No Dock icon and no window.
- A small crown in the menu bar, black or white to match the menu bar. On a
  MacBook with a notch, a full menu bar can hide it behind the notch. If you
  see no crown, quit a menu-bar app or two and look again.
- Within a second, a dialog "Run Erlkönig in the background?" with the buttons
  **Run in the background** and **Not now**. Click **Run in the background**.

A locally built app carries no quarantine flag, so macOS should open it without
asking. If it says the app "cannot be opened" or comes from an unidentified
developer, go to System Settings > Privacy & Security, click **Open Anyway**,
and tell CC.

**Send CC:** whether the crown appeared, whether the dialog appeared, and a
screenshot of anything else macOS showed.

## 3. Approve the login items

Run in the background registers two things: the background node, and the
menu itself as an app that opens at login. macOS decides whether the node
needs your approval. Three outcomes are possible:

- A notification, "Background Items Added", naming Erlkönig (or
  erlkonig-node).
- One dialog from the app, "Allow Erlkönig in Login Items", with the button
  **Open System Settings**. (There should be only this one dialog. Before this
  build a "macOS refused" alert came first; tell CC if you see that.)
- Nothing at all, if macOS allowed it straight away.

If you were asked, go to System Settings > General > Login Items &
Extensions. Under "Allow in the Background", turn on the entry for Erlkönig.
Erlkönig should also be listed under "Open at Login".

Optional, if you want to see what macOS recorded (it asks for your password):

    sudo sfltool dumpbtm | grep -i -B2 -A12 erlkonig

**Send CC:** the exact text of the notification or dialog, the names the
entries have in Login Items (both sections), whether the background switch is
on, and the `sfltool` output if you ran it. Then say "approved". CC will run
`scripts/check-node.sh` and should see `ai.erlkonig.node` loaded and running
with its own pid, started as `erlkonig-node run --launchd`.

## 4. The status line

Click the crown. The first line is grey and can't be clicked. That is the
status.

**You should see:** `Running · no peers · sync off`. No peers and no sync is
correct: the app has no `node.toml` yet. **Open at login** should be ticked.

Other things the line can say:

| Status line | Meaning |
|---|---|
| `Erlkönig is not running` | the background node has not started (not approved yet, or it failed) |
| `Waiting for approval in System Settings > Login Items` | step 3 is still open; Open at login is ticked, and clicking it offers **Open System Settings**, **Turn off** or **Cancel** |
| `Problem: …` | the node runs but cannot do what it was asked |
| `Installing in ChatGPT…` / `Installing in Claude Code…` | an install is running; the menu stays usable, and a dialog reports the result |
| `Move Erlkönig to Applications to finish setup` | macOS is running a temporary copy; see step 1 |

The line refreshes every two seconds.

**Send CC:** the exact text of the status line, and whether Open at login is
ticked.

## 5. Copy my ID

Click **Copy my ID**. For about four seconds the status line shows
`Copied your ID 1a2b3c…7d8e9f`. Paste the clipboard into your message to CC.

**Send CC:** the pasted ID. It should be 64 characters of 0–9 and a–f. CC will
check that it matches `node_id` in `node.status`.

## 6. Install in Claude

1. Click **Install in Claude**.
2. Finder opens with `erlkonig-claude-plugin.zip` selected, in
   `~/Library/Application Support/Erlkonig`.
3. A dialog lists the upload steps. Claude Code is installed on this Mac, so
   the dialog also offers **Also install in Claude Code**. Click **Done**, not
   that button: installing into Claude Code writes to your real `~/.claude`,
   and that is a separate decision.
4. In the Claude desktop app, open Customize, then Plugins, then Add plugin,
   then Upload plugin. Pick the zip. In the file dialog you can press
   Cmd-Shift-G and paste
   `~/Library/Application Support/Erlkonig/erlkonig-claude-plugin.zip`.
5. The plugin **Erlkönig** should appear, with a skill named `faculties` and
   an MCP server named `faculties`.
6. Start a **local Cowork task** (not an ordinary chat: ordinary chats do not
   run local tools) and ask: *"Use the Erlkönig tools to list my compass
   goals."* Allow the tool if Claude asks. You should see a call to
   `compass_list`, and the answer "No goals yet." The pile is new and empty.

This step tests something no headless check could: whether Cowork starts the
plugin's MCP server on the Mac itself. If the error says **"Erlkonig is not
installed on this Linux computer"**, Cowork ran it inside its Linux virtual
machine instead. That is a finding, not a mistake on your side; send it as it
is.

**Send CC:** a screenshot of the plugin's page in Customize > Plugins, and the
tool call and its result from the Cowork task, or the exact error. If the tool
fails, a screenshot of any MCP error under the plugin's details helps most.

## 7. Quit the menu and check that the node keeps running

Click **Quit Erlkönig Menu**. The crown goes away. Only the menu has quit: the
background node should keep running.

**Send CC:** "quit done". CC runs `scripts/check-node.sh` and should see the
node still running with the same pid as in step 3, and a fresh `node.status`.

Then open Erlkonig from Applications again. The crown comes back. The
"Run in the background?" question should **not** come back, because it is
asked only once. The status line should again read `Running · no peers · sync
off`.

**Send CC:** whether the crown came back and whether any dialog appeared.

## 8. Log out and back in

Log out (Apple menu > Log Out) and log in again, or restart the Mac if that is
easier.

**You should see:** the crown in the menu bar again without opening anything,
and `Running · no peers · sync off` in its status line.

**Send CC:** whether the crown came back by itself, and whether macOS showed
any login-item notification or dialog. CC runs `scripts/check-node.sh` and
should see the node running with a new pid, and the same `node_id` as in
step 5.

## 9. Not yet: Install in ChatGPT

Please do **not** click **Install in ChatGPT** yet. It runs ChatGPT's own
`codex` to add a plugin marketplace and the plugin, which writes to your real
`~/.codex`. Sol (Liora-GPT) lives in `~/.codex`, so this waits until Sol
agrees. It has passed with a scratch `HOME` and `CODEX_HOME`: the plugin
installed, 229 tools listed through `codex app-server`, and `compass_list`
answered in a thread. If you click it by accident, tell CC straight away.

## Updating to a newer build

Copying a new build over a running one can get the running processes killed
for an invalid code signature, so replace it this way:

1. Untick **Open at login**. This stops and unregisters the background node.
2. Click **Quit Erlkönig Menu**.
3. Quit Claude and ChatGPT if they use the Erlkönig plugin, so nothing runs the
   old `faculties`.
4. In Terminal:

       mv /Applications/Erlkonig.app ~/.Trash/Erlkonig-old.app
       ditto ~/Erlkonig/build/dist.noindex/Erlkonig.app /Applications/Erlkonig.app

5. Open Erlkonig from Applications and tick **Open at login** again. Because of
   ad hoc signing, macOS may ask for approval again (step 3). An entry for the
   old build may stay in Login Items; turn it off there if it does.

Your pile, key and settings in `~/Library/Application Support/Erlkonig` stay
as they are.

## Undo everything

1. Untick **Open at login**. This unregisters the background node and the
   menu's login item.
2. Click **Quit Erlkönig Menu**.
3. Delete `/Applications/Erlkonig.app`.
4. In Claude, remove the plugin under Customize > Plugins.
5. `~/Library/Application Support/Erlkonig` holds only the app's own new pile,
   key, logs and the zip. You can delete it.

## What CC runs over ssh

    ~/Erlkonig/erlkonig-app/scripts/check-node.sh

It only reads. It prints these sections:
- the LaunchAgent as launchd sees it (`launchctl print gui/$(id -u)/ai.erlkonig.node`)
- the `bin` link and a `codesign --verify` of the bundle it points into
- `node.status`, including any problem and warning
- the node's process tree
- the last lines of `node.log` and of each child's log

The headless checks are `scripts/accept.sh`, run against the assembled bundle.

## Already checked, headless, against this bundle's own binaries

`scripts/accept.sh` passed on 2026-10-05 against this bundle (exit 0):

- `codesign --verify --deep --strict` passes, both plists pass
  `plutil -lint`, and the LaunchAgent runs `erlkonig-node run --launchd` with
  `KeepAlive {SuccessfulExit: false}` and `ProcessType Standard`.
- The node creates a 0700 data folder, an empty pile and a key (both 0600).
- A second node run by hand exits 0, SIGTERM stops the node cleanly, and the
  key is reused after a restart.
- A node started like the LaunchAgent (`run --launchd`) waits while another
  node holds the lock, instead of exiting into launchd's restart loop, and
  takes over within a second once the other node stops.
- The maintenance child restarts with backoff (1 s, 2 s, 4 s) after
  `kill -9`.
- When the node itself is killed with SIGKILL, its maintenance child is left
  behind; the next node stops it, even when the dead node's pid now belongs to
  another live process, and that process is not touched. Exactly one
  `maintain-all` runs afterwards.
- A pile named in `node.toml` that is missing at start (a disk not mounted
  yet) is not created, and is picked up without any edit once it appears.
- The plugin's MCP command reaches 229 tools from 34 faculties through the
  bundled faculties. faculties sees none of the planted variables, and a
  planted `ERLKONIG_BIN_DIR` (with a fake faculties) and `ERLKONIG_DATA_DIR`
  have no effect.
- With a scratch `CLAUDE_CONFIG_DIR`, Claude Code installs the plugin (also
  when started with a bare PATH, as from the menu) and shows `✔ Connected`.
- With a scratch `CODEX_HOME`, ChatGPT's codex installs the plugin, lists 229
  tools through `codex app-server`, and answers `compass_list` in a thread.
- Your real `~/.codex` and `~/.claude` files have the same checksums before and
  after, and nothing is left running.

`cargo test` (47 tests), `cargo clippy -D warnings` and the release build pass
at the same commit.

## Not covered yet

- **A stable signing identity.** This Mac has no code-signing certificate
  (`security find-identity -v -p codesigning` finds none), so builds are ad
  hoc. `scripts/bundle.sh --sign <identity>` uses one as soon as there is one.
- **Local Network access.** Sync is off in milestone 1, so nothing here
  exercises it, and checks over ssh could not: macOS exempts processes started
  over ssh. When sync is turned on, one test has to run from your GUI session.
- **A pile in Desktop, Documents or Downloads.** macOS guards these folders,
  and the background node is subject to that. If `node.toml` names a pile or
  key there, the node warns in `node.status` and its log; expect a permission
  prompt, or a refusal, when it is first used, and again after a rebuild while
  the app is signed ad hoc.
