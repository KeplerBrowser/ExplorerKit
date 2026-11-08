# ExplorerKit docs

ExplorerKit lets you embed Servo in your own app, with the platform's own web
view as a compatibility layer where Servo isn't ready yet. These docs explain
how to try it, how it works, and how to use it on each platform.

New here? Read [Get started](getting-started.md), then
[How ExplorerKit works](../ARCHITECTURE.md).

## Start

| Page | What it covers |
| --- | --- |
| [Get started](getting-started.md) | Build and run an example on iOS, desktop, or Android |
| [How ExplorerKit works](../ARCHITECTURE.md) | The layers, the principles, the engine and its compatibility layer, and how a request moves through the code |

## Platforms

| Page | What it covers |
| --- | --- |
| [React Native](platforms/react-native.md) | `<ExplorerView>` props, events, methods, and platform differences |
| [Android](platforms/android.md) | The Android host, building the AAR, and using it from Kotlin |
| [Desktop (Rust)](platforms/desktop.md) | Embedding Servo in a Rust app with `winit`, GPUI, or AppKit |

## Concepts

| Page | What it covers |
| --- | --- |
| [Runtime and views](concepts/runtime.md) | One engine per process, web views, threads, profiles, popups |
| [Surfaces](concepts/surfaces.md) | Where pages are drawn, attaching and detaching, several views, shutdown |
| [Commands, events, and page prompts](concepts/controller.md) | The command envelope, prompt flow, fallbacks, and status codes |

## Reference

| Page | What it covers |
| --- | --- |
| [What works where](reference/capabilities.md) | Every capability on every platform |
| [Crates and packages](reference/crates.md) | What each crate does and what may depend on what |
| [macOS system web view](reference/macos-system-webview.md) | System WebKit views and their JavaScript evaluation rules |
| [macOS WebKit automation](reference/macos-webkit-automation.md) | Opt-in native WebDriver connection, view selection, creation and dialogs |
| [Testing and validation](reference/testing.md) | The checks for each area, from quick to manual |
| [Dependencies and the Servo version](reference/dependencies.md) | The Servo pin, lockfiles, patches, and updates |

## Project

| Page | What it covers |
| --- | --- |
| [Contributing](../CONTRIBUTING.md) | How ideas become merged changes, without writing code |
| [Agent guide](../AGENTS.md) | The rules AI agents follow in this repository |
| [Docs guide](AGENTS.md) | How to write and maintain these docs |
| [llms.txt](../llms.txt) | An index of these docs for AI tools |

The [GitHub Wiki](https://github.com/KeplerBrowser/ServoKit/wiki) has extra
recipes and troubleshooting. When it disagrees with these docs, these docs
are right.
