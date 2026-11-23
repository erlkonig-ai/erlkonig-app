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
from an ARM64 build.

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

## Release inputs

The source pins and release eligibility checks are in `ci/source-inputs.json`.
The release requires the exact accepted site revision, a trusted macOS ARM64
runner with its signing and notarization configuration, and a publication
credential authorized for the release site. Before enabling either workflow,
validate the checkout on all three native targets, check the final packages
and attest the toolchain and license notices.
