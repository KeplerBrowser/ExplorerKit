# Dependencies and the Servo version

ExplorerKit builds against a published Servo release from crates.io. This page
explains which version that is, where versions are pinned, which local patches
exist, and how to update them safely.

## Servo version

ExplorerKit uses **`servo = "=0.6.0"`** from crates.io, with the matching
`servo-default-resources` and `servo-allocator` versions.

- ExplorerKit does not build against the `upstream/servo` submodule or a Git
  branch of Servo.
- Servo 0.6 makes WebGL, WebCrypto, and jemalloc opt-in. ExplorerKit turns all
  three back on (`webgl` and `webcrypto` features on `servo`, and
  `servo-allocator/use-jemalloc`). Upstream Servo still uses the system
  allocator on Windows.
- Servo is only compiled when a crate's `servo` feature is on. The portable
  controller used on iOS never includes Servo.
- Direct rendering dependencies match Servo's: surfman 0.13 and WebRender 0.70.

Changing the Servo version is a deliberate decision, not a routine update. See
[Updating Servo](#updating-servo).

## Cargo roots and lockfiles

There are three separate Cargo roots, each with its own checked-in lockfile.
There is no `Cargo.toml` at the repository root.

| Cargo root | Lockfile | What it builds |
| --- | --- | --- |
| `crates/Cargo.toml` | `crates/Cargo.lock` | All ExplorerKit crates, including the Android host |
| `examples/desktop-winit/Cargo.toml` | `examples/desktop-winit/Cargo.lock` | The `winit` desktop example |
| `examples/desktop-gpui/Cargo.toml` | `examples/desktop-gpui/Cargo.lock` | The GPUI desktop example |

They are separate because Cargo applies `[patch.crates-io]` only from the root
being built. Keeping roots separate means each patch affects only the build
that needs it.

Shared version pins for the ExplorerKit crates live once in
`[workspace.dependencies]` in `crates/Cargo.toml`. Each example manifest pins
its own app dependencies, such as `winit` or `gpui`.

Always build with `--locked` so Cargo uses the checked-in lockfile.

## Local patches

| Patch | Applied in | Why |
| --- | --- | --- |
| `tikv-jemalloc-sys` (`crates/vendor/tikv-jemalloc-sys`) | `crates/Cargo.toml`, both desktop examples | Removes an obsolete Android `libgcc` link so Servo links with modern NDKs |
| `stylo_derive` 0.21.0 (`patches/stylo_derive-0.21.0`) | GPUI example only | GPUI's logging stack makes Stylo's generated `ToCss` error conversion ambiguous. The patch replaces the ambiguous `?` with explicit error returns. |
| `zed-font-kit` 0.14.1-zed (`patches/zed-font-kit-0.14.1-zed`) | GPUI example only | GPUI 0.2.2 needs FreeType 0.20, which conflicts with Servo's FreeType 0.23. The patch applies an upstream manifest-only fix. |
| `minidriver` (`patches/minidriver`) | Host-bundled macOS automation helper, outside Cargo | Attaches to existing system WebKit views and adapts native mouse input for the tested system backend. |

Each patch folder has a `PATCH.md` with its source, license, and removal
criteria. Remove a patch as soon as the upstream graph no longer needs it.

If your own app combines GPUI with ExplorerKit, add both GPUI patches to your
own Cargo root. Cargo does not inherit patches from dependencies.

The minidriver directory retains the exact driver pin, two ordered patches,
their hashes, upstream notices and a [helper build recipe](../../patches/minidriver/PATCH.md).
It does not contain or build a replacement WebKit engine. The host packages
and launches the helper; system WebKit stays supplied by macOS. Rebuilding or
publishing the helper is separate from compiling ExplorerKit's Rust feature.

## Updating a dependency

To refresh a lockfile on purpose:

```sh
cargo generate-lockfile --manifest-path crates/Cargo.toml
cargo generate-lockfile --manifest-path examples/desktop-winit/Cargo.toml
cargo generate-lockfile --manifest-path examples/desktop-gpui/Cargo.toml
```

To move one crate to an exact version, edit the manifest, then run
`cargo update` against the root that owns it:

```sh
cargo update --manifest-path crates/Cargo.toml -p rustls --precise 0.23.40
cargo update --manifest-path examples/desktop-gpui/Cargo.toml -p gpui --precise 0.2.2
cargo update --manifest-path examples/desktop-winit/Cargo.toml -p winit --precise 0.30.13
```

If a lockfile contains two versions of the same crate, name the one you want
with `name@version`. If several roots share a dependency, update all of them
together.

## Updating Servo

Do all of these in one change:

1. Update the `servo`, `servo-default-resources`, and `servo-allocator` pins in
   `crates/Cargo.toml`, plus any direct Servo pins in examples and
   dev-dependencies.
2. Refresh `crates/Cargo.lock` and the lockfiles of both desktop examples,
   plus any other lockfile whose graph changed.
3. Rerun the `--locked` checks in [Testing and validation](testing.md).
4. Write down any Servo API changes ExplorerKit had to absorb.

## Licenses

ExplorerKit's own code is under the [MIT license](../../LICENSE). Every
dependency keeps its own license. The main ones:

| Project | License | Used for |
| --- | --- | --- |
| [Servo](https://servo.org) | MPL-2.0 | The web engine |
| [WRY](https://github.com/tauri-apps/wry) | MIT or Apache-2.0 | System WebKit views on macOS |
| [surfman](https://github.com/servo/surfman) | MIT, Apache-2.0, or MPL-2.0 | GPU surfaces |
| [raw-window-handle](https://github.com/rust-windowing/raw-window-handle) | MIT, Apache-2.0, or Zlib | Native window handles |

`crates/Cargo.lock` and `bun.lock` list every dependency.

The retained third-party sources and patches keep their original license
files or source notices:

| Folder | License |
| --- | --- |
| `crates/vendor/tikv-jemalloc-sys` | MIT or Apache-2.0; the bundled jemalloc source is BSD 2-clause (`jemalloc/COPYING`) |
| `patches/stylo_derive-0.21.0` | MPL-2.0. Changed files stay under MPL-2.0. |
| `patches/zed-font-kit-0.14.1-zed` | MIT or Apache-2.0 |
| `patches/minidriver` | Changed WebDriver files retain their upstream notices. The helper also links WTF and bmalloc; its binary distribution needs the applicable notices for all compiled code. |

The Android AAR, the iOS XCFramework, and the macOS framework contain
compiled third-party code, including Servo. Before publishing any of them,
ship the license notices of everything inside, and say where the source of
MPL-2.0 code such as Servo can be found. The macOS build already writes an
SPDX software bill of materials (`distribution/macos/create-sbom.mjs`).

ExplorerKit is an independent project. It is not made, endorsed, or supported by
the Servo project or Linux Foundation Europe, which hosts Servo. When you
mention Servo, describe what ExplorerKit does with it ("embeds Servo", "for
Servo"), and don't use Servo's logo.

## The `upstream/servo` submodule

`upstream/servo` is a pinned Git submodule of the Servo repository. It is a
reference copy for reading and comparing code. It is not used to build
ExplorerKit.

It is useful when you change native runtime behavior and want to compare with
upstream. Good places to start:

1. `ports/servoshell`: Servo's own browser shell, for embedding, lifecycle, and
   prompt handling.
2. `support/android/apk/servoview` and `support/android/apk/servoapp`: Servo's
   Android host and demo app.
3. `components/shared/embedder/embedder_controls.rs`: the requests Servo sends
   to embedders, such as pickers and context menus.
4. `components/servo/webview_delegate.rs`: the full `WebViewDelegate` surface.
5. The published [`servo` crate docs](https://docs.rs/servo/latest/servo/).

```sh
git submodule update --init --recursive upstream/servo
git submodule status
```

You only need the submodule when working on native runtime behavior. Keeping
it pinned means reviews and docs can point at one known upstream snapshot.
When you move to a newer snapshot, update the submodule and any docs that
compare against it.

ExplorerKit reuses upstream patterns for low-level wiring: surface ownership,
render loops and wakeups, baseline IME behavior, and JNI lifecycle. It keeps
its own implementation of higher-level prompts, such as file pickers, context
menus, permissions, dialogs, and app-facing policy decisions.
