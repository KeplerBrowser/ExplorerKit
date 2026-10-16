# Runtime, views, and the engine

This page explains how ExplorerKit runs Servo: one engine per process, a
`Runtime` that owns your web views, and the rules for threads, profiles,
popups, and shutdown.

## The short version

- Servo runs **once per process**, on **one UI thread**. ExplorerKit starts it
  the first time you need it and keeps it alive.
- A **`Runtime`** owns your web views and routes commands and events to the
  right one. Your app owns layout, selection, and presentation.
- A **web view** (`WebViewHandle`) is one browsing context: a page, its
  history, and its pending prompts. It can exist without a surface.
- **Shutdown is final.** Servo cannot restart in the same process.

## One engine per process

The first successful Servo start keeps one engine on a long-lived UI thread.
Servo's handle cannot move between threads, so ExplorerKit never moves the
engine and never restarts it after that thread exits. Web views and surfaces
live shorter lives and come and go independently.

The host keeps its engine connection even when no views exist. Closing views
or dropping a host leaves the engine running for reuse. Only an explicit
`Runtime::shutdown()` stops it. See
[closing views and shutting down](surfaces.md#closing-views-and-shutting-down).

Servo owns its own internals: IPC, networking, storage, and engine
coordination. ExplorerKit does not reach into them.

## Runtime, sessions, and web views

```rust
let mut runtime = Runtime::new(host);
let session = runtime.create_session();
let view = runtime.create_webview(session)?;
```

- The `Runtime` manages which views exist and routes each handle to its view.
- A `SessionHandle` groups views logically. It does **not** separate storage:
  all views in a process share one profile.
- Each Servo view has its own `WebView`, delegate, controller state, pending
  prompts, event queue, and render target.

Your app decides which view is selected, where each one appears, and how they
are presented. The runtime never models tabs or windows.

## Profiles and persistent storage

Native Rust hosts can choose where Servo keeps cookies, logins, HSTS data, and
web storage:

```rust
let options = SurfaceHostOptions::new(waker, clipboard)
    .with_config_directory("/path/to/profile");
```

Set it before Servo first starts. ExplorerKit passes it straight to Servo's
`Opts::config_dir`, so Servo owns the files and their format. The directory is
process-wide. Once the engine is running, it only accepts the same directory.
To switch profiles, start a new process.

## Threads

Call ExplorerKit from the thread that owns the engine, usually your UI thread.
The private desktop C boundary checks this and rejects calls from other
threads. On Android, the host runs updates on the UI thread from
`Choreographer` frames. The one exception is `GpuFrameCompletion`, which you
may send to another thread.

## Web view lifetime on Servo-backed hosts

A web view's identity (its controller) and its surface have separate
lifetimes:

1. A controller can exist before its surface is ready.
2. For app-created views, navigation and commands sent before the first
   surface exists wait in Rust until it is attached. An adopted native child
   already has a running document and accepts commands before attachment.
3. Detaching a surface removes the render target, but does not invalidate the
   controller.
4. Disposing the host invalidates the controller. Later commands fail with an
   explicit error instead of acting on a recycled object.

This is why Android and React Native views can be recycled or resized without
losing the page.

## Popups and new windows

When a page needs a new browsing context, Servo asks the embedder through
`WebViewDelegate::request_create_new`. Creation is denied unless the source
view has a handler installed with `Runtime::set_new_webview_handler`:

```rust
runtime.set_new_webview_handler(view, Some(std::rc::Rc::new(|_request| {
    // This example accepts every request from this view.
    NewWebViewDecision::Create
})))?;
```

The handler returns `NewWebViewDecision::Deny` or `Create` synchronously on
the owning thread. It receives a `NewWebViewRequest` with the actual source
handle, a unique opaque request ID, and the source URL when known. Servo does
not supply the destination URL or window features; those fields remain
`None`. The handler cannot infer a user click or destination from this
metadata. Do not call back into the Runtime or wait for UI inside it.

On `Create`, ExplorerKit retains the original native child and registers an
ordinary `WebViewHandle` in the source's session. It preserves Servo's native
POST, `about:blank`, opener, named-window, and `noopener` behavior. It does not
recreate the page by loading its URL.

| Receipt | Meaning |
| --- | --- |
| `PopupRequested` | Creation was denied; this is information, not a pending question. |
| `PopupCreated` | The child is registered. `ExplorerKitEvent::created_webview` contains its handle; `webview` identifies the immediate source. |
| `PopupCreationFailed` | The handler accepted, but preparation failed before native creation. |

Each receipt carries the attempt's `request_id`. The creation receipt remains
available even if its source closes before you drain events. Subsequent
events use the child's own handle, with no managed-child ID in the envelope.
Use that handle for the existing navigation, evaluation, input, prompt, and
surface methods. Answers to prompts must target the view that asked. See
[evaluation lifetime](controller.md#javascript-evaluation-results) for result
identity and navigation behavior.

The child can execute before your app attaches a surface. Your app chooses
its layout slot later; see [attaching and detaching](surfaces.md#attaching-and-detaching).
Source and child have independent lifetimes. Closing either leaves the
other's document, history, and pending work intact. Source lineage records
IDs only and does not keep its document alive.

A child initially copies its source's handler. Changing or removing a
handler affects only the addressed view, including for nested requests.
`None` denies future requests. `set_popup_policy(DefaultDeny)` also clears the
handler; `ManagedChild` is rejected on this registered route. Standalone
lower-level embedders retain their legacy managed-child policy. Existing
managed-child surface methods remain aliases for registered children and
resolve even after the originating source closes; new callers use the child
handle directly.

See [platform support](../reference/capabilities.md#popups-and-new-windows)
before enabling this handler. Unsupported hosts return an error.
Navigation allow/deny for normal links is a separate request
(`request_navigation`), and `load_web_resource` is HTTP(S) resource
interception.

## Native macOS: Servo or system WebKit per view

On macOS, a native Rust host can use `MacOsViewHost` and pick an engine for
each view when it creates it: `Servo`, or `SystemWebView` (a `WKWebView` owned
by [WRY](https://github.com/tauri-apps/wry)). The runtime still owns every
view's lifetime, and your app still owns the window, layout, and selection.
Servo and WebKit do not share cookies, storage, or credentials. Details:
[macOS system web view](../reference/macos-system-webview.md).

## Related

- [Surfaces](surfaces.md): drawing, attaching, multiple views, and shutdown.
- [Commands, events, and page prompts](controller.md): how apps control views.
