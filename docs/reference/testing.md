# Testing and validation

Use this page to pick the smallest check that proves your change works. Checks
are grouped by area. Each area lists fast automatic checks first, then manual
checks that need a device, simulator, or desktop session.

When you run a manual check, write down what you ran in the pull request: the
platform, the device or OS version, and the test page URL. A manual check that
was not run should be reported as "not run", not skipped silently.

## Quick checks

These run in a few minutes on any OS and do not compile Servo.

| Area | Command |
| --- | --- |
| Servo-free Rust crates | `cargo test --manifest-path crates/Cargo.toml -p explorerkit-embedder -p explorerkit-host -p servokit-controller-ffi -p explorerkit --locked` |
| React Native types | `bun run --cwd packages/react-native-explorerkit typecheck` |
| React Native package build | `bun run --cwd packages/react-native-explorerkit prepare` |
| React Native unit tests | `bun test packages/react-native-explorerkit/src/__tests__` |
| Commit messages | `bun run commitlint --from <base> --to HEAD` |
| Docs | Check changed links, then `git diff --check` |

> [!NOTE]
> Two of the React Native unit tests exercise macOS build scripts. They fail
> on Linux and Windows.

## macOS WebKit automation

The durable native test exercises the public Rust API and standard driver
commands together. It owns the adapter's existing-view lifetime, session-local
mapping, native creation/error retirement, shared dialog and teardown guarantees.
Rerun it when the macOS adapter, WRY, installed WebKit, minidriver pin/patches, or
view/prompt ownership changes. It needs an interactive macOS desktop, Bun and a
compatible local helper; it never downloads or builds that helper.

Reuse the app's existing Cargo target across worktrees. For the desktop example:

```sh
export CARGO_TARGET_DIR="$PWD/examples/desktop-winit/target"
cargo test --manifest-path crates/Cargo.toml -p explorerkit-embedder --locked
cargo test --manifest-path crates/Cargo.toml -p explorerkit \
  --no-default-features --features macos-webkit-automation --lib --locked -- --test-threads=1
EXPLORERKIT_MINIDRIVER=/absolute/path/to/minidriver \
  cargo test --manifest-path crates/Cargo.toml -p explorerkit \
  --no-default-features --features macos-webkit-automation \
  --test macos-webkit-automation --locked
```

Without `EXPLORERKIT_MINIDRIVER`, the native test reports a skip; a successful
Cargo exit alone is not native acceptance evidence. Set
`EXPLORERKIT_AUTOMATION_EVIDENCE` to a fresh output directory to retain the client
result and protocol transcript. The harness runs both Runtime drop and explicit
shutdown, retaining their evidence in separate subdirectories. Repeat with `EXPLORERKIT_SELECTION_HOOK=1` when
changing the optional presentation callback.

`EXPLORERKIT_RETIREMENT_PROOF=1` selects the focused back-to-back creation
retirement proof plus pending-work teardown. It uses the same batch assertions
as the full client and records its mode in the result. It does not replace full
native acceptance; keep unrelated full-suite failures visible when using it.

The client compares standard window handles and BiDi trees with live SDK views,
checks document state through standard script execution, and verifies helper
reaping. Terminal process-loss callback tests exercise captured native blocks;
they do not kill a real WebContent process. Known native protocol defects remain
listed in [capabilities](capabilities.md#native-automation).

Changes to helper packaging also need a signed-copy launch, inherited-FD
handshake and teardown check against system frameworks. Ad-hoc signing proves
local launch only; it does not prove notarization or release distribution.

## Rust with Servo

These compile Servo, so run them on a prepared machine with Servo's build
dependencies installed.

```sh
# Facade behavior with the Servo backend
cargo test --manifest-path crates/Cargo.toml -p explorerkit --features servo --locked

# The full workspace, when you have the time and disk space
cargo test --manifest-path crates/Cargo.toml --workspace --locked

# One real-Servo test that covers shared engine ownership, reuse with zero
# views, popups, and final shutdown in one process
cargo test --locked --manifest-path crates/Cargo.toml -p explorerkit-embedder \
  --features servo popup_and_process_runtime_lifetimes_share_one_real_servo_proof \
  -- --test-threads=1
```

Servo can only start once per process, so real-Servo tests cannot use a fresh
engine per test. That is why the last test runs on one thread and checks
several lifetimes in sequence.

When Servo crash callbacks, evaluation correlation or event routing change, run
the conditional crash-settlement regression in `explorerkit-embedder`:

```sh
cargo test --manifest-path crates/Cargo.toml -p explorerkit-embedder \
  --features servo observed_crash_settles_only_pending_reads_on_its_registered_view --locked
```

This checks existing crash/result ordering, finalized outcomes, late callbacks,
reused IDs, sibling and registered-child isolation, destruction and native close
through Runtime's event queue. It invokes the callback translation directly;
it does not induce a native crash or prove renderer survival or recovery.

### Native Stop and current loading

When command routing, native readiness/lifetime or the relevant dependencies
change, rerun the Stop/query regressions in the portable crates, macOS facade,
Android host and desktop boundary. They cover explicit unsupported errors before
deferred queues, preserved pending loads and lifetimes, portable rejection, and
current state independent of queued completion events.

```sh
cargo test --manifest-path crates/Cargo.toml -p explorerkit-embedder -p servokit-controller-ffi --locked
cargo test --manifest-path crates/Cargo.toml -p explorerkit --features macos-system-webview --lib --locked -- --test-threads=1
cargo test --manifest-path crates/Cargo.toml -p explorerkit-host-android -p explorerkit-host-desktop --lib --locked
```

Native acceptance also uses a temporary local HTTP fixture with held headers,
a redirect destination and a streamed committed page. Through public Runtime,
observe loading before Stop and false while the response remains held, retained
partial content/rendered pixels, native identity/history, subsequent navigation,
Stop followed by a new load or same-URL reload, detached views, original
children and sibling isolation. Do not infer cancellation completion from the
method return or synthesize a load-complete event. Record the fixture revision,
commands, timings, results and limits in the PR; remove the one-time machinery.
This does not prove per-navigation outcome attribution or all-network quiescence.

### Desktop C boundary (macOS and Windows)

```sh
cargo test --manifest-path crates/Cargo.toml -p explorerkit-host-desktop --test c_boundary --locked
```

This builds the static library, then compiles the private C header as C++17,
links it, and runs a C++ test program in normal and `-DNDEBUG -O2` builds. It
checks all nine exported functions: creating hosts and tokens, rejecting a
null native handle, exact command byte handling (including bad UTF-8, bad
JSON, and oversized input), wrong tokens, repeated detach, event draining, and
destroy. It needs a C++17 compiler. It uses an invalid native handle on
purpose, so it does not prove that real attachment works. That needs a manual
check with a real AppKit view or Win32 window.

## Test pages

Most manual checks use the shared pages in
[`examples/fixtures`](../../examples/fixtures/README.md). Serve them from the
repository root:

```sh
python3 -m http.server 8481 --directory examples/fixtures
```

Then load `http://127.0.0.1:8481/smoke/index.html`. The page shows **Smoke
fixtures ready** when it loads. The Kotlin examples and the package test app's
Android build bundle the pages and serve them on the same port inside the app;
`examples/react-native-app` doesn't.

A page that loads only proves that page loads. It does not prove that
ExplorerKit fully supports the feature the page tests. Support claims live in
[What works where](capabilities.md).

Every example should be able to show the same basic behavior: render a page,
load an initial URL, load a new URL, reload, go back and forward, focus and
blur, survive surface attach, resize, and detach, keep rendering from the
app's own event loop, and report URL, title, load, history, focus, and error
events.

## Desktop

### macOS

#### Explicit site-data transfer

Rerun these checks when the portable operation lifecycle, WebKit store or
helper, Servo cookie or storage API, or destination navigation gate changes.
The native tests use an interactive AppKit session, loopback servers, and
fresh synthetic stores.

```sh
cargo test --manifest-path crates/Cargo.toml --locked -p explorerkit-embedder
cargo check --manifest-path crates/Cargo.toml --locked -p explorerkit --features servo
cargo test --manifest-path crates/Cargo.toml --locked -p explorerkit \
  --features macos-webkit --test macos-site-data
cargo test --manifest-path crates/Cargo.toml --locked -p explorerkit \
  --features macos-system-webview --test macos-site-data
cargo test --manifest-path crates/Cargo.toml --locked -p explorerkit \
  --features macos-webkit-automation --test macos-site-data
```

The WebKit-only fixture checks store and origin isolation, cookie opt-in,
read-only capture, cancellation, and teardown. The mixed fixture exercises
the first Servo request and script, shared-store merge, HTTP cookie scope,
preflight, and the terminal destination gate. The network fixture uses HTTP.
Servo's native HTTPS cookie getter verifies the transferred Secure metadata.
The automation-enabled run checks the selected store for the first
automation-enabled view using the same fixture. It requires no WebDriver
helper.

#### Engine configurations

For changes to macOS engine feature flags, host ownership or shared frame
types, check each [build configuration](macos-system-webview.md#set-up)
separately. An all-features run cannot prove either single-engine build:

```sh
for features in servo macos-webkit servo,macos-webkit macos-system-webview; do
  cargo test --manifest-path crates/Cargo.toml -p explorerkit --locked \
    --no-default-features --features "$features" --lib -- --test-threads=1
done
```

The facade's build-selection tests check the exact `compiled_engines()` list,
the unchanged Servo default, unavailable-engine errors, and sibling/registry
cleanup after failed creation. The WebKit-only run also retains the system
callback, evaluation and lifecycle regressions. Run the portable quick checks
above to preserve the no-engine configuration.

For native acceptance, compile the same app-owned `MacOsViewHost` consumer
source in the three engine configurations. Keep its constructor, delegate,
`SurfaceFrame` and GPU resource signatures unchanged. Inspect each actual
consumer's normal/build dependency graph: WebKit-only must exclude the Servo
engine/rendering dependency chain, and Servo-only must exclude the WebKit
integration chain. Verify the legacy flag enables both without explicitly
enabling the two individual flags alongside it.

Exercise real native placement, loading, evaluation, input, resize,
detach/reattach, original popup children, sibling isolation and cleanup in
each applicable configuration. Include WebKit store reopen and Servo GPU
frame/completion evidence. Check staged and closed WebKit children are released
on updates even when no live views remain, before the app's parent-view
delegate drops. Record the native evidence and limitations with the change;
placeholder-host tests do not establish browser behavior. Use the existing
application Cargo target and one native build stream.

Automatic checks:

```sh
cargo check --manifest-path examples/desktop-winit/Cargo.toml --locked

RUSTC_WRAPPER=sccache CARGO_TARGET_DIR=/tmp/explorerkit-gpui-target CARGO_PROFILE_DEV_DEBUG=0 \
  cargo check --manifest-path examples/desktop-gpui/Cargo.toml --locked
```

Manual check, whole window (`winit`):

```sh
cargo run --locked --manifest-path examples/desktop-winit/Cargo.toml -- \
  --smoke http://127.0.0.1:8481/smoke/index.html
```

A window shows **Smoke fixtures ready**, the page stays aligned when you
resize the window, and the terminal prints surface attach, resize, and detach
events, URL, load, and focus events, and `smoke result=pass`. Smoke mode exits
with code 0 after the page finishes loading, or 1 on timeout, error, crash, or
early window close. The default timeout is 30 seconds; change it with
`--smoke-timeout-ms <ms>`.

Manual check, part of a layout (GPUI):

```sh
RUSTC_WRAPPER=sccache CARGO_TARGET_DIR=/tmp/explorerkit-gpui-target CARGO_PROFILE_DEV_DEBUG=0 \
  cargo run --locked --manifest-path examples/desktop-gpui/Cargo.toml -- \
  http://127.0.0.1:8481/smoke/index.html
```

GPUI's own UI surrounds the Servo area, the page shows **Smoke fixtures
ready**, the Servo view stays aligned with its slot when you resize, and the
footer shows URL, load, title, and the latest event.

### Multiple views on macOS

```sh
RUSTC_WRAPPER=sccache CARGO_PROFILE_DEV_DEBUG=0 FREETYPE2_NO_PKG_CONFIG=1 \
cargo run --locked --manifest-path crates/Cargo.toml -p explorerkit \
  --features servo --example multiple-native-views -- --smoke
```

This creates two independent pages in separate native views. It checks
JavaScript, input, navigation, history, and resizing in each one, closes
either view, creates replacements, and creates a page after a period with no
views. It prints `multiple-native-views result=pass` after final engine
shutdown. It uses data URLs, so it needs no test page server. Leave out
`--smoke` to keep both pages open. It does not cover clipping, overlap, hiding
and showing views, or GPU frame export.

### Final shutdown on macOS

The [GPUI shutdown check](../../examples/desktop-gpui/README.md#shutdown-check)
covers cleanup when a window closes or the app quits, with both orders of
logger setup, and with shutdown through the original host or a fresh
replacement host. Every combination must exit successfully.

That check does not cover SharedWorker shutdown stalls. When a host changes
its [final process-exit policy](../concepts/surfaces.md#final-process-exit-when-servo-shutdown-stalls),
check a no-worker normal exit and a SharedWorker deadline exit with the actual
terminal primitive. Reopen each disposable profile in a new process and check
one persistent cookie and one app-owned saved marker. Record the revision,
commands, deadline observations and limits in the PR; keep one-time proof tools
out of production source. A consumer app must also check its own update and
relaunch ordering.

### GPU frame export on macOS

For `OffscreenSurface::exportable()`, check by hand that you can: consume
frames from two live pages, confirm 32BGRA orientation and exact pixel size,
use up all three pool slots and release them, resize across scale changes, and
close either view while the other keeps responding. This needs an interactive
macOS session.

### System web view on macOS

Rerun these when you change `MacOsViewHost`, system JavaScript evaluation,
or system favicon decoding, history retention, callback scheduling, child callbacks
and native lifetimes.

Automatic regressions (macOS):

```sh
cargo test --manifest-path crates/Cargo.toml -p explorerkit --features macos-system-webview --lib --locked
cargo test --manifest-path crates/Cargo.toml -p explorerkit --features macos-system-webview --test macos-system-configuration --locked
```

The configuration regression runs on the process main thread with an ephemeral
store. It checks that configurations share only their native pool and store,
while configurations, preferences and content controllers stay independent.

These cover callback and runtime queue ordering, retiring pending requests on a
same-URL commit, evaluation ID reuse, closing and replacing sibling views, and
both orderings of process-termination and evaluation errors. Native-result wake
regressions check publication before notification, released callback borrows,
coalesced deferred drains, independent host wakers and stale-result suppression.
Favicon regressions
also check decoded first-frame pixels, malformed and oversized data, missing
private-selector handling, per-view retirement at commit, process termination
and destruction, retained history identity and reload invalidation, shared
same-document data, byte and entry limits, eviction and orphan cleanup. Native
membership regressions distinguish new silent aliases from preexisting evicted
entries, prune during guarded navigation before retention, and keep native
membership independent of the image-record budget. Cached-state regressions
cover initial values, burst coalescing, result-before-state ordering and stale
generations without a native view.

Manual check in an interactive AppKit app:

For callback scheduling changes, also use an otherwise idle native host whose
waker schedules owning-thread updates and event drains. Check delayed title,
load, evaluation and favicon results, original child creation and native close,
source/child independence, and a separate native runtime's waker. Do not supply
periodic SDK updates to rescue a missing notification. Also cover delayed URL
changes, silent same-URL aliases followed by Back to current and older aliases,
reload, image eviction and foreign JavaScript traversal without refetch rescue,
Stop/failure/replacement/redirect, detach/reattach, construction failure and
same-handle native replacement. Verify initial observations before SDK root
navigation and before returning original children, one registration per native
lifetime, removal before native release, stale-generation suppression and child
survival after source destruction. Explicit idle collections must perform no
native state/history reads. Audit new native symbols and build/link against the
packaged macOS 14.0 deployment target. Record the actual runtime OS; a minimum
deployment-target build is not an older-OS runtime test. This check preserves
Servo animation pacing and does not remove the host's own AppKit/input work.

1. In one app-owned slot, create and destroy both `Servo` and `SystemWebView`
   views while a sibling Servo view keeps responding.
2. The system view must resize with its slot; accept native pointer, scroll,
   focus, text, and IME composition input; load, reload, go back and forward;
   report URL, load, title, crash, and navigation-state events; reject forwarded
   `HostInputEvent`s; and deny new windows by default and downloads.
3. Recreate the view and restart the app with the same
   `WebKitDataStoreIdentifier`. Cookies, local storage, and IndexedDB must
   persist. A different identifier must give isolated storage.
4. Load `https://www.youtube.com/watch?v=aqz-KE-bpKQ`. On macOS 26.5.1, Servo
   shows YouTube's "Your browser can't play this video" message at `0:00`,
   while the system view plays the video.
5. For JavaScript evaluation, use a real app with WRY-owned views. Cover
   calls before the first commit, during loading, on cookie-gated and changing
   pages, with two views at once, on hidden views, and across navigation,
   reload, history traversal, failed navigation, and single-page-app changes.
   A standalone `WKWebView` demo or a successful compile is not enough.
6. For favicon changes, use two WRY-owned runtime views with distinct declared
   PNG/ICO icons, including relative and cross-origin URLs. Inspect the owning
   view handle, pixel channels, row order and semitransparent pixels. Exercise
   reload, failed navigation, icon-free destinations, delayed loads,
   detach/reattach and destruction while a load is pending. For Back/Forward,
   verify exact retained pixels with no new icon requests, both retention limits,
   eviction fallback, oversized initial-only images, duplicate URLs, same-document
   history, stale completions and pruning. Confirm `Started` precedes the commit
   favicon event and missing data reports `None`. Confirm cookie-gated
   icons use the view's WebKit data store and a separate store stays isolated.
   Use truthful 16/32-pixel declarations and vary success/failure completion
   order. Check equal-size DOM ties, native media matching, declared MIME hints,
   duplicate declarations, unavailable and unmatched metadata, and that a
   failed candidate cannot suppress a valid one. Confirm the private metadata
   query settles held native decisions once without delaying document completion
   indefinitely or emitting app evaluation results. Exercise metadata/data
   retirement at commit, process/view invalidation and close.
   For SVG, check public format qualification, native inferred/fractional sizes,
   exact straight-alpha pixels, native object/byte lifetime, original and
   normalized encoded limits, output pixels and normalized history retention.
   Include external-resource/profile controls and record synchronous conversion
   costs without calling measurements worst-case bounds. Post-load DOM or theme
   changes are outside this native capability; see
   [favicon limits](capabilities.md#favicons).

7. For accepted children, install an attached AppKit destination provider and
   drive creation through the public Runtime. Verify default and explicit
   denial, destination rollback, original blank and fresh POST children, named
   reuse, nested sources, `noopener`, redirects, cookies, cross-origin access
   restrictions and opener messaging. Exercise ordinary child controls,
   detach/reattach, a request from a detached source, and independent source
   and child close. Check that native close reports the correct handle once,
   rejects later commands, and releases the last container on an empty Runtime
   update. Confirm UI-selector availability still matches WRY's file/media
   callbacks and that root and child favicons still arrive on their own handles.

Record the final URL and title, the input results, the event order, and the
exact revision and OS in the pull request. This evidence covers macOS only.

### Profile copy proof

```sh
cargo run --locked --manifest-path examples/desktop-winit/Cargo.toml --bin profile-branch-proof -- prove
```

This proves that a closed Servo profile directory can be copied into two
branches that then diverge independently. It seeds a test cookie and a
`localStorage` value in a visible Servo window, shuts Servo down, snapshots the
profile, copies it twice, runs a visible and a headless Servo process at the
same time, and reopens each copy in a fresh process. A pass prints
`concurrent_authenticated=true`, `fresh_process_reopen=true`, and
`result=PASS`. It needs an interactive desktop session. It is a proof tool,
not a profile API, and does not cover IndexedDB.

### Windows and Linux

Compile check on a prepared machine:

```sh
cargo check --manifest-path examples/desktop-winit/Cargo.toml --locked
```

Manual check: serve the test pages, run `examples/desktop-winit`, and confirm
that the page renders and that resize, focus, URL, load, and close events
appear in the terminal. Windows has no framework adapter or shared GPU texture
export yet. On Linux, ExplorerKit has not yet picked a baseline X11 or Wayland
setup, and dma-buf export does not exist.

## Android

These need the Android setup from [Get started](../getting-started.md#android-react-native-or-kotlin).

| Check | Command |
| --- | --- |
| Rust host crate | `cargo test --manifest-path crates/Cargo.toml -p explorerkit-host-android --locked` |
| Build and verify the AAR, then copy it into the package | `examples/react-native-app/android/gradlew -p examples/react-native-app/android :explorerkit-host-android:stageReactNativeExplorerKitReleaseAar` |
| React Native example build | `bun run --cwd examples/react-native-app build:android` |
| React Native adapter unit tests | `bun run --cwd packages/react-native-explorerkit test:native:android` |
| Kotlin browser example build | `examples/react-native-app/android/gradlew -p examples/android-kotlin-browser :app:assembleDebug` |
| Bare Android example build | `examples/react-native-app/android/gradlew -p examples/android-native-example :app:assembleDebug` |

The React Native example and adapter tests use the staged AAR, so build it
first. The Kotlin and bare Android examples build the Rust library directly.

Manual checks on an arm64 device or emulator:

```sh
# React Native: the package's test app, with shortcuts to every test page
# (start Metro first: bun run --cwd packages/react-native-explorerkit/example start)
bun run --cwd packages/react-native-explorerkit example:android

# Kotlin browser example
adb install -r examples/android-kotlin-browser/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.kepler.explorerkit.androidkotlinbrowser/com.kepler.explorerkit.androidexample.MainActivity

# Bare Android example
adb install -r examples/android-native-example/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n com.kepler.explorerkit.androidexample/.MainActivity

# Logs for both Kotlin examples
adb logcat -s ExplorerKitNativeExample NativeExplorerView ExampleFixtureServer
```

The Kotlin examples show a native toolbar, test page shortcuts, the Servo
view, and an event footer. Prompts use Android UI and resolve through the
shared Android host, not React Native. The bare example's README has a
step-by-step script for every prompt type.

## iOS

Release builds of the exact packed package (macOS with Xcode), from
`packages/react-native-explorerkit`:

```sh
node scripts/validate-packed-consumer.mjs --ios-only
```

This packs the package, installs that exact archive in a clean React Native
0.85 app outside the repository, and builds Release for device arm64,
simulator arm64, and simulator x86_64. It does not run the app. Each package
candidate still needs its own manual simulator run.

Manual simulator check with the package's test app, which has shortcuts to
every test page:

```sh
bun run --cwd packages/react-native-explorerkit example:fixtures:ios   # serves the test pages
bun run --cwd packages/react-native-explorerkit/example start          # Metro
(cd packages/react-native-explorerkit/example && bundle install)
bun run --cwd packages/react-native-explorerkit example:pods:ios
bun run --cwd packages/react-native-explorerkit example:ios
```

The pages should render through `WKWebView`, and commands, events, navigation
policy, JavaScript evaluation, and dialogs should work. Picker, permission,
and context-menu pages may load, but iOS does not support those prompts.

End-to-end flows ([Maestro](https://maestro.mobile.dev)) run against the same
test app:

```sh
bun run --cwd packages/react-native-explorerkit test:e2e:ios
```

The public example in `examples/react-native-app` is a plain browser. Use it
for a quick look, not for these checks.

## React Native on macOS (prototype)

```sh
bun run --cwd packages/react-native-explorerkit typecheck
bun run --cwd packages/react-native-explorerkit prepare
bun test packages/react-native-explorerkit/src/__tests__/macos-fabric-source-routing.test.ts

# Needs a prepared macOS Servo, Xcode, and CocoaPods setup and a clean checkout
EXPLORERKIT_BUILD_FROM_SOURCE=1 EXPLORERKIT_SOURCE_DIR="$PWD" \
  bun run --cwd examples/react-native-macos-app pods:macos
```

Then start Metro with `bun run --cwd examples/react-native-macos-app start`
and launch with `bun run --cwd examples/react-native-macos-app macos`. Check
rendering, resize, scroll, pointer input, focus and text input, navigation
commands, view recycling, and events. The universal framework build needs a
lot of temporary disk space. Passing this check does not make the macOS
adapter supported.

> [!NOTE]
> `ExplorerView.tsx` currently throws on any platform other than Android and iOS,
> and React Native macOS reports its platform as `macos`. Confirm the example
> still renders before relying on this check.

## Adding a new check

Add a check to this page only when a real feature and test page exist for it.
A tracked check must name the guarantee it protects, the layer that owns it,
and when it must run again. One-off proof scripts belong in the pull request,
not in the repository. See [CONTRIBUTING.md](../../CONTRIBUTING.md#keep-proof-tools-temporary).
