# Standard release CI candidate (inactive)

This isolated source successor combines the frozen AppImage app source,
existing inactive standard-release overlay, and approved three-target website
contract. Exact input identities are recorded in this packet's evidence. It
changes no installed app or live source; neither draft is activated.

The two drafts share `scripts/ci/native-build.sh`. The development draft tests
and packages PR/main source with no release signing. The standard draft accepts
only a tag push in `erlkonig-ai/erlkonig-app`; it requires canonical stable
`vMAJOR.MINOR.PATCH` equal to the actual shared Cargo version and Mac source
plist. Its native jobs rebuild that exact checkout and retained locks. An
arbitrary development artifact cannot become a release.

The build script preserves the existing Rust 1.98.0/native/WASM inputs, public
sibling commit/tree checks, scoped native tests, ABI inspection, icon build and
package scripts. Outputs and caches are outside the source tree. It checks the
actual notices directory (`notices/manifest.json` and `notices/README.txt`), not
the retired `THIRD_PARTY_NOTICES.txt` path. The package probe compares every
packaged notice byte to the selected source.

Linux builds natively on Ubuntu24.04 for x86_64 and aarch64. It reuses the
retained AppRun, desktop/icon staging, pinned appimagetool/runtime metadata and
ELF/SquashFS inspection. The canonical output is
`erlkonig-${VERSION}-${TARGET}.AppImage`. Every target has separate package
bytes, size/hash, native acceptance and transfer records. A static inspection
receipt is never accepted as a native receipt. No x86_64 result is inferred
from the ARM64 Sparks.

The final-package probe checks matching native architecture, accessible FUSE,
AppRun and packaged notices, then passes the exact AppImage path to the existing
isolated Xvfb/X11 first-run/seeded ownership-forwarding-shutdown probe. This
recipe has not executed against the current AppImage. In particular, the
retained probe's PID/listener assumptions through the AppImage mount/child
lifecycle require actual validation before any acceptance receipt. It does not
establish Wayland/KDE, native Quit/menu, peer exchange or real client behavior.
A failure or missing result prevents the native receipt; there is no substitute
success marker. The previous client-isolation incident is not accepted.

Linux compiler provisioning is an explicit retained-root interface,
`ERLKONIG_LINUX_RUST_ROOT` -> `RETAINED_RUST_ROOT`. The source gate requires
per-target Rust1.98.0+wasm byte inventories; the build verifies that preprovisioned
root without installing or selecting a newer compiler. ARM64's exact inventory
is retained; x86_64's is absent and the recipe refuses. No identity is invented.
The runtime/build tooling downloads use retained asset IDs and hashes, never a
moving latest URL. Mac signing tooling/recipe is unchanged.

Mac first builds/packages the current ad-hoc bundle using the existing packager.
The trusted release wrapper signs a copy, notarizes the DMG, staples it and emits
its automatic artifact-bound receipt with actual packaged plist version/build.
The final distributed DMG is mounted read-only and its executable runs through
the current `isolated_app.py` seeded fixture, duplicate/forwarder/restart and
noninteractive AppKit termination checks. This is not empty-profile onboarding,
observed menu Quit, migration, peer exchange or comprehensive UI acceptance.
A fresh helper from the same build supplies its fixture. No live profile or
installed app is selected. Real signing and this runtime path were not exercised
while preparing this source candidate.

`record_native_acceptance.py` and `produce_release_manifest.py` are imported from
the single immutable site checkout, with no second manifest implementation. The
acceptance wrapper hashes final package bytes before/after the fixed native
probe and binds the source version and run/attempt. Native jobs transfer only
one package, its acceptance receipt and (Mac) signing receipt, plus fixed mapping.
The publisher downloads all three named artifacts from the same run and attempt;
missing, extra, swapped, symlinked or wrongly signed transfer entries fail.
Build environment and runtime result JSON are retained in a separate bounded
Actions artifact, excluding fixture keys, piles, tokens and logs.

Publication jobs use one concurrency group across tags. GitHub concurrency is
not a durable queue for every rapid tag push; pending jobs can be superseded.
This candidate does not add a separate release queue.

The publisher creates a fresh draft with `--verify-tag`, uploads packages and
receipts without clobbering, invokes the shared producer against actual draft
asset metadata, uploads the manifest last, then publishes with `make_latest=false`. It re-reads the public
release and package bytes through the existing `refresh_downloads.collect`
and uses the existing catalog selector to mark its highest complete stable ID
as GitHub latest, with readback, before dispatching `erlkonig-release-complete` to `erlkonig-ai/sites`. Failure
before publication leaves at most a draft; failure after publication leaves the
previous site source unchanged. Do not rerun creation to replace an existing
release. Once a published release is complete/public, recovery uses the existing
site draft's exact `release_id` workflow-dispatch input; it revalidates everything
without republishing or rebuilding.

## Inputs still required before activation

- Complete the checked-in source-input gates, actual public closure, retained
  lock qualification and license/notice decisions. Current flags remain false;
  the candidate refuses them. Current retained packaging provenance is not a
  public tagged source commit. No source/revision substitution is made.
- Integrate this overlay and the accepted site implementation into reviewed
  commits. Set `ERLKONIG_SITE_INTEGRATION_COMMIT` to that site's full immutable
  commit, never a moving branch. Existing `source-inputs.json` pins actual sibling
  commits and records the already-integrated Faculty overlay as evidence only.
- Set `ERLKONIG_RELEASE_MAC_RUNNER_JSON` to the accepted ephemeral trusted ARM64
  runner labels. It needs an AppKit session, selected accepted Xcode, Python
  3.11.8+ with tarfile data filtering, Rust tooling, GitHub CLI and existing
  Developer ID identity/notary profile. Identity/profile names are explicit
  noncredential variables `ERLKONIG_MAC_SIGN_IDENTITY` and
  `ERLKONIG_MAC_NOTARY_PROFILE`. There is no certificate import, profile creation,
  password prompt or shared-machine Xcode mutation in this candidate.
- Bind the existing cross-repository dispatch authority to the publisher's
  `SITE_DISPATCH_TOKEN` environment interface. The draft deliberately contains
  no guessed secret mapping. Without that capability it refuses before draft
  creation. `GITHUB_TOKEN` is scoped to same-repository release writes; it is
  not assumed to authorize the sites repository. This is the remaining concrete
  credential-integration seam, not a second activation policy.
- Exercise all three target-native probe paths and actual hosted resource budget;
  `df` is logging only, and an ephemeral-runner label does not establish resource
  admission or adequate RAM/disk. No measured admission limits are invented;
  source tests cannot certify native runtime, signing or runner behavior.
- Install the receiving site workflow on the site's actual default branch and
  bind its explicit source branch/accepted ancestor. `repository_dispatch`
  executes default-branch workflow code. The live design source is 83e5157ef075a9e7df2d0b4e9b1fb2ce62824bb4;
  Sky's clean gorbie-look 7085 checkout is not the deployment baseline.
  Public live headers establish Netlify. Later local evidence found cached site
  ID 044526fe-fe84-41d5-9c6f-280b56716f41 and a workspace-root publish path, but
  their association with erlkonig.ai remains unidentified. Neither cache proves
  the production project, branch, publish directory or trigger. No migration is
  invented. A GITHUB_TOKEN source push does not trigger another Actions push
  workflow; the actual host binding remains a separate existing-host input.
- Reconcile any historical published releases without the standard manifest;
  the unchanged complete-history collector currently refuses those rather than
  fabricating availability or silently promoting an incomplete update.

Source-only successor checks: 28 orchestration/refusal/wiring tests, 55 shared
site/manifest tests, Python AST, YAML parsing and shell syntax. Mac signing
source is byte-identical to the earlier 24-stub-test candidate; those signing
stubs were not rerun for this unchanged script. These checks perform no native build, native app launch, signing,
credential access, workflow run, release creation or deployment.

Official interfaces consulted:
[GitHub tag events](https://docs.github.com/en/actions/reference/workflows-and-actions/events-that-trigger-workflows#push),
[gh run download](https://cli.github.com/manual/gh_run_download),
[gh release create](https://cli.github.com/manual/gh_release_create),
[release API](https://docs.github.com/en/rest/releases/releases#update-a-release),
[repository dispatch](https://docs.github.com/en/rest/repos/repos#create-a-repository-dispatch-event).

## App advisory notice

`update_catalog.rs` now recognizes unsigned AppImage entries for both native
Linux triples; Mac DMG rules are unchanged. The exact three-target latest.json
fixture is generated by the same website renderer. The notice remains a menu
link to `https://erlkonig.ai/#downloads`; it downloads no app and installs
nothing. Rust tests were prepared, not executed by this source-only task.

Runner labels `ubuntu-24.04` and `ubuntu-24.04-arm` were checked against
https://docs.github.com/en/actions/reference/runners/github-hosted-runners .
Labels do not establish RAM/disk/FUSE capacity or authorize a CI execution.
