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
- **Commands and events.** Load, reload, stop, back, forward, and focus work. The
  view reports URL, load status, title, crash, and navigation-state events
  (whether it can go back or forward).
- **Input.** AppKit delivers input straight to the view. ExplorerKit rejects
  `HostInputEvent`s forwarded to a system view.
- **Defaults.** WebKit applies its own defaults for normal navigation and
  permissions. New windows are denied unless explicitly enabled as described
  below. Downloads are denied.
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

## Stop loading

`Runtime::stop_loading(view)` calls `WKWebView.stopLoading` through Wry's
published native handle. Success means the native call returned; observe
`Runtime::system_webview_is_loading(view)` on later owning-thread turns to
read the current native content-loading flag. A false sample does not identify
a navigation, cancellation cause or successful load, or prove that every page
request, script or timer stopped. The query does not pump the event loop.

```rust
runtime.stop_loading(view)?;
// After processing commands and events on a later owning-thread turn:
let loading = runtime.system_webview_is_loading(view)?;
```

For a streamed page, Stop retains the committed partial document, native view
and native history. A provisional or redirecting load can leave the previous
committed page. ExplorerKit does not change history or recreate the view.
Detached views with a retained native object work; an idle Stop is harmless.
Both calls reject a view whose native object has not been created. Stop is
never queued for later attachment and does not cancel a deferred initial URL.
Registered children use their own handles and remain independent of siblings.

Use the app's existing backend selection to discover support; see
[Stop loading support](capabilities.md#stop-loading). The app owns the Reload/Stop control and user intent. Read native state
after processing commands/events, and discard a sample after another command.
Old uncorrelated load-complete events must not override that state or settle a
new navigation waiter. A new load may not yet have entered native loading when
sampled: false is only the current flag. Attributed navigation outcomes remain
unavailable through these calls.

Stop does not synthesize completion, failure or crash events, or cancel pending
JavaScript evaluations. Actual commit, close and process-termination callbacks
retain their existing behavior. Ordinary load failure can also make loading
false; this is not an attributed cancellation-complete notification.

## Popups and new windows

Install a destination provider on `MacOsViewHostOptions` with
`with_new_webview_destination_handler`, then enable the source's
[per-view creation handler](../concepts/runtime.md#popups-and-new-windows).
Enabling a system handler without a provider returns an error.

The provider receives the same `NewWebViewRequest` after the source handler
returns `Create`. Return `AppKitNewWebViewDestination::new(surface, viewport,
parent)`, where `parent` is an `Rc<dyn HasWindowHandle>` retaining a live AppKit
view in a window. The viewport must have nonzero dimensions and a finite,
positive scale. ExplorerKit mounts its container there and reports
`PopupCreated`, then `SurfaceAttached`, using the new ordinary Runtime handle.
The app owns the slot, window, layout, and selection.

On SystemWebView, `wants_popup` comes from WebKit's private
`WKWindowFeatures._wantsPopup` computed preference, guarded at runtime with
`respondsToSelector:`. An unavailable selector yields `None`; a known false
stays `Some(false)`. This is distinct from WebKit's explicit `_popup` token.
Private API can change or disappear with an OS update; the availability guard
does not make it public API. This
mapping covers only this macOS backend and does not choose presentation.

Both callbacks run synchronously on AppKit's thread. The provider may mount a
slot using independently retained native parents and placement data. Neither
callback may reenter Runtime or GUI state, wait for rendering, or run a nested
event loop. Return an error to abandon a provisional destination; creation is
then denied and `PopupCreationFailed` reports the failure.

ExplorerKit builds with WebKit's original configuration and returns the native
child directly. WebKit performs its original navigation, including POST and
redirects, and preserves its data store, opener, named-target, messaging, and
origin rules. ExplorerKit does not replay the URL or choose a different store.
Use the child's ordinary handle for navigation, evaluation and surface controls.
A child copies its source's handler; later policy changes are per view.

The source may be detached when it requests a child, but the new destination
must already be attached. Detaching either view preserves its document and
callbacks. Closing the source or child leaves the other alive. A browser
`webViewDidClose` callback retires exactly that handle and emits `Closed` once;
the next Runtime update releases its native owner, including when no live
views remain. Keep pumping AppKit and Runtime during cleanup.

ExplorerKit installs its own UI delegate before the first navigation and on
each child. It handles creation and close, forwarding WRY's existing file-panel
and media-permission callbacks with their native arguments and completion
blocks. WRY continues to own the web view and navigation callbacks. Recheck
this forwarding list when updating WRY. Favicon loading uses its separate
delegate and keeps the same behavior below.

## Favicons

WebKit supplies standard favicon candidates and loads their image data in the
view's own browser context. ExplorerKit sends owned first-image straight RGBA8
pixels through `HostEvent::FaviconChanged`. Match the event's view handle to the
view whose icon you display. Touch icons are ignored; ExplorerKit adds no
downloader, animation playback, public candidate list, or app icon cache.

The adapter uses WebKit's private icon-loading delegate alongside WRY's own
navigation and UI delegates. If the private selector is unavailable, the view
continues working and emits no favicon events. See
[capability limits](capabilities.md#favicons) before depending on this behavior.

For each committed document, the first standard icon decision starts one
internal query in WebKit's public client content world for DOM order, type,
media and sizes. Icon loading waits for that query. For candidates matching the snapshot, media must match the captured
environment. A valid unsupported declared MIME type is declined; missing,
empty or invalid hints are tried. The decoder checks the actual data, so a supported hint cannot make malformed data usable.

Among matching candidates, prefer the highest usable short-edge bitmap
resolution, capped by the largest usable finite declared size unless `any` is
present. Missing or invalid sizes use the decoded dimensions. `any` uses the
actual rasterized dimensions, not an infinite preference. Equal preferences use
the last declaration in the collected DOM order, following
[HTML's icon selection rule](https://html.spec.whatwg.org/multipage/links.html#rel-icon).
The resolution score is ExplorerKit's choice for a generic pixel payload; the
app still chooses its display size. Identical declarations share the last
matching position; a better-resolution duplicate can improve the result.

A usable lower preference can arrive while a better one is pending. Failure
leaves that image alone, and a late lower preference cannot overwrite a better
success. If the query is unavailable or fails, successful current-document
images arrive in completion order. With a snapshot, unmatched native candidates
can supply an image until a matched candidate succeeds. These compatibility
paths preserve useful icons but do not promise DOM-order, media, type-hint or
size selection. The snapshot is one-shot; later DOM or theme changes do not
cause new discovery or selection.

A new document commit retires unfinished icon work, including a reload of the
same URL. Destroying the view ends its stream, while detaching its surface
keeps the native view and icon delegate alive. Already queued events stay in
order; receiving an image does not prove its document is still current.

SVG uses the registered public AppKit SVG reader when it can exclusively
identify that format. It rasterizes at the finite positive native size at 1x,
rounding fractional dimensions up, then normalizes to PNG for the existing
pixel decoder and history store. Native size can be inferred even without
authored dimensions. Other raster formats keep their existing decoder path.
Original encoded inputs and normalized PNGs are limited to 16 MiB, and output
to 4,194,304 pixels. AppKit parsing, drawing and encoding are synchronous;
these checks do not cap AppKit's internal allocations or execution time.

When the icon delegate is attached, each document commit queues
the `Started` load status, then a favicon event. Back and Forward restore
`Some(image)` from retained native history data. A missing, evicted or unobserved
value produces `None`, including a fresh document commit; your app should show
its fallback image. Native icon loading can subsequently supply a new `Some`.
Reloading an entry invalidates its previous document's retained image, including
shared same-document entries, before lookup. Decode failure supplies no replacement image. Servo's forwarded events retain
their existing engine behavior.

Each system view retains at most 8 MiB of unique encoded icon bytes and 128
image-history records, including the current entry. Known same-document
entries share retained bytes. Entries removed from native history are pruned
first, then least recently used records or data are evicted until both limits
hold. An image larger than the retention budget can be delivered initially but
cannot be restored from this store. SVG retention counts the normalized PNG,
which can exceed the budget even when its source is small. Unknown native
entries safely produce `None`; URLs are not used to guess document identity.
Detaching preserves this state, and destroying the view releases it. The limits cover retained history
image data; transient decoding allocations and queued events are separate.

Native URL, loading and back/forward availability notifications reconcile
history at the callback boundary, including before a navigation policy decision.
There is no history scan during Runtime event collection and no periodic SDK
update is needed to preserve silent same-document entries. Separately from the
image limits, each view keeps one retained native identity per item in the last
observed native history list. This O(n) membership metadata distinguishes new
aliases from old entries whose images were evicted; it has no additional entry
cap or image payload. A pointer slot is about eight bytes on 64-bit systems,
plus vector storage, and retaining an item can extend its native metadata
lifetime. This is not a total memory or callback-time bound. Removed items are
pruned at the next native boundary; normal commits preserve membership, detach
keeps it, and process invalidation or destruction releases it.

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
