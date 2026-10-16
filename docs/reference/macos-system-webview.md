# macOS system web view

On macOS, a native Rust app can choose, per view, whether a page runs in
**Servo** or in the **system WebKit** (`WKWebView`). This is useful as a
fallback for sites Servo can't handle yet. This page covers how to create
system views, what they support, and the exact rules for evaluating JavaScript
in them.

It applies to native Rust apps only. The React Native macOS adapter always
uses Servo, and Windows has no equivalent.

## Set up

Turn on the `macos-system-webview` feature of the `explorerkit` crate (it also
turns on `servo`), then use `explorerkit::webview::macos::MacOsViewHost` as your
host:

```rust
use explorerkit::webview::macos::{
    MacOsViewHost, MacOsViewHostOptions, MacOsWebViewKind, MacOsWebViewOptions,
    WebKitDataStoreIdentifier,
};

let host = MacOsViewHost::new(
    surfaces, // implements SurfaceDelegate and AppKitViewDelegate
    MacOsViewHostOptions::new(surface_host_options, WebKitDataStoreIdentifier(store_id)),
);
let mut runtime = Runtime::new(host);
let session = runtime.create_session();

let servo_view = runtime.create_webview_with_options(
    session,
    MacOsWebViewOptions::new(MacOsWebViewKind::Servo),
)?;
let system_view = runtime.create_webview_with_options(
    session,
    MacOsWebViewOptions::new(MacOsWebViewKind::SystemWebView),
)?;
```

Your delegate's `appkit_parent` returns the live AppKit parent view for a
slot. ExplorerKit creates the system view inside it and never takes ownership of
your window.

## How system views behave

- **Ownership.** A system view is a child `WKWebView` owned by
  [WRY](https://github.com/tauri-apps/wry). The `Runtime` still owns every
  view's lifetime. Your app still owns the window, the layout slot, the
  chrome, and which engine to pick.
- **Commands and events.** Load, reload, back, forward, and focus work. The
  view reports URL, load status, title, crash, and navigation-state events
  (whether it can go back or forward).
- **Input.** AppKit delivers input straight to the view. ExplorerKit rejects
  `HostInputEvent`s forwarded to a system view.
- **Defaults.** WebKit applies its own defaults for normal navigation and
  permissions. New windows and downloads are denied.
- **Storage.** System views use a WebKit data store named by your
  `WebKitDataStoreIdentifier`. Use the same identifier to keep cookies, local
  storage, and IndexedDB across launches; use a different one to isolate
  them. Servo and WebKit never share storage, logins, autofill, passkeys, or
  credentials.
- **Detach and reattach.** WRY is parented to an ExplorerKit-owned container
  view. Detaching removes the container from your view but keeps the browser
  alive. Reattaching can move the same container under a different live
  parent. Destroying a view, or dropping the host normally, removes system
  views before your parent view's services are released.

## Favicons

WebKit supplies standard favicon candidates and loads their image data in the
view's own browser context. ExplorerKit decodes the first image into owned
straight RGBA8 pixels and sends `HostEvent::FaviconChanged` through the existing
runtime event stream. Match the event's view handle to the view whose icon you
are displaying. Touch icons are ignored; ExplorerKit adds no downloader,
resizing, animation playback, or app icon cache.

The adapter uses WebKit's private icon-loading delegate alongside WRY's own
navigation and UI delegates. If the private selector is unavailable, the view
continues working and emits no favicon events. See
[capability limits](capabilities.md#favicons) before depending on this behavior.

A new document commit retires unfinished icon work, including a reload of the
same URL. A newer candidate supersedes unfinished work for the preceding
candidate. Destroying the view ends its stream, while detaching its surface
keeps the native view and icon delegate alive. Already queued events stay in
order; receiving an image does not prove its document is still current.

When the icon delegate is attached, each document commit queues
the `Started` load status, then a favicon event. Back and Forward restore
`Some(image)` from retained native history data. A missing, evicted or unobserved
value produces `None`, including a fresh document commit; your app should show
its fallback image. Native icon loading can subsequently supply a new `Some`.
Reloading an entry invalidates its previous document's retained image, including
shared same-document entries, before lookup. Decode failure supplies no replacement image. Servo's forwarded events retain
their existing engine behavior.

Each system view retains at most 8 MiB of unique encoded icon bytes and 128
native history-entry records, including the current entry. Known same-document
entries share retained bytes. Entries removed from native history are pruned
first, then least recently used records or data are evicted until both limits
hold. An image larger than the retention budget can be delivered initially but
cannot be restored from this store. Unknown native entries safely produce
`None`; URLs are not used to guess document identity. Detaching preserves this
state, and destroying the view releases it. The limits cover retained history
data; transient decoding allocations and queued events are separate.

## Evaluating JavaScript

`Runtime::evaluate_javascript(view, evaluation_id, script)` works for both
engines. Servo views use Servo's `WebView::evaluate_javascript`. System views
call WebKit's `evaluateJavaScript:completionHandler:` on the same WRY-owned
`WKWebView`, on the AppKit thread. Either way, the result arrives later as a
`HostEvent::JavaScriptEvaluationResult` inside a `ExplorerKitEvent`.

### What the script sees

- The script runs synchronously in the main document's page world. Its result
  arrives asynchronously.
- It does not wait for Promises, a destination URL, the page to settle, or
  future DOM content.
- Evaluation itself does not focus, show, attach, navigate, or recreate the
  view. Your script can still have side effects in the page.
- Frames follow normal page rules: same-origin frames are reachable, and there
  is no way to pick a frame or world, or to reach cross-origin content.

### When a view is ready

A system view becomes ready at its first observed main-document commit (WRY's
`PageLoadEvent::Started` on macOS). The page does not need to finish loading.

- Calling before that is accepted, and returns `WebViewNotReady`. The script
  is not saved for later.
- A hidden or detached view that stays alive stays ready.
- Your app must keep running the AppKit loop and calling ExplorerKit's update
  and drain functions. There is no background-activity or latency guarantee.

### Results

| Result | What you get |
| --- | --- |
| String, boolean, finite number, null, array, or object with string keys | `ok: true` and tagged JSON in `value_json`. A string becomes `{"type":"string","value":"text"}`. Array and object members are tagged too. |
| `nil` or `NSNull` from WebKit | `{"type":"null"}`. `undefined` is reported as null. |
| Empty content, such as `""` | A normal successful value, never an error. |
| JavaScript exception, including a syntax error | `EvaluationFailure`. WebKit does not separate compile errors from runtime errors the way Servo does. |
| Unsupported or cyclic value, non-string object key, non-finite number, nesting too deep, or encoding failure | `SerializationError` |
| A new page committed while the read was pending | `DocumentNotFound` |
| The web content process ended, or an unknown native failure | `InternalError` |
| The native view died while still logically alive | `InternalError` for pending reads; later calls return `WebViewNotReady` until a new commit |

A failure has `ok: false`, an `error_type`, and no `value_json`. Values are
whatever WebKit's bridge returns, so there is no exact support for
`undefined`, array holes, custom objects, or Servo DOM references. Return plain
JSON-compatible data when you need the same result on both engines.

If WebKit reports its "unsupported result" error (`WKErrorDomain` code 5)
because execution was interrupted, ExplorerKit reports `SerializationError`,
even though the script did not return a bad value. If the process then
terminates, only reads still pending get `InternalError`; results already
delivered are unchanged, and new calls return `WebViewNotReady` until the next
commit. The existing process-termination notification sends the single
`Crashed` event; evaluation errors never create another one. ExplorerKit never
reloads, recreates, or switches engines on its own.

### Evaluation IDs

- Match results by runtime, view handle, and your evaluation ID.
- IDs must not be blank, and must be unique among a view's outstanding
  requests. The same ID may be in use in different views at once.
- Unknown or destroyed handles, blank IDs, and duplicate pending IDs fail
  immediately, and produce no result event.
- A request stays outstanding until you receive or give up on its result.
  Use a fresh, increasing ID per view lifetime.
- Your own timeout does not stop the script. Stop waiting, ignore the late
  result, and don't reuse that ID while its result could still arrive.

### When the page changes

Every observed main-document commit (including a same-URL reload and a
committed history step) reports load start, then retires each still-pending
read with one `DocumentNotFound`. Starting a navigation, or a navigation that
fails before committing, is not such a boundary.

A result finished before the commit stays a snapshot from the old page and is
ordered before the load-start event, including after it moves into the
runtime's queue. Callbacks from retired requests are ignored. Single-page-app
changes without a commit keep snapshot-at-execution semantics. Two separate
evaluations are not atomic: if you need the URL, title, and content from the
same page, return them together from one script.

### Keeping results fresh in your app

ExplorerKit does not expose a document identity, so your app must guard against
stale results:

- Keep your own generation counter per view. Advance it when you process
  lifecycle events.
- Check the generation again when deferred work actually applies the result,
  not only when it arrives. Comparing URLs is not enough, because a same-URL
  reload is a new document.
- A result never proves its document is still current when you receive or
  apply it.

### Lifetime

While a view is alive, each accepted request gets at most one final result.
Results can arrive out of order. There is no hard deadline, and no final
result after the view is destroyed. Destroying a view ends its native lifetime
and discards both queues for the old handle. Cancel your own waiters; a new
view starts fresh with no carried-over requests. Results already delivered
cannot be taken back.

## Testing

See [Testing and validation](testing.md#system-web-view-on-macos).
