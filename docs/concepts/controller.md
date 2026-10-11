# Commands, events, and page prompts

Apps talk to a web view in three ways:

- **Commands** go from your app to the page: load a URL, reload, go back, run
  JavaScript.
- **Events** go from the page to your app: the URL changed, the title changed,
  loading finished.
- **Page prompts** are questions the page asks, which need an answer: may this
  link open? What did the user type in this `prompt()`? Which menu item was
  picked?

ExplorerKit handles all three in Rust, in one place, so every platform behaves
the same way. Platform adapters carry messages in and out, but don't decide
what they mean. This holds for Servo views and for the iOS `WKWebView`
adapter. macOS system web views use WebKit's own defaults for page prompts;
see [macOS system web view](../reference/macos-system-webview.md).

## Who decides what

Rust owns the rules:

- which commands exist, and checking their fields;
- the identity of each pending prompt (its request ID) and which view it
  belongs to;
- that a prompt nobody answers ends with the engine's own default, never a
  made-up answer (see [Fallbacks](#fallbacks));
- checking that an answer is valid before applying it;
- what happens when a command targets a view that no longer exists.

Platform adapters own everything native: views, rendering, input, threads, and
showing UI. Your app owns how prompts look and what to answer.

React Native callbacks and native dialogs only supply an answer. They never
become the source of truth for a pending prompt. Adapters may guard against
completing the same native callback twice, but Rust decides when a prompt
exists and when it is over.

## The command envelope

Every command is one JSON object with a version, a command name, and its
fields:

```json
{"version":1,"command":"loadUrl","url":"https://servo.org"}
{"version":1,"command":"reload"}
{"version":1,"command":"goBack"}
{"version":1,"command":"evaluateJavaScript","evaluationId":"evaluation-1","script":"document.title"}
{"version":1,"command":"resolveNavigationRequest","navigationId":"navigation-1","allow":false}
{"version":1,"command":"resolveSimpleDialog","dialogId":"dialog-1","confirmed":true,"promptValue":"Servo"}
{"version":1,"command":"resolveContextMenu","contextMenuId":"context-menu-1","action":"copy-link"}
{"version":1,"command":"dismissContextMenu","contextMenuId":"context-menu-1"}
```

It is parsed in exactly one place: `ControllerCommand` in
`crates/explorerkit-embedder/src/controller_command.rs`. Android, desktop, and
the iOS controller all use that parser, so the JSON schema is never
duplicated.

### Commands

| Command | Fields | What Rust does |
| --- | --- | --- |
| `loadUrl` | `url` | Checks and normalizes the URL with `NavigationRequest::new`, then loads it |
| `reload` | none | Reloads the page |
| `goBack` | none | Goes back in history |
| `goForward` | none | Goes forward in history |
| `focus` | none | Focuses the view |
| `blur` | none | Removes focus |
| `evaluateJavaScript` | `evaluationId`, `script` | Runs the script and sends a matching result event later |

### Prompt answers

| Command | Fields | What Rust does |
| --- | --- | --- |
| `resolveNavigationRequest` | `navigationId`, `allow` | Answers a pending navigation request |
| `resolveSimpleDialog` | `dialogId`, `confirmed`, optional `promptValue` | Answers a pending `alert`, `confirm`, or `prompt` |
| `resolveContextMenu` | `contextMenuId`, `action` | Picks a context-menu action (Servo hosts only) |
| `dismissContextMenu` | `contextMenuId` | Closes a context menu (Servo hosts only) |

### Validation

- Always send `"version": 1`. The iOS controller rejects an envelope without
  it. The shared parser used by Android and desktop currently also accepts a
  missing version, but rejects any value other than `1`.
- `command` must be a name Rust knows.
- Request IDs (`navigationId`, `dialogId`, `contextMenuId`, `evaluationId`)
  must be non-empty strings.
- `loadUrl.url` must be a valid navigation target.
- A context-menu `action` must be a known `ContextMenuAction`.

> [!NOTE]
> Answers to select pickers, file pickers, and permission prompts don't use
> the envelope yet. The Android host has its own calls for them
> (`ExplorerViewBinding.resolveSelectElement`, `resolveFilePicker`,
> `resolvePermission`), and the Rust `Runtime` cannot answer them.

## How a prompt flows

On Servo-backed hosts (Android, native Rust, macOS React Native):

1. Servo asks a question through its `WebViewDelegate`.
2. Rust records a pending request with a new ID, tied to that view.
3. The adapter reports it as an event, for example `navigationRequested`.
4. Your app, or the adapter's native fallback UI, answers with the view and
   request ID.
5. Rust checks the view, the request, and the answer, then applies it.

On iOS, the same rules run in the portable Rust controller, but WebKit is the
engine:

1. Objective-C++ passes the WebKit callback to Rust as an observation.
2. Rust records the pending request and emits an event.
3. Your answer goes back through Rust, which validates it and returns an
   effect.
4. Objective-C++ applies the effect by calling WebKit's stored completion
   handler.

Objective-C++ keeps the native completion handlers, and runs the timers the
Rust controller asks for, because those are platform objects. Rust stays the
source of truth for request IDs and answers.

## Fallbacks

When nobody answers a prompt, the engine's own default applies. For example,
Servo allows the navigation, answers `alert` with OK, and cancels `confirm`
and `prompt`. ExplorerKit adds no timers and makes up no answers: it ends a
prompt with the engine's default when nobody handles it or the view goes
away. Risky requests are denied: permission prompts that are replaced or
dropped are denied, and popups are denied unless you opt in.

Some paths still add timers or answer on their own today:

| Path | Unanswered navigation | Unanswered dialog |
| --- | --- | --- |
| iOS (React Native) | Rust allows it after 5 seconds | Rust applies the default answer after 60 seconds |
| Android (React Native) | JavaScript allows it after 5 seconds. With no handler, the Kotlin adapter allows it at once. | No timeout. With no handler, the adapter shows a native dialog. |
| Android (Kotlin) and native Rust | Waits until your app answers | Waits until your app answers |

> [!WARNING]
> Known gap: the iOS timers, the JavaScript navigation timer on Android, and
> the Kotlin adapter's own navigation answer don't follow the rule above yet.
> A native Rust or Kotlin app with a handler must answer every prompt, or the
> page waits.

## Transports

Each platform carries the same envelope its own way:

| Platform | Transport |
| --- | --- |
| Android | Fabric command → Kotlin → JNI `nativeSendControllerCommandWithController(controllerHandle, commandJson)`, or the C function `servo_host_send_controller_command_with_token_ffi(token, command_json)` |
| React Native macOS | Fabric command → Objective-C++ → `explorerkit_desktop_private_dispatch_controller_command(host, token, bytes, length)` |
| iOS | Fabric command → Objective-C++ → `servokit_controller_dispatch(handle, bytes, length)` |
| Rust | `servo_host_send_controller_command_with_token(token, ControllerCommand)` or the JSON variant, or the typed `Runtime` methods |

Transports only check their own concerns, such as string encoding and byte
limits. The iOS boundary has exactly four functions: `servokit_controller_create`,
`servokit_controller_dispatch`, `servokit_controller_destroy`, and
`servokit_controller_result_free`. Its results carry effects and events back
to Objective-C++. The iOS binary contains this controller only, not Servo.

Older per-command Rust helpers, such as `servo_host_load_url_with_token`, now
route through the same `ControllerCommand` path.

On Android, items that a React Native app adds to a context menu reach the
native view through a separate `showContextMenu` view command. That command
only hands over UI. The menu's answer still goes through the envelope.

### Stability

Only the React Native props, callbacks, and ref methods are app-facing API,
and they are still experimental. The envelope, the JNI and C transports, and
the iOS controller's C functions are internal contracts between ExplorerKit's
own adapters. They are stable enough for those adapters, but they are not a
public SDK or ABI.

### Status codes

The Android token transport returns a `ServoStatus`:

| Condition | `ServoStatus` |
| --- | --- |
| Command accepted | `Ok` |
| Malformed controller token | `InvalidControllerHandle` |
| Token for a disposed host | `ExpiredControllerHandle` |
| Bad `loadUrl` URL | `InvalidUrl` |
| Bad JSON, unknown command, unknown menu action, or other envelope error | `BackendError` |

The private desktop C boundary returns its own
`ExplorerKitDesktopPrivateStatus`:

| Condition | Status |
| --- | --- |
| Accepted | `EXPLORERKIT_DESKTOP_PRIVATE_OK` |
| Null host pointer | `EXPLORERKIT_DESKTOP_PRIVATE_NULL_POINTER` |
| Unknown, disposed, or mismatched token | `EXPLORERKIT_DESKTOP_PRIVATE_STALE_TOKEN` |
| Null, empty, oversized, non-UTF-8, malformed, or unknown command | `EXPLORERKIT_DESKTOP_PRIVATE_INVALID_ARGUMENT` |
| Called from the wrong thread | `EXPLORERKIT_DESKTOP_PRIVATE_WRONG_THREAD` |
| Reentrant call while busy | `EXPLORERKIT_DESKTOP_PRIVATE_BUSY` |
| The runtime rejected an accepted command | `EXPLORERKIT_DESKTOP_PRIVATE_RUNTIME_ERROR` |
| A panic was caught at the boundary | `EXPLORERKIT_DESKTOP_PRIVATE_PANIC` |

The two sets of codes are not interchangeable.

## What stays out of the envelope

Commands are for browser control only. These have their own APIs and must not
be folded into the envelope:

- attaching, detaching, and resizing surfaces;
- native window ownership;
- touch, keyboard, and IME input;
- running the update loop.

## Events

Servo-backed hosts send all events through one structured event bridge
(`host_event_bridge.rs`), and adapters decode them. Add new events to that
bridge rather than creating new per-field paths. On native Rust,
`Runtime::drain_events()` returns them as `ExplorerKitEvent` values.

Common events: URL, title, favicon (native Rust only), status text, load
status, history, focus, cursor, fullscreen, crash, error, surface attach,
resize, and detach, plus one event per page prompt.

## JavaScript evaluation results

`evaluateJavaScript` results come back as an event with your `evaluationId`.
On Servo, Rust turns Servo's `JSValue` into tagged JSON, such as
`{"type":"string","value":"Example title"}` or `{"type":"null"}`, and maps
Servo's `JavaScriptEvaluationError` into these categories:
`DocumentNotFound`, `CompilationFailure`, `EvaluationFailure`,
`InternalError`, `WebViewNotReady`, and `SerializationError`. The ID only
matches the result to its request.

For native Rust Servo views, evaluation IDs are scoped to the addressed view.
An empty ID or one already pending on that view is rejected; the same ID can
be pending on siblings. A registered native child uses the same rules as an
app-created view.

When Servo reports `LoadStatus::Started`, still-pending evaluations from that
view's previous observed load finish with `DocumentNotFound`. A late native
callback cannot complete a reused ID or affect another view. This follows
observed load events: Servo does not report `Started` for every reload or
overlapping navigation, so submitting a navigation command alone does not
retire an evaluation. The SDK does not cancel native script execution or
promise which document a script submitted during navigation will run in.

Destroying a view discards its queued results; retire your own waiters when
you destroy it. Native closure reports `Closed`. Closing a source does not
cancel evaluations on its live children.

For the React Native promise API, see
[React Native](../platforms/react-native.md#evaluating-javascript). For macOS system
views, see [macOS system web view](../reference/macos-system-webview.md#evaluating-javascript).

## Checklist for new control features

- Is the target a Servo host or the iOS controller? Keep native engine objects
  and completion handlers in the platform adapter either way.
- Does Rust own the request ID and answer validation? Does a prompt nobody
  answers end with the engine's own default, with no timer or made-up answer?
- Does a command for a disposed view fail with an explicit error?
- Do names match Servo's delegate and embedder-control terms?
- Are surfaces, input, and the update loop still separate from commands?
