# minidriver attachment and system WebKit input patches

This directory retains the small downstream patches used by ExplorerKit's
experimental macOS automation adapter. The host bundles and launches the helper;
ExplorerKit connects an owned socket to the system WebKit views. Cargo does not
apply these patches or build a WebKit engine.

## Source and purpose

The base is WebKit revision
[`db2d79f4413193cbfa65b50ebddcec39eaba5809`](https://github.com/WebKit/WebKit/commit/db2d79f4413193cbfa65b50ebddcec39eaba5809).
`source.json` records that pin, ordered patch hashes and the tested helper hash.
Apply the patches in this order:

1. `0001-attach-existing-views.patch` adds one-use `--attach-fd` support and
   selects an existing native browsing context instead of launching MiniBrowser
   or creating a startup view. Native active-first selection falls back to the
   first eligible context; enumeration order is not a durable view identity.
2. `0002-system-webkit-input.patch` uses native down/up commands for an attached
   backend's click, carries the released mouse button into native actions, and
   releases known held mouse state before native cancellation.

Both files are byte-for-byte copies of the approved experimental patches.
Together they change five WebDriver source files. They do not change an engine,
protocol schema, generated atom or WebSocket implementation. The host uses
standard Classic HTTP and BiDi WebSocket endpoints; it does not implement a new
driver. Known input and BiDi limits belong in the
[capability reference](../../docs/reference/capabilities.md).

## Reproduce the helper

Use a separate WebKit source checkout at the recorded revision and Xcode's
macOS SDK. The tested toolchain is recorded in `source.json`. A full checkout
avoids missing upstream build prerequisites; a sparse checkout also needs the
support generators in `WebKitLibraries`, `Tools/clangd`, `Tools/ccache` and the
source directories used by the three targets below.

The following commands modify that checkout and create build products. They
are a build recipe, not a command run automatically by the SDK. Choose a source
directory and a reusable output directory explicitly; do not point at a checkout
containing unrelated changes. When an agent runs them, the repository's external
change approval rule still applies.

```sh
# Set these to absolute paths chosen for the helper build.
export MINIDRIVER_SOURCE=/path/to/WebKit
export MINIDRIVER_PATCHES=/path/to/ExplorerKit/patches/minidriver
export MINIDRIVER_OUTPUT=/path/to/minidriver-build

git -C "$MINIDRIVER_SOURCE" rev-parse HEAD
# Must equal the revision in source.json; begin with a clean source diff.
git -C "$MINIDRIVER_SOURCE" diff --exit-code
git -C "$MINIDRIVER_SOURCE" apply "$MINIDRIVER_PATCHES/0001-attach-existing-views.patch"
git -C "$MINIDRIVER_SOURCE" apply "$MINIDRIVER_PATCHES/0002-system-webkit-input.patch"

for target in bmalloc WTF minidriver; do
  case "$target" in
    minidriver) project=Source/WebDriver/minidriver.xcodeproj ;;
    *) project="Source/$target/$target.xcodeproj" ;;
  esac
  xcodebuild -project "$MINIDRIVER_SOURCE/$project" -scheme "$target" \
    -configuration Release -sdk macosx \
    -derivedDataPath "$MINIDRIVER_OUTPUT/derived" \
    "SYMROOT=$MINIDRIVER_OUTPUT" "OBJROOT=$MINIDRIVER_OUTPUT/obj" \
    CODE_SIGNING_ALLOWED=NO ARCHS=arm64 ONLY_ACTIVE_ARCH=YES -jobs 2 build || exit 1
done

"$MINIDRIVER_OUTPUT/Release/minidriver" --help
otool -L "$MINIDRIVER_OUTPUT/Release/minidriver"
otool -l "$MINIDRIVER_OUTPUT/Release/minidriver"
shasum -a 256 "$MINIDRIVER_OUTPUT/Release/minidriver"
```

These are the three upstream targets used for the retained helper. WTF and
bmalloc are static support archives. The tested executable links system
frameworks/libraries, not a custom WebKit framework. Do not add DYLD overrides
or bundle a replacement engine for attachment mode. The source pin identifies
the driver, not the exact source revision of Apple's installed WebKit.

The recorded binary hash identifies the tested unsigned artifact. Signing a
copy changes its bytes; record that copy's hash and signing identity separately.
Inspect the Mach-O minimum OS as well as linked libraries; the validated helper's
deployment requirement is listed in [capabilities](../../docs/reference/capabilities.md#native-automation).
An embedder packages, signs and reaps its own helper. Signed distribution and
release promotion are separate from retaining these patches.

## License and removal

The changed upstream files retain their copyright notices and redistribution
terms; their original notices are copied in `UPSTREAM-NOTICES.txt`. They are available at the pinned revision in
[Source/WebDriver](https://github.com/WebKit/WebKit/tree/db2d79f4413193cbfa65b50ebddcec39eaba5809/Source/WebDriver).
WebKit contains code under several licenses; see its
[license information](https://webkit.org/licensing-webkit/).
When distributing a helper, include the applicable notices for its WebDriver,
WTF and bmalloc code, including third-party code compiled into those targets.
This patch directory is not a complete notice bundle for a binary distribution.

Remove each patch when the selected upstream driver supplies the corresponding
attachment/input behavior and the native automation regression passes against
system WebKit without that patch. Updating the pin or patches requires rerunning
the native session, existing-view lifetime, standard input and documented
failure checks described in [testing](../../docs/reference/testing.md).
