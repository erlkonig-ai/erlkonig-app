# Erlkönig for iOS

The foreground replica runs a native TribleSpace store inside the app sandbox. It displays collections, storage and passive connection statistics. The user selects which readable collections to replicate. Rust creates the UIKit scene, native controls and collection views. Xcode links the Rust static library through the process entry point in `app/main.c`.

While Sync is visible, it reads the current peer's immutable health snapshot directly. Rows are ordered by authenticated peer ID. Rates use the host's observation timestamps and require two samples from the same connection. After a reconnect, RTT can appear before the first rate.

## Build the simulator app

Use an Apple silicon Mac, Xcode with the iOS Simulator SDK, Python 3.12 or later, Git and rustup. The native workspace pins Rust 1.98.0. From the repository root, run

```sh
./ios/build-simulator.sh
```

The script verifies the public inputs, builds the arm64 Rust library with the iOS workspace lock and links the simulator app with code signing disabled. Xcode writes `ios/build/xcode/Build/Products/Debug-iphonesimulator/ErlkonigPhone.app`. The deployment target is iOS 17.0.

To run the native tests on the host, prepare the sources and enter the iOS workspace.

```sh
python3 ios/prepare-sources.py
cd ios/native
cargo test --locked --workspace
```

## Native source and licensing

`upstream-lock.json` pins the public archives and checksums for the native source graph. Preparation verifies each archive, applies the corresponding patch, and checks the resulting tree. Later runs recheck the installed trees. The iOS manifest and Cargo lock are independent of the desktop build.

| Source patch | Purpose | Upstream license |
| --- | --- | --- |
| `triblespace-mobile.patch` | Maintains assertion counts during replay, computes storage snapshots from local observations, and publishes passive peer health with traffic and RTT from existing connections. | MIT or Apache-2.0 |
| `netdev-ios.patch` | Enumerates interface addresses and flags through the public iOS SDK. Gateway details and per-address IPv6 flags are unavailable. | MIT |
| `netwatch-ios.patch` | Samples interface state every five seconds while the app is active. Route-only changes and changes during suspension may go unobserved. | MIT or Apache-2.0 |

The upstream MIT notices accompanying these source patches are in `licenses/`. The Xcode asset catalog includes the product icon rendered from the crown geometry in `native/crates/erlkonig-crown`. On macOS, `python3 app/render-icon.py OUTPUT.png` renders it again from the iOS directory.
