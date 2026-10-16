# Surfaces: where pages are drawn

A **surface** is the place Servo draws a page: a native window, a child view
inside your layout, or an offscreen buffer you composite yourself. Your app
owns the surface. ExplorerKit borrows it, draws into it, and gives it back.

This page explains the surface types, how to attach and detach them, and the
rules for running several views and shutting down. It applies to native Rust
apps that use the `explorerkit` crate. The Android host has its own
implementation with the same attach and detach rules, but it does not use
these Rust types.

## The contract in one table

| Your app owns | ExplorerKit owns |
| --- | --- |
| Top-level windows, menus, tabs, and chrome | The Servo web view and its delegate |
| Layout: where the page appears and how big it is | Rendering into the surface you provide |
| The event loop, and when to call ExplorerKit | Turning Servo's requests into ExplorerKit events |
| Hit testing, and which input reaches the page | Request IDs and pending state for page prompts |
| The UI for dialogs, menus, and pickers | Checking that answers to prompts are valid |

ExplorerKit never creates top-level windows, menus, or tabs, and never runs your
event loop. A whole-window web view is just the case where your layout slot is
the entire window.

## Building blocks

These types come from the `explorerkit-host` crate and are re-exported by
`explorerkit::surface`:

| Type | What it is |
| --- | --- |
| `HostSurface` | Your name for a layout slot or native surface, such as `"main"`. |
| `SurfaceViewport` | Where the slot is, how big it is, and the display scale. |
| `SurfaceTarget` | What to draw into: a `NativeSurface` or an `OffscreenSurface`. |
| `SurfaceDelegate` | The trait you implement so ExplorerKit can ask for targets and hand you frames. |
| `SurfaceFrameInfo` | Viewport and mode details for a painted frame. |

`SurfaceDelegate` has four callbacks. Only the first is required:

- `render_target`: return the target for a surface when it is first attached.
- `update_render_target`: return the target again after it or the viewport
  changes. By default it calls `render_target`.
- `before_update`: runs before each round of Servo work.
- `present_frame`: called when Servo has a new frame. By default it presents
  the frame.

The facade's `SurfaceFrame` also exposes frame details and optional CPU
readback.

## Surface types

### Native surface

`NativeSurface` borrows a native display handle and window handle (from
[`raw-window-handle`](https://docs.rs/raw-window-handle)). Servo renders and
presents directly into that window or child view. Your app keeps the handles
valid while the surface is attached, and keeps owning the window, its
placement, input routing, and the event loop.

This is the path for whole-window `winit` apps and AppKit child views on
macOS.

On macOS, `explorerkit::surface::macos::AppKitChildSurface` creates and
updates a child `NSView` for one slot in your layout. You give it the parent
view, the slot's bounds, and the scale factor. The GPUI example uses it.

### Offscreen surface with CPU readback

`OffscreenSurface::new(parent, parent_size)` renders into a framebuffer that
belongs to a parent native context. Read the pixels with
`SurfaceFrame::read_rgba` and draw them into your layout. This is good for
proofs, screenshots, and compatibility. It copies pixels through the CPU, so
it is not a zero-copy path.

### Exportable GPU frames (macOS)

`OffscreenSurface::exportable()` gives each web view a small pool of GPU
surfaces. `SurfaceFrame::take_gpu_frame()` hands you a `GpuFrame`: an opaque
32BGRA `CVPixelBuffer` backed by an IOSurface, with no CPU copy. Its
`GpuFrameInfo` names the web view, size generation, pool slot, frame number,
and exact pixel size. The image is vertically flipped as in OpenGL, so a Metal
renderer flips it when sampling.

The pool has three slots:

- If you hold all three, ExplorerKit waits to paint rather than overwrite a
  frame you are still using.
- Each frame comes with a one-shot `GpuFrameCompletion` that you may send to
  another thread. Call it after your last GPU read of that frame.
- If you drop a completion without calling it, that slot is never reused. Its
  IOSurface stays allocated until the app exits. This trades a small leak for
  never reading freed memory.

For exportable targets, viewport size is in logical points, and ExplorerKit
allocates `size × scale` pixels. Native and CPU-readback targets still use
physical pixel sizes.

The exported frame types live under `explorerkit::surface::macos` on purpose.
Windows shared textures and Linux dma-buf would be separate platform types
that reuse the same offscreen target and completion rules.

## Attaching and detaching

- Create a web view before you attach a surface to it.
- A web view has at most one surface at a time.
- For an app-created view, the first attach creates the Servo view. An
  [adopted native child](runtime.md#popups-and-new-windows) already exists;
  attachment uses that same document and rendering context. Later attaches
  reuse the view, so the page, history, and controller state survive.
- Send the viewport's position, size, and scale together, before the next
  update.
- Detaching removes the render target but keeps the web view alive, so you can
  attach it again later.
- If your layout collapses to zero size, don't attach yet, or detach until the
  slot has a real size.

Adopted Servo children have their own exportable GPU context. Attach them to
`OffscreenSurface::exportable()`; other target kinds return an error. Each
frame names the child's handle. Moving the child to another slot means
detaching and reattaching the same view, without loading the page again.

On macOS, a [system web view](../reference/macos-system-webview.md) keeps a
ExplorerKit-owned container view while detached. Reattaching moves that
container, with the same browser view inside, under the new parent you
provide. Focus and blur requests made while detached are replayed after
reattach.

## The update loop

Your event loop drives ExplorerKit:

1. Call `Runtime::perform_updates(view)`, or `Runtime::perform_all_updates()`
   when you have several views.
2. ExplorerKit runs `before_update`, lets Servo do its work, paints if Servo
   asked to, and calls `present_frame`.
3. Call `Runtime::drain_events()` and update your UI from the events.

The waker you pass in `SurfaceHostOptions` is called when Servo needs another
update, so your loop can sleep until then.

Input works the same way. Your app hit-tests its own layout, then sends
pointer, wheel, keyboard, IME, and focus events that belong to the page as
`HostInputEvent`s, with coordinates relative to the slot. App shortcuts can
handle events first.

## Several views at once

One `Runtime<SurfaceHost<_>>` can own many independent web views:

- Create several `WebViewHandle`s and attach a different `HostSurface` to
  each. Your `SurfaceDelegate` picks the target by `HostSurface` name.
- Each view has its own Servo `WebView`, delegate state, events, and render
  target, all on one shared engine. You don't need a popup or an opener page
  to create a view.
- Call `perform_all_updates` from your loop. One Servo step can call any
  view's delegate, so all views must be serviced.
- `RuntimeError::WebView` names the view that failed; the other views' events
  are still available from `drain_events`. `RuntimeError::Host` means the
  whole host failed.

React Native and the private desktop C boundary still use one view per
adapter. This does not add a multi-view React Native API.

## Closing views and shutting down

Your app decides how long the engine lives. ExplorerKit does not watch your
windows and never shuts the engine down on its own, even when the last view
closes. You can keep the runtime alive with zero views and create new ones
later.

- `Runtime::destroy_webview(view)` detaches and closes one view, invalidates
  its handle, and drops its queued events. Other views keep working. If detach
  fails, the view stays registered so you can retry. An exportable view
  returns this retryable error while you still hold its GPU frames.
- A successful native `window.close()` retires that view immediately and
  reports `Closed`, even with an exported frame outstanding. The frame keeps
  its IOSurface alive independently; complete it after the last GPU read as
  usual. The closed document no longer receives commands or produces frames.
- Dropping the host normally releases its engine connection after it closes
  all views. A later host in the same process can reuse the engine.
- `Runtime::shutdown()` consumes the runtime, closes its views, and shuts the
  engine down for good. Servo cannot restart in the same process. Call it on
  the thread that owns the engine, before you destroy native parent windows or
  logging. It still finishes cleanup if one detach fails, and returns the first
  error. A runtime with no attached surfaces can also retire an engine left
  behind by an earlier, dropped host. It cannot shut down another live host's
  engine.

Always destroy or detach a view's renderer before you remove its native view
or close its parent window.

Not covered yet: native clipping, stacking, and hide/show of embedded views.

## Related

- [Runtime and views](runtime.md): the engine, threads, and profiles.
- [Desktop guide](../platforms/desktop.md): the `winit` and GPUI examples.
- [Testing and validation](../reference/testing.md#desktop): checks for these rules.
