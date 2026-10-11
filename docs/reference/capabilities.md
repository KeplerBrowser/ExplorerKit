# What works where

This page lists every capability and whether it works on each platform. It is
the single source of truth for support claims. If another page disagrees, this
page wins, and the other page should be fixed.

- **Android** means the Servo-backed path: React Native or Kotlin on the
  shared Android host. The host reports prompts and checks answers; the native
  fallback UI for dialogs, menus, pickers, and permission prompts comes from
  the React Native adapter. Kotlin apps get the same events and build their
  own UI.
- **iOS** means the React Native `<ExplorerView>` on `WKWebView`.
- **Native Rust** means a Rust app using the `explorerkit` crate with Servo,
  tested on macOS.
- Views that a macOS Rust app runs on the system web view have their own
  list in [macOS system web view](macos-system-webview.md).

The React Native macOS prototype is left out: it uses the same Servo path as
native Rust, but its React Native wiring is incomplete and not supported.

## Summary

✅ works · 🟡 partial · ➖ not supported yet

| Capability | Android | iOS | Native Rust |
| --- | :---: | :---: | :---: |
| Render pages, navigate, attach and detach surfaces | ✅ | ✅ | ✅ |
| Commands: load, reload, back, forward, focus, blur | ✅ | ✅ | ✅ |
| URL, title, load, and history events | ✅ | ✅ | ✅ |
| Status text, cursor, fullscreen, crash, and error events | ✅ | ➖ | ✅ |
| Favicon events | ➖ | ➖ | ✅ |
| Evaluate JavaScript | ✅ | ✅ | ✅ |
| Navigation policy (allow or block a navigation) | ✅ | ✅ | ✅ |
| JavaScript dialogs: `alert`, `confirm`, `prompt` | ✅ | ✅ | ✅ |
| Context menus | ✅ | ➖ | 🟡 events and answers, no built-in UI |
| Select menus | ✅ | ➖ | 🟡 events only |
| File pickers | ✅ | ➖ | 🟡 events only |
| Date, time, and color pickers | ✅ | ➖ | 🟡 events; commit the value as IME input |
| Permission prompts | ✅ | ➖ | 🟡 events only |
| Keyboard input and IME | ✅ | ✅ (WebKit) | ✅ |
| Clipboard: cut, copy, paste, select all | ✅ | ✅ (WebKit) | 🟡 app-provided clipboard |
| Popups and new windows | 🟡 reported, never opened | ➖ | ✅ per-request native children on macOS Servo |
| Several views under one runtime | ➖ | ➖ | ✅ |
| Offscreen rendering with GPU frame export | ➖ | ➖ | ✅ macOS |
| System WebKit per view | ➖ | n/a | ✅ macOS |
| Persistent profile directory | ➖ | ➖ | ✅ |

"Events only" means a native Rust app is told about the prompt, but the
`Runtime` has no method to answer it yet.

## Details by capability

### Navigation policy

The page asks before following a link or loading a frame (Servo's
`request_navigation`, or `WKNavigationDelegate` on iOS).

- **Android:** Rust records the request and checks the answer. The React
  Native adapter answers for you: it allows the navigation if you have no
  `onShouldStartLoadWithRequest`, if your callback fails, or if it takes
  longer than 5 seconds. This timeout lives in the adapter's JavaScript today.
  Kotlin apps must answer each request themselves.
- **iOS:** the Rust controller records the request, checks the answer, and
  allows the navigation after 5 seconds without one. Only `{ url }` is
  exposed.
- **Native Rust:** answer each `NavigationRequested` event with
  `resolve_navigation_request`. Until you do, the navigation waits.
- Test page: `smoke/policy.html`.

### JavaScript dialogs

- **Android:** in React Native without a handler, the adapter shows native
  dialogs. With `onJavaScriptDialog`, your UI answers. If the handler throws,
  the dialog is dismissed. There is no timeout if the handler never answers.
  Kotlin apps answer with `resolveSimpleDialog`.
- **iOS:** with `onJavaScriptDialog`, your UI answers. Without it, alerts
  complete and `confirm` and `prompt` are cancelled. The Rust controller
  applies the same defaults after 60 seconds without an answer.
- **Native Rust:** answer with `resolve_simple_dialog`.
- Test page: `controls/dialogs.html`.

### Context menus

- **Android:** Rust tracks the menu, validates actions, and applies picks and
  dismissals. In React Native, long-press shows a native menu with Servo's
  actions; you can add items with `onBeforeShowContextMenu` and observe picks
  with `onContextMenuItemSelected`. Kotlin apps show their own menu.
- **iOS:** not supported. The test page may load, but ExplorerKit does not
  handle the menu.
- **Native Rust:** handle `ContextMenuRequested` and answer with
  `resolve_context_menu` or `dismiss_context_menu`.
- Test page: `controls/context-menu-demo.html`.

### Select menus, file pickers, and other pickers

- **Android:** Rust tracks each request and checks answers. The React Native
  adapter shows native UI for `<select>`, the system document picker for file
  inputs, and native pickers for color, date, time, `datetime-local`, month,
  and week inputs; Kotlin apps build their own. Cancelled or unavailable
  pickers are dismissed safely, and unsupported input types fail without
  crashing. There are no React Native hooks for these yet.
- **iOS:** not supported by ExplorerKit. WebKit may show its own UI.
- **Native Rust:** you receive `SelectElementRequested`, `FilePickerRequested`,
  and `InputMethodRequested` events. You can commit a picker value as
  `HostInputEvent::ImeCommit`, but the `Runtime` cannot answer select or file
  requests yet.
- Test pages: `controls/select-elements.html`, `controls/file-input.html`,
  `controls/pickers.html`.

### Permissions

- **Android:** the React Native adapter shows a native prompt; Kotlin apps
  show their own. Rust tracks the request; a replaced or dropped request is
  denied, and answers are checked. There is no React Native hook yet.
- **iOS:** not supported.
- **Native Rust:** you receive `PermissionRequested`, but cannot answer it
  through the `Runtime` yet.
- Test page: `controls/permissions.html`.

### Keyboard, IME, and focus

- **Android:** Android's soft keyboard and `InputMethodManager`. The page
  resizes when the keyboard opens. Focus and blur are commands, and focus
  changes are events. There is no custom IME API.
- **iOS:** WebKit and UIKit handle the keyboard. Focus and blur map to the
  web view's first responder.
- **Native Rust:** send keyboard and IME input as `HostInputEvent`s.
- Test pages: `smoke/form.html`, `controls/ime-form.html`.

### Clipboard

- **Android:** Android's real `ClipboardManager` instead of Servo's in-process
  fallback. A paste while the app is in the background gets empty text,
  because Android 10 and newer block background clipboard reads.
- **iOS:** WebKit's native editing behavior. ExplorerKit adds nothing.
- **Native Rust:** you provide a `SurfaceClipboard`, or use `MemoryClipboard`.

### Fullscreen, cursor, crash, and errors

- **Android:** fullscreen changes, cursor changes, crashes, and errors are
  events (`onFullscreenChanged`, `onCursorChanged`, `onCrashed`, `onError`).
  Hiding system UI for fullscreen is up to your app.
- **iOS:** only URL, title, load status, history, and focus events. Crash,
  error, cursor, and fullscreen events are not supported.
- Test page: `smoke/error.html` checks load errors. No test page triggers a
  crash.

### Popups and new windows

Servo asks through `request_create_new` when a page calls `window.open` or
opens a `target="_blank"` link.

- **Android:** always denied. `onCreateNewWebViewRequested` reports
  `parentWebViewId`, `parentUrl`, and `policy`. `targetUrl` and
  `windowFeatures` are always null for now, because Servo 0.6 doesn't expose
  them.
- **iOS:** not reported.
- **Native Rust on macOS Servo:** denied by default. A per-view
  `set_new_webview_handler` can accept each request. The original native child
  becomes an ordinary Runtime handle with independent controls, events,
  lifetime, and exportable GPU frames. It can execute before attachment. See
  [Runtime and views](../concepts/runtime.md#popups-and-new-windows).
- **macOS system web view:** child creation remains unsupported and handler
  installation returns an error. The published Wry dependency lacks the
  required ownership and close hooks. Windows, Linux, Android, iOS, and React
  Native child presentation are not validated by the macOS Servo support.

### JavaScript evaluation

- **Android:** Servo's `WebView::evaluate_javascript`, resolved as tagged
  JSON.
- **iOS:** `WKWebView.evaluateJavaScript`, with the same tagged JSON and
  WebKit's error categories.
- **Native Rust:** `Runtime::evaluate_javascript`, for Servo views and macOS
  system views.

See [React Native](../platforms/react-native.md#evaluating-javascript) and
[macOS system web view](macos-system-webview.md#evaluating-javascript).

### Packaging

The packed `react-native-explorerkit` archive contains the Android AAR
(`arm64-v8a` and `x86_64`) and the Servo-free iOS controller XCFramework. App
builds run no Cargo and download nothing. It is not published to npm yet, and
there is no CI or release process yet.

## Not supported yet

| Capability | Notes |
| --- | --- |
| Messaging between web pages and native code | Needs an explicit bridge and a security model first |
| Preload and user scripts | Needs per-view script ordering and a security policy |
| `injectJavaScript` from `react-native-webview` | Different from on-demand evaluation |
| Custom URL schemes and app asset loading | Useful for offline content |
| Auth hooks and resource interception | Important for enterprise and proxy setups |
| Accessibility | Needed for production use |
| Bluetooth device selection, media session, screenshots | Lower priority |
| Windows shared textures and Linux dma-buf export | Separate platform work |

## About the terms

Servo calls file pickers, select menus, context menus, and dialogs **embedder
controls** (see `components/shared/embedder/embedder_controls.rs` upstream).
Other capabilities, such as navigation policy, JavaScript evaluation, popups,
and fullscreen, come from Servo's `WebViewDelegate`. This page lists both,
because apps care about the capability, not where Servo defines it.

A test page that loads proves only that it loads. It is not a support claim.
These claims describe Servo `0.6.0`. See [Dependencies](dependencies.md).
